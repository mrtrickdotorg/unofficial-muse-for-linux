#!/usr/bin/env python3
"""Typed, credential-contained adapter for an external community muse-mcp install.

One stdin request/one stdout response. Never start an MCP process or expose a raw
route. Imports of the community library are deferred until explicit execution.
"""


import json
import fcntl
import os
import re
from contextlib import contextmanager
from pathlib import Path
import secrets
import stat


MAX_REQUEST = 1024 * 1024
MAX_RESPONSE = 4 * 1024 * 1024


# Requests whose upstream calls are cheap only on a reused connection. One
# handshake costs 1.5-2.2s and a call on an open connection ~0.1s, so these are
# the operations where the reuse pays for itself.
SHARED_GATEWAY_OPS = frozenset({"send", "watch", "tasks"})


ERROR_CODES = frozenset({"auth_required", "account_changed", "vm_unavailable", "timeout", "invalid_request",
    "insecure_config", "bridge_unavailable", "bridge_failure", "protocol_error", "response_too_large"})


class BridgeFailure(Exception):
    def __init__(self, code, uncertain=False, detail=None):
        self.code = code if code in ERROR_CODES else "bridge_failure"
        self.uncertain = bool(uncertain)
        # Self-authored context only, bounded: never child output or an exception
        # message, so a credential can never reach the app's status line.
        self.detail = str(detail)[:300] if detail else None
        super().__init__(self.code)


def venv_built_for(prefix):
    """The Python version a venv was created with, from its pyvenv.cfg."""
    try:
        text = (Path(prefix) / "pyvenv.cfg").read_text(encoding="utf-8", errors="replace")
    except OSError:
        return None
    for line in text.splitlines():
        key, separator, value = line.partition("=")
        if not separator or key.strip() not in ("version", "version_info"):
            continue
        parts = value.strip().split(" ")[0].split(".")
        if len(parts) >= 2 and parts[0].isdigit() and parts[1].isdigit():
            return "{}.{}".format(parts[0], parts[1])
    return None


def import_detail(exc, paths=None):
    """Interpreter facts for a failed community import, bounded and text-free.

    `paths` must be the search path from before any fallback was attempted, so
    the report describes the state the launcher produced.
    """
    import sys
    entries = sys.path if paths is None else paths
    site_packages = any("site-packages" in entry for entry in entries
                        if isinstance(entry, str))
    missing = getattr(exc, "name", None)
    running = "{}.{}".format(*sys.version_info[:2])
    version = "python" + running
    roots = [Path(sys.prefix)] if sys.prefix else []
    if len(Path(sys.executable).parents) > 2:
        roots.append(Path(sys.executable).parents[2])
    installed = bool(missing) and any(
        (site_dir / missing).exists()
        for root in roots
        for site_dir in [root / "lib" / version / "site-packages",
                         root / "lib64" / version / "site-packages"]
        + sorted(root.glob("lib*/python*/site-packages")))
    # The app can run in another environment than the one that built the venv
    # (a container has its own Python). pyvenv.cfg records the building
    # interpreter, so only that is evidence of a split: a venv may hold several
    # version directories and still be perfectly usable.
    built = venv_built_for(sys.prefix)
    built_for = ("; venv_built_for={}".format(built)
                 if built and built != running else "")
    if built_for:
        # A venv built by another interpreter cannot load its compiled wheels
        # (cffi's backend is built per version), so point at the cure instead of
        # reporting an installation that is in fact complete.
        origin = (built_for + "; rebuild the venv with Python {} by running "
                  "setup-account-bridge.sh in this environment").format(running)
    else:
        origin = "; prefix={}".format(sys.prefix or "?")
    return ("{}; missing={}; installed_in_venv={}; running={}{}"
            "; site_packages_on_sys_path={}").format(
        type(exc).__name__, missing or "-", installed, running,
        origin, site_packages)[:300]


