#!/usr/bin/env python3
"""Long-lived account host: held gateway connections behind a private socket.

Every account action used to spawn a Python process and pay three HTTPS
token/VM lookups plus a Noise XX handshake before its first byte. Measured on
this machine: 3.0s for a fresh connection against 0.06s for a call on one that
is already open. This host opens the connections once and serves the same
allowlisted operations over a private Unix socket, so the bridge keeps its op
semantics (and its refusals) while the desktop stops paying a handshake per
action. The one-shot bridge stays available and is still the fallback.

The host deliberately reuses `muse_account_bridge.Adapter`: one place decides
what an operation may do, so the socket cannot become a second, weaker door.
"""
import argparse
import importlib.util
import json
import os
import signal
import socket
import struct
import sys
import threading
import time
from contextlib import ExitStack

MAX_LINE = 1024 * 1024
MAX_IDLE = 300.0
RUNTIME_DIR_NAME = "unofficial-muse-for-linux"
WAIT_OPS = frozenset({"watch"})
RETRY_CODES = frozenset({"bridge_failure", "timeout", "auth_required"})
REFUSED = {"ok": False, "error": {"code": "invalid_request", "uncertain": False}}


def fnv1a64(data):
    """FNV-1a, so Rust and Python derive the same socket name."""
    value = 0xCBF29CE484222325
    for byte in data:
        value ^= byte
        value = (value * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return value


def runtime_root(environ=None):
    environ = os.environ if environ is None else environ
    base = environ.get("XDG_RUNTIME_DIR")
    if base:
        return os.path.join(base, RUNTIME_DIR_NAME)
    cache = environ.get("XDG_CACHE_HOME") or os.path.join(environ.get("HOME", ""), ".cache")
    return os.path.join(cache, RUNTIME_DIR_NAME)


def socket_path(config_dir, environ=None):
    name = "account-{:016x}.sock".format(fnv1a64(os.fsencode(str(config_dir))))
    return os.path.join(runtime_root(environ), name)


def bridge_module():
    here = os.path.dirname(os.path.abspath(__file__))
    spec = importlib.util.spec_from_file_location(
        "_muse_account_bridge_host", os.path.join(here, "muse_account_bridge.py"))
    if spec is None or spec.loader is None:
        raise RuntimeError("bridge unavailable")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def cookie_digest(path):
    if not path:
        return None
    try:
        with open(path, "rb") as handle:
            return fnv1a64(handle.read())
    except OSError:
        return None


def peer_uid(connection):
    if not hasattr(socket, "SO_PEERCRED"):
        return None
    raw = connection.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED,
                               struct.calcsize("3i"))
    _, uid, _ = struct.unpack("3i", raw)
    return uid


def authorized(connection):
    """Only this user may drive the account, even inside a shared runtime dir."""
    uid = peer_uid(connection)
    return uid is None or uid == os.getuid()


class Connection:
    """One held gateway connection; exactly one request uses it at a time.

    The upstream transport keeps one frame consumer per connection: two threads
    reading at once split frames and corrupt the stateful Noise decrypt
    (docs/PROTOCOL.md). The lock is that rule, not an optimisation.
    """

    def __init__(self, bridge, config_dir):
        self.bridge = bridge
        self.config_dir = config_dir
        self.lock = threading.Lock()
        self.stack = None
        self.client = None
        self.digest = None
        self.opened = 0

    def close(self):
        stack, self.stack = self.stack, None
        self.client = None
        self.digest = None
        if stack is not None:
            try:
                stack.close()
            except Exception:
                pass

    def dispatch(self, request):
        with self.lock:
            try:
                with self.bridge.secure_config(self.config_dir) as config:
                    digest = cookie_digest(config.get("cookies_file"))
                    if self.client is None or digest != self.digest:
                        self.close()
                        self._open(config, digest)
                    return self.bridge.Adapter(self.client, config).dispatch(request)
            except Exception as exc:
                error = self.bridge.classify_exception(exc)
                # A transport or session failure poisons the held connection;
                # a refused request (invalid, changed account) does not.
                if error.code in RETRY_CODES:
                    self.close()
                raise error from None

    def _open(self, config, digest):
        client = self.bridge.community_client(config)
        stack = ExitStack()
        # Held open for the life of the connection: this is the whole point.
        stack.enter_context(client.gateway_scope(config))
        self.stack, self.client, self.digest = stack, client, digest
        self.opened += 1


