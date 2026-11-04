"""Offline host behavior: no network, no credentials, no real session."""
import importlib.util
import json
import os
import pathlib
import socket
import subprocess
import sys
import tempfile
import threading
import time
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, ROOT / filename)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class FakeBridge:
    """The bridge's adapter and refusals, with a client that never connects."""

    def __init__(self, rows, cookies_file):
        from contextlib import contextmanager
        self.rows = rows
        self.cookies_file = cookies_file
        self.clients = 0
        self.session_dirs = []
        self._contextmanager = contextmanager
        self.bridge = load("account_bridge_for_host", "muse_account_bridge.py")
        self.BridgeFailure = self.bridge.BridgeFailure
        self.parse_pairs = self.bridge.parse_pairs
        self.classify_exception = self.bridge.classify_exception
        self.error_response = self.bridge.error_response
        self.validate_request = self.bridge.validate_request
        self.Adapter = self.bridge.Adapter

    def secure_config(self, config_dir):
        self.session_dirs.append(str(config_dir))
        bridge = self

        @self._contextmanager
        def scope():
            yield {"cookies_file": bridge.cookies_file, "node_id": "test"}

        return scope()

    def community_client(self, config):
        self.clients += 1
        fake = self

        class Client:
            def status(self):
                return {"ok": True, "authenticated": True, "vm_id": "vm-1",
                        "identity": {"name": "Test User"}}

            def history(self, thread=None, limit=20):
                return fake.rows

            @fake._contextmanager
            def gateway_scope(self, config):
                # The host holds this open for the connection's lifetime.
                yield object()

        return Client()