def checked(value, expected):
    if isinstance(value, dict) and value.get("ok") is False:
        code = str(value.get("error", {}).get("code", "")).lower()
        code = {"invalid_argument": "invalid_request", "invalid_thread": "invalid_request",
                "gateway_error": "bridge_failure"}.get(code, code)
        raise BridgeFailure(code)
    if not isinstance(value, expected):
        raise BridgeFailure("protocol_error")
    return value


def classify_exception(exc, uncertain=False):
    if isinstance(exc, BridgeFailure):
        exc.uncertain = exc.uncertain or uncertain
        return exc
    if isinstance(exc, TimeoutError):
        return BridgeFailure("timeout", uncertain)
    if type(exc).__name__ == "AuthError":
        return BridgeFailure("auth_required", uncertain)
    if type(exc).__name__ == "VMUnavailableError":
        return BridgeFailure("vm_unavailable", uncertain)
    return BridgeFailure("bridge_failure", uncertain)


def optional_text(value):
    return str(value) if isinstance(value, (str, int, float)) and not isinstance(value, bool) else None


def required_text(value):
    return optional_text(value) or ""


def content_text(value):
    """Defense in depth for content from the two direct read-only task routes."""
    text = optional_text(value)
    if text is None:
        return None
    text = re.sub(r'(?i)(["\']?(?:set-cookie|cookie)["\']?\s*[:=]\s*)[^\r\n]+',
                  r'\1[REDACTED]', text)
    text = re.sub(r'(?i)(Bearer\s+)[^\s,;"\']+', r'\1[REDACTED]', text)
    text = re.sub(r'(?i)((?:access[_-]?token|auth[_-]?token|hatch[_-]?token|token|hatch_sess|datr|api[_-]?key|password|secret|credential)["\']?\s*[:=]\s*["\']?)[^\s"\'&,;]+',
                  r'\1[REDACTED]', text)
    return text


def object_rows(rows):
    rows = checked(rows, list)
    if any(not isinstance(row, dict) for row in rows):
        raise BridgeFailure("protocol_error")
    return rows


def history_rows(rows):
    rows = object_rows(rows)
    latest = {}
    for row in rows:
        message_id = required_text(row.get("message_id"))
        if not message_id:
            continue
        seq = row.get("seq")
        seq = seq if isinstance(seq, int) and not isinstance(seq, bool) and seq >= 0 else 0
        raw = row.get("raw") if isinstance(row.get("raw"), dict) else {}
        payload = raw.get("payload") if isinstance(raw.get("payload"), dict) else {}
        message = {
            "message_id": message_id, "seq": seq,
            "role": required_text(row.get("role")), "text": required_text(row.get("text")),
            "timestamp": optional_text(row.get("timestamp")),
            "status": optional_text(row.get("status") or payload.get("status") or raw.get("status")),
        }
        if message_id not in latest or seq >= latest[message_id]["seq"]:
            latest[message_id] = message
    return sorted(latest.values(), key=lambda row: (row["seq"], row["message_id"]))


def secure_directory(path):
    """Open each directory component with O_NOFOLLOW, pinning the final fd."""
    path = Path(path)
    if not path.is_absolute() or ".." in path.parts:
        raise BridgeFailure("invalid_request")
    fd = os.open("/", os.O_RDONLY | os.O_DIRECTORY)
    try:
        for part in path.parts[1:]:
            next_fd = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=fd)
            os.close(fd)
            fd = next_fd
            info = os.fstat(fd)
            if info.st_uid not in (0, os.getuid()) or info.st_mode & 0o022:
                raise BridgeFailure("insecure_config")
        info = os.fstat(fd)
        if info.st_uid != os.getuid() or info.st_mode & 0o077:
            raise BridgeFailure("insecure_config")
        result, fd = fd, None
        return result
    finally:
        if fd is not None:
            os.close(fd)