class Host:
    def __init__(self, config_dir, bridge=None):
        self.bridge = bridge or bridge_module()
        self.config_dir = config_dir
        self.control = Connection(self.bridge, config_dir)
        self.waiting = Connection(self.bridge, config_dir)
        self.touched = time.monotonic()

    def touch(self):
        self.touched = time.monotonic()

    def idle_for(self):
        return time.monotonic() - self.touched

    def connection(self, op):
        # A long wait must not queue a send behind it.
        return self.waiting if op in WAIT_OPS else self.control

    def handle_line(self, line):
        self.touch()
        try:
            if len(line) > MAX_LINE:
                raise self.bridge.BridgeFailure("invalid_request")
            try:
                request = json.loads(line, object_pairs_hook=self.bridge.parse_pairs)
            except (ValueError, UnicodeDecodeError):
                raise self.bridge.BridgeFailure("invalid_request") from None
            if not isinstance(request, dict):
                raise self.bridge.BridgeFailure("invalid_request")
            # The host serves exactly one session directory; a caller may name
            # it (the one-shot bridge requires one) but can never redirect it.
            request["config_dir"] = self.config_dir
            self.bridge.validate_request(request)
            data = self.connection(request["op"]).dispatch(request)
            return {"ok": True, "data": data}
        except Exception as exc:
            return self.bridge.error_response(self.bridge.classify_exception(exc))

    def close(self):
        self.control.close()
        self.waiting.close()


def encode(reply):
    return json.dumps(reply, ensure_ascii=True, allow_nan=False).encode("utf-8") + b"\n"


def serve_client(host, connection):
    try:
        if not authorized(connection):
            return
        reader = connection.makefile("rb")
        writer = connection.makefile("wb")
        while True:
            line = reader.readline(MAX_LINE + 2)
            if not line:
                return
            if not line.endswith(b"\n"):
                # Bounded read: an unterminated request is refused, not buffered.
                writer.write(encode(REFUSED))
                writer.flush()
                return
            writer.write(encode(host.handle_line(line)))
            writer.flush()
    except OSError:
        return
    finally:
        try:
            connection.close()
        except OSError:
            pass


def serve(host, path, stop, idle_seconds=None, accept_timeout=0.5):
    directory = os.path.dirname(path)
    os.makedirs(directory, mode=0o700, exist_ok=True)
    os.chmod(directory, 0o700)
    listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    try:
        listener.bind(path)
    except OSError:
        listener.close()
        return
    try:
        os.chmod(path, 0o600)
        listener.listen(8)
        listener.settimeout(accept_timeout)
        while not stop.is_set():
            if idle_seconds is not None and host.idle_for() >= idle_seconds:
                break
            try:
                connection, _ = listener.accept()
            except socket.timeout:
                continue
            except OSError:
                break
            threading.Thread(target=serve_client, args=(host, connection),
                             daemon=True).start()
    finally:
        listener.close()
        try:
            os.unlink(path)
        except OSError:
            pass


def already_served(path):
    """True when a live host answers; otherwise the path is stale and removed."""
    if not os.path.exists(path):
        return False
    probe = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    try:
        probe.connect(path)
        return True
    except OSError:
        try:
            os.unlink(path)
        except OSError:
            pass
        return False
    finally:
        probe.close()


def main(argv=None):
    parser = argparse.ArgumentParser(description="Serve Muse account reads over a private socket.")
    parser.add_argument("--config-dir", required=True)
    parser.add_argument("--idle-seconds", type=float, default=MAX_IDLE)
    args = parser.parse_args(argv)
    # Dependency output never reaches a protocol path, and there is no log to
    # leak credentials into: the socket envelope is the only reporting channel.
    quiet = os.open(os.devnull, os.O_WRONLY)
    try:
        for descriptor in (1, 2):
            os.dup2(quiet, descriptor)
    finally:
        os.close(quiet)
    config_dir = os.path.abspath(os.path.expanduser(args.config_dir))
    path = socket_path(config_dir)
    if already_served(path):
        return 0
    stop = threading.Event()

    def cancel(_signum, _frame):
        stop.set()

    signal.signal(signal.SIGTERM, cancel)
    signal.signal(signal.SIGINT, cancel)
    parent = os.getppid()

    def watch_parent():
        while not stop.is_set():
            if os.getppid() != parent:
                stop.set()
                return
            stop.wait(1.0)

    def watch_stdin():
        try:
            while sys.stdin.buffer.read(4096):
                pass
        except Exception:
            pass
        stop.set()

    threading.Thread(target=watch_parent, daemon=True).start()
    threading.Thread(target=watch_stdin, daemon=True).start()
    host = Host(config_dir)
    try:
        serve(host, path, stop, idle_seconds=args.idle_seconds or None)
    finally:
        host.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