class HostTests(unittest.TestCase):
    def host(self):
        tmp = tempfile.TemporaryDirectory(dir=os.environ.get("TMPDIR") or pathlib.Path.home())
        self.addCleanup(tmp.cleanup)
        root = pathlib.Path(tmp.name)
        jar = root / "cookies.txt"
        jar.write_bytes(b"first")
        fake = FakeBridge([{"message_id": "m1", "seq": 1, "role": "user", "text": "hi"}], str(jar))
        module = load("account_host_under_test", "muse_account_host.py")
        host = module.Host(str(root), bridge=fake)
        self.addCleanup(host.close)
        return module, host, fake, jar, root

    def test_repeated_requests_share_one_held_connection(self):
        _, host, fake, _jar, _ = self.host()
        for _ in range(3):
            reply = host.handle_line(json.dumps({"op": "history", "limit": 20}).encode())
            self.assertTrue(reply["ok"], reply)
        self.assertEqual(fake.clients, 1)

    def test_a_wait_gets_its_own_connection_so_a_send_never_queues_behind_it(self):
        _, host, fake, _jar, _ = self.host()
        host.handle_line(json.dumps({"op": "history", "limit": 20}).encode())
        host.handle_line(json.dumps({"op": "watch", "limit": 20, "timeout": 1}).encode())
        self.assertEqual(fake.clients, 2)

    def test_a_changed_cookie_jar_replaces_the_held_connection(self):
        _, host, fake, jar, _ = self.host()
        host.handle_line(json.dumps({"op": "history", "limit": 20}).encode())
        jar.write_bytes(b"second")
        host.handle_line(json.dumps({"op": "history", "limit": 20}).encode())
        self.assertEqual(fake.clients, 2)

    def test_a_request_cannot_redirect_the_session_directory(self):
        _, host, fake, _jar, root = self.host()
        reply = host.handle_line(json.dumps(
            {"op": "history", "limit": 20, "config_dir": "/elsewhere"}).encode())
        self.assertTrue(reply["ok"], reply)
        self.assertEqual(fake.session_dirs, [str(root)])

    def test_bad_framing_is_answered_never_raised(self):
        _, host, _fake, _jar, _ = self.host()
        for line in (b"{oops", b'["op"]', b'{"op": "raw", "method": "chat.stream"}',
                     b'{"op": "history", "limit": 20, "extra": 1}'):
            with self.subTest(line=line):
                reply = host.handle_line(line)
                self.assertFalse(reply["ok"])
                self.assertEqual(reply["error"]["code"], "invalid_request")

    def test_the_socket_name_is_a_published_fnv_1a_64_and_stable(self):
        module = load("account_host_fnv", "muse_account_host.py")
        self.assertEqual(module.fnv1a64(b""), 0xCBF29CE484222325)
        self.assertEqual(module.fnv1a64(b"a"), 0xAF63DC4C8601EC8C)
        self.assertEqual(module.fnv1a64(b"foobar"), 0x85944171F73967E8)
        environ = {"XDG_RUNTIME_DIR": "/run/user/1000", "HOME": "/home/x"}
        first = module.socket_path("/home/x/.config/account", environ)
        self.assertEqual(first, module.socket_path("/home/x/.config/account", environ))
        self.assertTrue(first.startswith("/run/user/1000/unofficial-muse-for-linux/account-"))
        self.assertNotEqual(first, module.socket_path("/home/x/.config/other", environ))

    def test_the_socket_is_private_and_serves_a_request(self):
        module, host, fake, _jar, root = self.host()
        environ = {"XDG_RUNTIME_DIR": str(root / "run"), "HOME": str(root)}
        path = pathlib.Path(module.socket_path(str(root), environ))
        stop = threading.Event()
        thread = threading.Thread(target=module.serve, args=(host, str(path), stop), daemon=True)
        thread.start()
        for _ in range(200):
            if path.exists():
                break
            time.sleep(0.02)
        self.assertTrue(path.exists(), "the host never bound its socket")
        self.assertEqual(path.stat().st_mode & 0o777, 0o600)
        self.assertEqual(path.parent.stat().st_mode & 0o777, 0o700)
        client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        client.settimeout(10)
        client.connect(str(path))
        client.sendall(json.dumps({"op": "history", "limit": 20}).encode() + b"\n")
        data = b""
        while not data.endswith(b"\n"):
            data += client.recv(65536)
        self.assertTrue(json.loads(data)["ok"])
        self.assertEqual(fake.clients, 1)
        client.close()
        stop.set()
        thread.join(10)
        self.assertFalse(path.exists(), "an idle host must remove its socket")
        self.assertFalse(module.already_served(str(path)))

    def test_an_oversized_line_is_refused_and_the_connection_closed(self):
        module, host, _fake, _jar, root = self.host()
        environ = {"XDG_RUNTIME_DIR": str(root / "run"), "HOME": str(root)}
        path = pathlib.Path(module.socket_path(str(root), environ))
        stop = threading.Event()
        thread = threading.Thread(target=module.serve, args=(host, str(path), stop), daemon=True)
        thread.start()
        for _ in range(200):
            if path.exists():
                break
            time.sleep(0.02)
        client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        client.settimeout(10)
        client.connect(str(path))
        client.sendall(b"x" * (module.MAX_LINE + 8) + b"\n")
        data = b""
        while not data.endswith(b"\n"):
            chunk = client.recv(65536)
            if not chunk:
                break
            data += chunk
        self.assertEqual(json.loads(data)["error"]["code"], "invalid_request")
        client.close()
        stop.set()
        thread.join(10)

    def test_the_host_exits_when_its_parent_closes_stdin(self):
        module = load("account_host_exit", "muse_account_host.py")
        with tempfile.TemporaryDirectory(dir=os.environ.get("TMPDIR") or pathlib.Path.home()) as tmp:
            root = pathlib.Path(tmp)
            runtime = root / "run"
            config = root / "cfg"
            config.mkdir(mode=0o700)
            environ = dict(os.environ, XDG_RUNTIME_DIR=str(runtime))
            path = pathlib.Path(module.socket_path(str(config), {"XDG_RUNTIME_DIR": str(runtime)}))
            process = subprocess.Popen(
                [sys.executable, "-I", str(ROOT / "muse_account_host.py"),
                 "--config-dir", str(config), "--idle-seconds", "60"],
                stdin=subprocess.PIPE, stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL, env=environ)
            try:
                for _ in range(250):
                    if path.exists():
                        break
                    time.sleep(0.02)
                self.assertTrue(path.exists(), "the host never created its socket")
                process.stdin.close()
                self.assertEqual(process.wait(timeout=15), 0)
                self.assertFalse(path.exists())
            finally:
                if process.poll() is None:
                    process.kill()
                    process.wait(5)


if __name__ == "__main__":
    unittest.main()