def secure_file(directory_fd, name, private):
    fd = os.open(name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=directory_fd)
    try:
        info = os.fstat(fd)
        if (not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid()
                or info.st_nlink != 1 or info.st_mode & (0o077 if private else 0o022)):
            raise BridgeFailure("insecure_config")
        if info.st_size > MAX_REQUEST:
            raise BridgeFailure("insecure_config")
        result, fd = fd, None
        return result
    finally:
        if fd is not None:
            os.close(fd)


def compatible_cookie_descriptor(source_fd):
    """Adapt HttpOnly Netscape rows in memory, never modifying the user's jar.

    The installed loader skips all # comments, including Netscape's HttpOnly
    prefix. Only that pseudo-comment prefix is removed. Credentials stay inside
    this child; a sealed private memfd keeps subsequent upstream reads pinned.
    """
    data = os.pread(source_fd, MAX_REQUEST + 1, 0)
    if len(data) > MAX_REQUEST:
        raise BridgeFailure("insecure_config")
    data = b"".join(line[len(b"#HttpOnly_"):] if line.startswith(b"#HttpOnly_") else line
                    for line in data.splitlines(keepends=True))
    fd = os.memfd_create("muse-account-session", os.MFD_CLOEXEC | os.MFD_ALLOW_SEALING)
    try:
        os.fchmod(fd, 0o600)
        with os.fdopen(os.dup(fd), "wb") as output:
            output.write(data)
        os.lseek(fd, 0, os.SEEK_SET)
        # Linux UAPI fcntl.h values; standalone Python builds may omit the
        # symbolic constants even when the running kernel supports memfd seals.
        fcntl.fcntl(fd, getattr(fcntl, "F_ADD_SEALS", 1024 + 9),
                    getattr(fcntl, "F_SEAL_WRITE", 0x0008)
                    | getattr(fcntl, "F_SEAL_GROW", 0x0004)
                    | getattr(fcntl, "F_SEAL_SHRINK", 0x0002)
                    | getattr(fcntl, "F_SEAL_SEAL", 0x0001))
        result, fd = fd, None
        return result
    finally:
        if fd is not None:
            os.close(fd)


@contextmanager
def secure_config(config_dir):
    """Read nonsecret settings and pin credentials within this child only.

    Do not call upstream load_config: it writes defaults and trusts environment.
    The HttpOnly compatibility copy is sealed memory, never a persistent file.
    """
    directory_fd = cookie_dir_fd = cookie_fd = source_fd = None
    try:
        config_dir = Path(config_dir)
        directory_fd = secure_directory(config_dir)
        settings = {}
        try:
            settings_fd = secure_file(directory_fd, "config.json", private=False)
        except FileNotFoundError:
            settings_fd = None
        if settings_fd is not None:
            with os.fdopen(settings_fd, encoding="utf-8") as file:
                settings = json.loads(file.read(MAX_REQUEST + 1))
            if not isinstance(settings, dict):
                raise BridgeFailure("invalid_request")
        cookie_path = settings.get("cookies_file", str(config_dir / "cookies.txt"))
        if not isinstance(cookie_path, str) or not cookie_path:
            raise BridgeFailure("invalid_request")
        cookie_path = Path(cookie_path)
        if not cookie_path.is_absolute():
            cookie_path = config_dir / cookie_path
        cookie_dir_fd = secure_directory(cookie_path.parent)
        source_fd = secure_file(cookie_dir_fd, cookie_path.name, private=True)
        cookie_fd = compatible_cookie_descriptor(source_fd)
        config = {"cookies_file": f"/proc/self/fd/{cookie_fd}",
                  "node_id": required_text(settings.get("node_id")) or secrets.token_hex(8)}
        if settings.get("vm_id"):
            config["vm_id"] = required_text(settings["vm_id"])
        yield config
    except FileNotFoundError:
        raise BridgeFailure("auth_required") from None
    except (OSError, UnicodeError):
        raise BridgeFailure("insecure_config") from None
    except (ValueError, TypeError):
        raise BridgeFailure("invalid_request") from None
    finally:
        for fd in (cookie_fd, source_fd, cookie_dir_fd, directory_fd):
            if fd is not None:
                os.close(fd)


def validate_request(request):
    if not isinstance(request, dict):
        raise BridgeFailure("invalid_request")
    op = request.get("op")
    allowed = {"connect": set(), "identity": set(), "history": {"thread", "limit"},
               "watch": {"thread", "limit", "timeout", "after"},
               "tasks": set(),
               "send": {"thread", "message", "device_id", "expect_vm_id",
                        "expect_identity_name"}}
    if not isinstance(op, str) or op not in allowed:
        raise BridgeFailure("invalid_request")
    if set(request) - ({"op", "config_dir"} | allowed[op]):
        raise BridgeFailure("invalid_request")
    directory = request.get("config_dir")
    if not isinstance(directory, str) or not directory.startswith("/") or "\x00" in directory:
        raise BridgeFailure("invalid_request")
    thread = request.get("thread")
    if thread is not None and (not isinstance(thread, str) or not thread.strip()
                               or len(thread) > 4096 or "\x00" in thread):
        raise BridgeFailure("invalid_request")
    if op in ("history", "watch"):
        limit = request.get("limit")
        if not isinstance(limit, int) or isinstance(limit, bool) or not 1 <= limit <= 200:
            raise BridgeFailure("invalid_request")
    if op == "watch":
        timeout = request.get("timeout")
        if not isinstance(timeout, int) or isinstance(timeout, bool) or not 1 <= timeout <= 60000:
            raise BridgeFailure("invalid_request")
        if "after" in request:
            after = request["after"]
            if not isinstance(after, str) or not 1 <= len(after) <= 256:
                raise BridgeFailure("invalid_request")
    if "expect_vm_id" in request or "expect_identity_name" in request:
        for key in ("expect_vm_id", "expect_identity_name"):
            value = request.get(key)
            if not isinstance(value, str) or not value or len(value) > 256 or "\x00" in value:
                raise BridgeFailure("invalid_request")
    if "device_id" in request:
        device = request["device_id"]
        if not isinstance(device, str) or re.fullmatch(r"[A-Za-z0-9_-]{1,128}", device) is None:
            raise BridgeFailure("invalid_request")
    if op == "send":
        message = request.get("message")
        if not isinstance(message, str) or not message.strip() or len(message.encode("utf-8")) > 512 * 1024:
            raise BridgeFailure("invalid_request")


def provides_checkout(site_dir):
    """True when this site-packages exposes the community checkout.

    The installer links the checkout with a `.pth` entry instead of installing
    a `muse_mcp` package, so a directory holding no `muse_mcp` of its own can
    still be exactly the one that provides it.
    """
    try:
        if (site_dir / "muse_mcp").exists():
            return True
        for entry in sorted(site_dir.glob("*.pth")):
            try:
                lines = entry.read_text(encoding="utf-8", errors="replace").splitlines()
            except OSError:
                continue
            for line in lines:
                line = line.strip()
                if not line or line.startswith(("#", "import ", "import\t")):
                    continue
                if (Path(line) / "muse_mcp").exists():
                    return True
    except OSError:
        return False
    return False


def ensure_paths(prefix=None, version=None):
    """Restore the venv's site-packages and its `.pth` entries if missing.

    A launcher can start this interpreter with the venv prefix set while site
    handling stayed incomplete. `site.addsitedir` then does exactly what the
    interpreter would have done: add the directory and process its `.pth`, so
    both the community checkout and its dependencies become importable.
    """
    import site
    import sys
    prefix = Path(prefix or sys.prefix)
    version = version or "python{}.{}".format(*sys.version_info[:2])
    candidates = [prefix / "lib" / version / "site-packages",
                  prefix / "lib64" / version / "site-packages"]
    if not any(candidate.is_dir() for candidate in candidates):
        # The venv was built by another interpreter version, which is what a
        # container with its own Python produces. Its pure-Python and abi3
        # dependencies still load; a compiled-only one fails by module name
        # instead of the whole bridge reporting itself missing.
        candidates = [site_dir for site_dir in sorted(prefix.glob("lib*/python*/site-packages"))
                      if provides_checkout(site_dir)]
    for candidate in candidates:
        if candidate.is_dir() and str(candidate) not in sys.path:
            site.addsitedir(str(candidate))


def fallback_paths(prefix=None, executable=None):
    """What the pinned install provides when site handling did not run.

    A venv normally exposes the community checkout through a `.pth` entry and
    its dependencies through site-packages. A launcher that starts this
    interpreter without that processing leaves both off `sys.path`, which looks
    exactly like an uninstalled bridge. Both still exist on disk beside the
    interpreter, so the bridge can add them itself.
    """
    import sys
    prefix = Path(prefix or sys.prefix)
    executable = Path(executable or sys.executable)
    version = "python{}.{}".format(*sys.version_info[:2])
    roots = [prefix.parent, prefix]
    if len(executable.parents) > 2:
        roots.append(executable.parents[2])
    paths = []
    for root in roots:
        for candidate in (root / "source",
                          root / ".venv" / "lib" / version / "site-packages",
                          root / "lib" / version / "site-packages"):
            text = str(candidate)
            if candidate.is_dir() and text not in paths:
                paths.append(text)
    return paths


def write_report(exc):
    """Record what this process actually sees when the import fails.

    Local-only (0700 directory, 0600 file, paths and module names, never file
    contents or credentials). The status line cannot carry this much, and the
    same message kept firing for states that look identical from outside.
    """
    import sys
    try:
        directory = Path.home() / ".local/state/unofficial-muse-for-linux"
        directory.mkdir(mode=0o700, parents=True, exist_ok=True)
        version = "python{}.{}".format(*sys.version_info[:2])
        site_dirs = {}
        for candidate in (Path(sys.prefix) / "lib" / version / "site-packages",
                          Path(sys.prefix) / "lib64" / version / "site-packages"):
            readable = candidate.is_dir()
            site_dirs[str(candidate)] = {
                "exists": candidate.exists(),
                "listable": readable,
                "entries": sorted(os.listdir(candidate))[:40] if readable else [],
            }
        report = {
            "kind": type(exc).__name__,
            "missing": getattr(exc, "name", None),
            "python": sys.version,
            "executable": sys.executable,
            "prefix": sys.prefix,
            "base_prefix": sys.base_prefix,
            "flags": {"no_site": bool(sys.flags.no_site),
                      "isolated": bool(sys.flags.isolated)},
            "uid": os.getuid(),
            "cwd": os.getcwd(),
            "sys_path": list(sys.path),
            "site_dirs": site_dirs,
        }
        target = directory / "bridge-report.json"
        descriptor = os.open(target, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        with os.fdopen(descriptor, "w", encoding="utf-8") as handle:
            json.dump(report, handle, indent=2)
    except Exception:
        pass


def import_client():
    """Return (MuseClient, None) or (None, why the import could not complete)."""
    import sys
    search_path = tuple(sys.path)
    # Repair incomplete site handling before the first attempt: a launcher can
    # set the venv prefix yet leave its site-packages unprocessed.
    ensure_paths()
    try:
        from muse_mcp.client import MuseClient
        return MuseClient, None
    except (ImportError, OSError) as exc:
        first = exc
    added = False
    for path in fallback_paths():
        if path not in sys.path:
            sys.path.append(path)
            added = True
    if added:
        try:
            from muse_mcp.client import MuseClient
            return MuseClient, None
        except (ImportError, OSError) as exc:
            write_report(exc)
            return None, import_detail(exc, search_path)
    write_report(first)
    return None, import_detail(first, search_path)


@contextmanager
def shared_gateway(client, config):
    """Hold one upstream connection for a whole request, when supported.

    Every gateway call otherwise pays its own Noise handshake: measured at
    1.5-2.2s, against ~0.1s for a call on an already-open connection, and one
    request makes up to four calls (a send reads history, dispatches and reads
    history again to confirm).
    """
    scope = getattr(client, "gateway_scope", None)
    if scope is None:
        yield
        return
    with scope(config):
        yield


def community_client(config):
    MuseClient, detail = import_client()
    if MuseClient is None:
        raise BridgeFailure("bridge_unavailable", detail=detail) from None
    class AwaitedMuseClient(MuseClient):
        def __init__(self, *args, **kwargs):
            super().__init__(*args, **kwargs)
            self._shared_gateway = None

        @contextmanager
        def gateway_scope(self, config):
            if self._shared_gateway is not None:
                yield self._shared_gateway
                return
            with super()._gateway(config) as gateway:
                self._shared_gateway = gateway
                try:
                    yield gateway
                finally:
                    self._shared_gateway = None

        @contextmanager
        def _gateway(self, config):
            if self._shared_gateway is not None:
                yield self._shared_gateway
                return
            with super()._gateway(config) as gateway:
                yield gateway

        def send(self, message, thread=None, wait_seconds=0, fire_and_forget=True, device_id=None):
            # Keep the Noise connection alive until the server processes the
            # request. Upstream's immediate-close dispatch can drop the write.
            # History below, not this transport response, proves persistence.
            body = {"items": [{"type": "text", "text": message}],
                    "node_id": config["node_id"], "capabilities": {}}
            if device_id is not None:
                body["device_id"] = device_id
            if thread is not None:
                body["session_id"] = thread
            # The dispatch rides the connection this request already holds: it
            # served only this send (the identity check, then the baseline
            # read), so the write is not exposed to unrelated traffic, and the
            # whole send pays one handshake instead of two.
            with self._gateway(config) as gateway:
                gateway.request("chat.stream", body=body, timeout=30)
            return {"ok": True, "sent": True}
    return AwaitedMuseClient(config_loader=lambda: config)


def status_facts(client):
    """Account facts from the aggregate `status` route, for older clients."""
    status = checked(client.status(), dict)
    if status.get("authenticated") is not True:
        raise BridgeFailure("auth_required")
    if not isinstance(status.get("vm_id"), str) or not status["vm_id"]:
        raise BridgeFailure("protocol_error")
    identity = checked(status.get("identity", {}), dict)
    return {"identity_name": required_text(identity.get("name")),
            "vm_id": status["vm_id"]}


def session_facts(client, config):
    """Prove the saved session still belongs to one account, cheaply.

    `status` runs `sessions.list`, `chat.unread_count`, `identity` and
    `vm.health`; measured, `sessions.list` alone can take ten seconds. A send
    preflight needs only the identity and the VM its connection resolved, so it
    asks for exactly that on the connection the request already holds.
    """
    gateway = getattr(client, "_gateway", None)
    if gateway is None:
        return status_facts(client)
    with gateway(config) as connection:
        identity = connection.call_json("identity")
        vm_id = getattr(connection, "vm_id", None)
    if not isinstance(identity, dict):
        raise BridgeFailure("protocol_error")
    if not isinstance(vm_id, str) or not vm_id:
        raise BridgeFailure("protocol_error")
    return {"identity_name": required_text(identity.get("name")), "vm_id": vm_id}


def updated_at_text(session):
    millis = session.get("updated_at_ms")
    if isinstance(millis, (int, float)) and not isinstance(millis, bool):
        import time
        return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(millis / 1000.0))
    return optional_text(session.get("updated_at"))


def session_rows(client, config):
    """Project the conversation list from `sessions.list` itself.

    `status()` also runs `chat.unread_count` and `vm.health`, and a failing
    `vm.health` leaves a held connection answering the *next* request with a
    gateway error. The snapshot only needs the identity and this list, so it
    asks for exactly those and reads the route that answers in ~0.1s.
    """
    gateway_scope = getattr(client, "_gateway", None)
    if gateway_scope is None:
        return [{
            "session_id": required_text(row.get("session_id")),
            "title": required_text(row.get("title")),
            "archived": bool(row.get("archived", False)),
            "updated_at": optional_text(row.get("updated_at")),
        } for row in object_rows(client.threads(include_archived=True))]
    with gateway_scope(config) as gateway:
        data = gateway.call_json("sessions.list")
    sessions = data.get("sessions", []) if isinstance(data, dict) else None
    if not isinstance(sessions, list):
        raise BridgeFailure("protocol_error")
    rows = []
    for session in sessions:
        if not isinstance(session, dict):
            raise BridgeFailure("protocol_error")
        rows.append({
            "session_id": required_text(session.get("session_id")),
            "title": required_text(session.get("title")),
            "archived": bool(session.get("archived", False)),
            "updated_at": updated_at_text(session),
        })
    return rows


def error_response(error):
    response = {"ok": False, "error": {"code": error.code, "uncertain": error.uncertain}}
    detail = getattr(error, "detail", None)
    if detail:
        response["error"]["detail"] = detail
    return response


def handle_request(request, client_factory=None):
    try:
        validate_request(request)
        with secure_config(request["config_dir"]) as config:
            client = (client_factory or community_client)(config)
            if request["op"] in SHARED_GATEWAY_OPS:
                with shared_gateway(client, config):
                    data = Adapter(client, config).dispatch(request)
            else:
                data = Adapter(client, config).dispatch(request)
            return {"ok": True, "data": data}
    except Exception as exc:
        return error_response(classify_exception(exc))


class Adapter:
    def __init__(self, client, config, confirmation_window=8.0):
        self.client = client
        self.config = config
        # Wall clock, not a read count. Reads are cheap on a held connection
        # (~0.1s), so a fixed attempt count gave up ~1s after dispatch and
        # reported an uncertain delivery while the service was still persisting
        # the prompt.
        self.confirmation_window = confirmation_window

    def dispatch(self, request):
        self.send_started = False
        try:
            return self._dispatch(request)
        except Exception as exc:
            raise classify_exception(exc, self.send_started) from None

    def _dispatch(self, request):
        if request["op"] == "connect":
            facts = session_facts(self.client, self.config)
            return {
                "identity_name": facts["identity_name"],
                "vm_id": facts["vm_id"],
                "threads": session_rows(self.client, self.config),
            }
        if request["op"] == "identity":
            # The same verification as `connect` minus `sessions.list`, which the
            # service can take ten seconds to answer. A send preflight only has
            # to prove the saved session still belongs to the same account.
            return session_facts(self.client, self.config)
        if request["op"] == "history":
            return history_rows(self.client.history(thread=request.get("thread"), limit=request["limit"]))
        if request["op"] == "tasks":
            with self.client._gateway(self.config) as gateway:
                schedules = gateway.call_json("tasks.list")["schedules"]
                runs = gateway.call_json("tasks.runs")["runs"]
            return {
                "schedules": [{
                    "id": required_text(row.get("id")), "title": required_text(row.get("title")),
                    "enabled": bool(row.get("enabled", False)),
                    "timezone": optional_text(row.get("timezone")),
                    "next_run_at_utc": row.get("next_run_at_utc"),
                } for row in schedules],
                "runs": [{
                    "run_id": required_text(row.get("run_id")), "job_id": required_text(row.get("job_id")),
                    "status": required_text(row.get("status")),
                    "result_summary": content_text(row.get("result_summary")),
                    "error_text": content_text(row.get("error_text")),
                    "scheduled_for_utc": row.get("scheduled_for_utc"),
                } for row in runs],
            }
        if request["op"] == "watch":
            # Hold the request open on one connection and re-read cheaply: a
            # fresh history read per tick costs a handshake, which is what made
            # an answer take seconds to appear.
            import time
            timeout = request["timeout"] / 1000.0
            deadline = time.monotonic() + timeout
            # A caller that already knows the newest message it has seen sends
            # it as `after`: the first read then answers at once when anything
            # newer is already there, so nothing that arrived while the request
            # was being set up is skipped.
            after = request.get("after")
            baseline = None
            while True:
                rows = history_rows(self.client.history(thread=request.get("thread"),
                                                        limit=request["limit"]))
                newest = (rows[-1]["message_id"], rows[-1]["seq"]) if rows else None
                if baseline is None:
                    if after is not None and (newest is None or newest[0] != after):
                        return rows
                    baseline = newest
                elif newest != baseline:
                    return rows
                if time.monotonic() >= deadline:
                    return rows
                time.sleep(0.4)
        if request["op"] == "send":
            import time
            thread = request.get("thread")
            message = request["message"]
            if "expect_vm_id" in request:
                # Verify the session in the same request as the dispatch, so a
                # send pays one account check instead of a whole `connect`,
                # which also lists every conversation.
                facts = session_facts(self.client, self.config)
                if (facts["vm_id"] != request["expect_vm_id"]
                        or facts["identity_name"]
                        != request["expect_identity_name"]):
                    raise BridgeFailure("account_changed")
            # Confirming a send only needs the rows around the new message, and
            # on a held connection a read costs ~0.1s.
            baseline = history_rows(self.client.history(thread=thread, limit=25))
            baseline_seq = max((r["seq"] for r in baseline), default=0)
            baseline_ids = {r["message_id"] for r in baseline}
            self.send_started = True
            device = {"device_id": request["device_id"]} if "device_id" in request else {}
            result = checked(self.client.send(message=message, thread=thread, fire_and_forget=True, wait_seconds=0, **device), dict)
            if result.get("sent") is not True:
                raise BridgeFailure("protocol_error", uncertain=True)
            deadline = time.monotonic() + self.confirmation_window
            while True:
                rows = history_rows(self.client.history(thread=thread, limit=25))
                for row in rows:
                    if (row["seq"] > baseline_seq and row["message_id"] not in baseline_ids
                            and row["role"] == "user" and row["text"] == message):
                        return {"sent": True, "confirmed": True, "message_id": row["message_id"]}
                if time.monotonic() >= deadline:
                    break
                time.sleep(0.2)
            return {"sent": bool(result.get("sent")), "confirmed": False, "message_id": None}
        raise BridgeFailure("invalid_request")


def parse_pairs(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise BridgeFailure("invalid_request")
        result[key] = value
    return result


def main():
    import sys
    # Quiet both Python and native dependency diagnostics before any imports or
    # credential access. Only the saved response descriptor can reach stdout.
    response_fd = os.dup(sys.stdout.fileno())
    with open(os.devnull, "w") as quiet:
        os.dup2(quiet.fileno(), sys.stdout.fileno())
        os.dup2(quiet.fileno(), sys.stderr.fileno())
    request = None
    try:
        raw = sys.stdin.buffer.read(MAX_REQUEST + 1)
        if len(raw) > MAX_REQUEST:
            raise BridgeFailure("invalid_request")
        try:
            request = json.loads(raw, object_pairs_hook=parse_pairs)
        except (ValueError, UnicodeError):
            raise BridgeFailure("invalid_request") from None
        response = handle_request(request)
        encoded = json.dumps(response, ensure_ascii=True, allow_nan=False).encode("utf-8")
        if len(encoded) > MAX_RESPONSE:
            encoded = json.dumps(error_response(BridgeFailure("response_too_large",
                isinstance(request, dict) and request.get("op") == "send"))).encode("utf-8")
    except Exception as exc:
        encoded = json.dumps(error_response(classify_exception(exc))).encode("utf-8")
    try:
        with os.fdopen(response_fd, "wb") as output:
            output.write(encoded + b"\n")
    except OSError:
        pass


if __name__ == "__main__":
    main()
