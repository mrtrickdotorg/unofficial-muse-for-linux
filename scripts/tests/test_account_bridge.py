"""Offline behavior tests: no real Muse credentials or network."""
import importlib.util
import pathlib
import unittest

SCRIPT = pathlib.Path(__file__).resolve().parents[1] / "muse_account_bridge.py"


def load_bridge():
    spec = importlib.util.spec_from_file_location("account_bridge", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class FakeClient:
    def status(self):
        return {"ok": True, "authenticated": True, "vm_id": "vm-1",
                "identity": {"name": "Test User", "access_token": "SECRET"},
                "vm": {"health": {"cookie": "SECRET"}}}

    def threads(self, include_archived=False):
        return [{"session_id": "thread-1", "title": "Example", "archived": False,
                 "updated_at": "2026-10-17T00:00:00Z", "raw": "SECRET"}]


class BridgeTests(unittest.TestCase):
    def test_missing_community_library_reports_interpreter_facts_only(self):
        import sys
        from unittest import mock
        bridge = load_bridge()
        with mock.patch.dict(sys.modules, {"muse_mcp": None, "muse_mcp.client": None}), \
                mock.patch.object(bridge, "fallback_paths", return_value=[]):
            with self.assertRaises(bridge.BridgeFailure) as caught:
                bridge.community_client({})
        self.assertEqual(caught.exception.code, "bridge_unavailable")
        detail = caught.exception.detail
        self.assertRegex(detail, r"(ModuleNotFound)?Error")
        self.assertIn("prefix=", detail)
        self.assertIn("site_packages_on_sys_path=", detail)
        self.assertIn("running=", detail)
        # Bounded, so no field can be truncated away unexplained.
        self.assertLessEqual(len(detail), 300)
        # The module that was actually missing is named, without any message text.
        self.assertIn("missing=", detail)
        self.assertIn("site_packages_on_sys_path=", detail)
        # Whether the missing module exists on disk separates a search-path
        # fault from a genuinely absent dependency.
        self.assertIn("installed_in_venv=", detail)
        self.assertLessEqual(len(detail), 300)
        # The response carries the detail, but only when there is one.
        self.assertEqual(bridge.error_response(caught.exception)["error"]["detail"], detail)
        self.assertNotIn("detail", bridge.error_response(
            bridge.BridgeFailure("bridge_unavailable"))["error"])

    def test_failed_import_records_what_the_process_saw(self):
        import json
        import os
        import pathlib
        import tempfile
        from unittest import mock
        bridge = load_bridge()
        missing = ModuleNotFoundError("noise", name="noise")
        with tempfile.TemporaryDirectory() as home:
            with mock.patch.dict(os.environ, {"HOME": home}):
                bridge.write_report(missing)
            target = (pathlib.Path(home) / ".local/state/unofficial-muse-for-linux"
                      / "bridge-report.json")
            self.assertTrue(target.is_file())
            # Paths only: never readable by group or other.
            self.assertEqual(target.stat().st_mode & 0o077, 0)
            report = json.loads(target.read_text(encoding="utf-8"))
            self.assertEqual(report["missing"], "noise")
            self.assertEqual(report["kind"], "ModuleNotFoundError")
            for key in ("python", "executable", "prefix", "sys_path", "site_dirs",
                        "uid", "cwd", "flags"):
                self.assertIn(key, report)

    def test_a_venv_built_by_another_python_version_is_still_usable(self):
        import pathlib
        import sys
        import tempfile
        bridge = load_bridge()
        with tempfile.TemporaryDirectory() as root:
            root = pathlib.Path(root)
            (root / "source" / "muse_mcp").mkdir(parents=True)
            # What a container with a newer Python produces: only the building
            # interpreter's directory exists on disk.
            foreign = root / "lib" / "python9.9" / "site-packages"
            (foreign / "muse_mcp").mkdir(parents=True)
            (foreign / "checkout.pth").write_text(str(root / "source") + "\n",
                                                  encoding="utf-8")
            bridge.ensure_paths(prefix=root)
            self.assertIn(str(foreign), sys.path)
            self.assertIn(str(root / "source"), sys.path)
            sys.path[:] = [p for p in sys.path
                           if p not in (str(foreign), str(root / "source"))]

    def test_a_pth_linked_checkout_counts_as_the_source(self):
        # What the installer really produces: no muse_mcp inside site-packages,
        # only a `.pth` naming the checkout.
        import pathlib
        import sys
        import tempfile
        bridge = load_bridge()
        with tempfile.TemporaryDirectory() as root:
            root = pathlib.Path(root)
            source = root / "source"
            (source / "muse_mcp").mkdir(parents=True)
            foreign = root / "lib" / "python9.9" / "site-packages"
            foreign.mkdir(parents=True)
            (foreign / "checkout.pth").write_text(str(source) + "\n",
                                                  encoding="utf-8")
            self.assertTrue(bridge.provides_checkout(foreign))
            bridge.ensure_paths(prefix=root)
            self.assertIn(str(foreign), sys.path)
            self.assertIn(str(source), sys.path)
            sys.path[:] = [p for p in sys.path
                           if p not in (str(foreign), str(source))]

    def test_a_version_split_says_how_to_cure_it(self):
        import pathlib
        import sys
        import tempfile
        from unittest import mock
        bridge = load_bridge()
        with tempfile.TemporaryDirectory() as root:
            root = pathlib.Path(root)
            (root / "source" / "muse_mcp").mkdir(parents=True)
            site_dir = root / "lib" / "python3.4" / "site-packages"
            site_dir.mkdir(parents=True)
            (root / "pyvenv.cfg").write_text("version = 3.4.0\nhome = /usr/bin\n",
                                             encoding="utf-8")
            (site_dir / "checkout.pth").write_text(str(root / "source") + "\n",
                                                   encoding="utf-8")
            with mock.patch.object(sys, "prefix", str(root)), \
                    mock.patch.object(sys, "executable", str(root / "bin" / "python")):
                detail = bridge.import_detail(
                    ModuleNotFoundError("_cffi_backend", name="_cffi_backend"), [])
        self.assertIn("venv_built_for=3.4", detail)
        self.assertIn("rebuild the venv with Python", detail)
        self.assertIn("setup-account-bridge.sh", detail)
        # The mismatch must not be truncated away.
        self.assertIn("missing=_cffi_backend", detail)
        self.assertLessEqual(len(detail), 300)

    def test_an_extra_version_directory_is_not_a_mismatch(self):
        # A venv may carry site-packages for several interpreters (that is how
        # the bridge survives a container with its own Python); only pyvenv.cfg
        # says which interpreter built it.
        import pathlib
        import sys
        import tempfile
        from unittest import mock
        bridge = load_bridge()
        running = "{}.{}".format(*sys.version_info[:2])
        with tempfile.TemporaryDirectory() as root:
            root = pathlib.Path(root)
            (root / "source" / "muse_mcp").mkdir(parents=True)
            (root / "pyvenv.cfg").write_text("version = {}\n".format(running),
                                             encoding="utf-8")
            other = root / "lib" / "python9.9" / "site-packages"
            other.mkdir(parents=True)
            (other / "checkout.pth").write_text(str(root / "source") + "\n",
                                                encoding="utf-8")
            with mock.patch.object(sys, "prefix", str(root)), \
                    mock.patch.object(sys, "executable", str(root / "bin" / "python")):
                detail = bridge.import_detail(ModuleNotFoundError("x", name="noise"), [])
        self.assertNotIn("venv_built_for=", detail)
        self.assertIn("prefix={}".format(root), detail)

    def test_venv_site_packages_and_pth_entries_are_restored(self):
        import pathlib
        import sys
        import tempfile
        bridge = load_bridge()
        version = "python{}.{}".format(*sys.version_info[:2])
        with tempfile.TemporaryDirectory() as root:
            root = pathlib.Path(root)
            site = root / "lib" / version / "site-packages"
            site.mkdir(parents=True)
            checkout = root / "source"
            checkout.mkdir()
            (site / "restore.pth").write_text(str(checkout) + "\n", encoding="utf-8")
            bridge.ensure_paths(prefix=root, version=version)
            # The directory itself, and what its .pth points at, are both usable.
            self.assertIn(str(site), sys.path)
            self.assertIn(str(checkout), sys.path)
            sys.path[:] = [p for p in sys.path if p not in (str(site), str(checkout))]

    def test_fallback_paths_cover_the_checkout_and_the_venv_site_packages(self):
        import pathlib
        import sys
        import tempfile
        bridge = load_bridge()
        version = "python{}.{}".format(*sys.version_info[:2])
        with tempfile.TemporaryDirectory() as root:
            root = pathlib.Path(root)
            (root / "source" / "muse_mcp").mkdir(parents=True)
            site = root / ".venv" / "lib" / version / "site-packages"
            site.mkdir(parents=True)
            paths = bridge.fallback_paths(prefix=root / ".venv",
                                         executable=root / ".venv" / "bin" / "python")
            # Dependencies live in site-packages; the checkout is beside the venv.
            self.assertIn(str(root / "source"), paths)
            self.assertIn(str(site), paths)
            self.assertLess(paths.index(str(root / "source")),
                            paths.index(str(site)))
            # The executable alone locates the same install.
            self.assertIn(str(root / "source"), bridge.fallback_paths(
                prefix=pathlib.Path("/usr"), executable=root / ".venv" / "bin" / "python"))
        with tempfile.TemporaryDirectory() as empty:
            self.assertEqual(bridge.fallback_paths(
                prefix=pathlib.Path(empty) / ".venv",
                executable=pathlib.Path("/usr/bin/python3.12")), [])

    def test_optional_device_is_validated_and_forwarded_once(self):
        bridge = load_bridge()
        request = {"op": "send", "message": "hello", "config_dir": "/unused", "device_id": "linux-0123"}
        bridge.validate_request(request)
        class Client:
            def history(self, **kwargs): return []
            def send(self, **kwargs):
                self.sent = kwargs
                return {"ok": True, "sent": True}
        client = Client()
        bridge.Adapter(client, {}, confirmation_window=0).dispatch(request)
        self.assertEqual(client.sent["device_id"], "linux-0123")
        for invalid in [None, "", " spaces ", "x/../y", "x" * 129, 3, "ü"]:
            with self.subTest(invalid=invalid), self.assertRaises(bridge.BridgeFailure):
                bridge.validate_request(dict(request, device_id=invalid))

    def test_a_quoted_send_forwards_the_reply_target_and_a_plain_one_does_not(self):
        # The quote's id is the service's own message id; a plain send must
        # reach the client with no target at all, exactly as before.
        bridge = load_bridge()
        class Client:
            def history(self, **kwargs):
                return []
            def send(self, **kwargs):
                self.sent = kwargs
                return {"ok": True, "sent": True}
        client = Client()
        quoted = {"op": "send", "message": "hello", "config_dir": "/unused",
                  "reply_to": "m-9"}
        bridge.validate_request(quoted)
        bridge.Adapter(client, {}, confirmation_window=0).dispatch(quoted)
        self.assertEqual(client.sent["reply_to"], "m-9")
        plain = {"op": "send", "message": "hello", "config_dir": "/unused"}
        bridge.validate_request(plain)
        bridge.Adapter(client, {}, confirmation_window=0).dispatch(plain)
        self.assertNotIn("reply_to", client.sent)
        for bad in ["", "x" * 257, "\x00m", 3, ["m-9"]]:
            with self.subTest(bad=bad), self.assertRaises(bridge.BridgeFailure):
                bridge.validate_request(dict(quoted, reply_to=bad))

    def test_watch_returns_on_a_new_message_and_times_out_on_a_quiet_talk(self):
        # A watch must hold one request instead of making the caller tick a
        # history poll per two seconds, and it must never hang past its timeout.
        bridge = load_bridge()
        first = {"message_id": "a", "seq": 1, "role": "user", "text": "hi"}
        class Busy(FakeClient):
            def __init__(self):
                self.calls = 0
            def history(self, **kwargs):
                self.calls += 1
                rows = [first]
                if self.calls >= 3:
                    rows.append({"message_id": "b", "seq": 2, "role": "assistant", "text": "done"})
                return rows
        client = Busy()
        result = bridge.Adapter(client, {}).dispatch({"op": "watch", "limit": 20, "timeout": 5000})
        self.assertEqual([r["message_id"] for r in result], ["a", "b"])
        self.assertEqual(client.calls, 3)
        class Quiet(FakeClient):
            def history(self, **kwargs):
                return [first]
        self.assertEqual(
            [r["message_id"] for r in bridge.Adapter(Quiet(), {}).dispatch(
                {"op": "watch", "limit": 20, "timeout": 1})],
            ["a"])
        for bad in [{"op": "watch", "limit": 0, "timeout": 10},
                    {"op": "watch", "limit": 20, "timeout": 0},
                    {"op": "watch", "limit": 20, "timeout": 60001},
                    {"op": "watch", "limit": 20, "timeout": True},
                    {"op": "watch", "limit": 20},
                    {"op": "watch", "limit": 20, "timeout": 10, "after": 3},
                    {"op": "history", "limit": 20, "timeout": 10}]:
            with self.subTest(bad=bad), self.assertRaises(bridge.BridgeFailure):
                bridge.validate_request(dict(bad, config_dir="/unused"))

    def test_watch_after_answers_on_the_first_read_when_already_newer(self):
        # A caller that knows what it has seen must not wait for the *next*
        # change: a reply that landed during the reconnect is already newer.
        import time
        bridge = load_bridge()
        rows = [{"message_id": "a", "seq": 1, "role": "user", "text": "hi"},
                {"message_id": "b", "seq": 2, "role": "assistant", "text": "done"}]
        class Client(FakeClient):
            def history(self, **kwargs):
                return rows
        started = time.monotonic()
        result = bridge.Adapter(Client(), {}).dispatch(
            {"op": "watch", "limit": 20, "timeout": 30000, "after": "a"})
        self.assertEqual([r["message_id"] for r in result], ["a", "b"])
        self.assertLess(time.monotonic() - started, 1.0)
        started = time.monotonic()
        result = bridge.Adapter(Client(), {}).dispatch(
            {"op": "watch", "limit": 20, "timeout": 300, "after": "b"})
        self.assertGreaterEqual(time.monotonic() - started, 0.25)
        self.assertEqual([r["message_id"] for r in result], ["a", "b"])
        for bad in ["", "x" * 257, 3, None]:
            with self.subTest(bad=bad), self.assertRaises(bridge.BridgeFailure):
                bridge.validate_request({"op": "watch", "limit": 20, "timeout": 10,
                    "after": bad, "config_dir": "/unused"})

    def test_watch_returns_when_the_newest_row_mutates_in_place(self):
        # A reply being produced keeps its message id and seq while its text
        # grows and its status flips at the end. A watch keyed on the id alone
        # slept through all of that and answered only at its deadline, which
        # is why a finished reply reached the screen up to a watch-window
        # late.
        import time
        bridge = load_bridge()
        user = {"message_id": "a", "seq": 1, "role": "user", "text": "hi"}
        class Growing(FakeClient):
            def __init__(self):
                self.calls = 0
            def history(self, **kwargs):
                self.calls += 1
                reply = {"message_id": "b", "seq": 2, "role": "assistant",
                         "text": "partial" if self.calls < 3 else "partial and more"}
                if self.calls >= 3:
                    reply["status"] = "completed"
                return [user, reply]
        client = Growing()
        started = time.monotonic()
        result = bridge.Adapter(client, {}).dispatch(
            {"op": "watch", "limit": 20, "timeout": 30000, "after": "a"})
        self.assertLess(time.monotonic() - started, 5.0)
        # The first read already had a row newer than `after` (the in-flight
        # reply), so this watch answered at once; a second watch anchored on
        # the reply itself must then wake on the in-place change alone.
        client.calls = 0
        started = time.monotonic()
        result = bridge.Adapter(client, {}).dispatch(
            {"op": "watch", "limit": 20, "timeout": 30000, "after": "b"})
        self.assertLess(time.monotonic() - started, 5.0)
        self.assertEqual(result[-1]["text"], "partial and more")
        self.assertEqual(result[-1]["status"], "completed")

    def test_watch_returns_early_on_a_terminal_live_status(self):
        # The 12.1 client holds one chat.subscribe for the session: a finished
        # turn is visible as a terminal live status even when the transcript
        # read has not caught up yet. A watch that only fingerprints history
        # sleeps through that and answers at its deadline, which is why a
        # finished answer could sit for a full watch window before printing.
        import time
        from contextlib import contextmanager
        bridge = load_bridge()
        user = {"message_id": "a", "seq": 1, "role": "user", "text": "hi"}
        reply = {"message_id": "b", "seq": 2, "role": "assistant", "text": "partial"}
        class Live(FakeClient):
            def __init__(self):
                self.history_calls = 0
                self.methods = []
            def history(self, **kwargs):
                self.history_calls += 1
                return [user, reply]
            @contextmanager
            def _gateway(self, config):
                outer = self
                class Gateway:
                    def subscribe_json(self, method, **kwargs):
                        outer.methods.append(method)
                        return [{"event": "agent.status", "seq": 9,
                                 "payload": {"status": "completed",
                                             "session_id": "s1"}}]
                yield Gateway()
        client = Live()
        started = time.monotonic()
        result = bridge.Adapter(client, {}).dispatch(
            {"op": "watch", "limit": 20, "timeout": 30000})
        self.assertLess(time.monotonic() - started, 5.0)
        self.assertEqual([r["message_id"] for r in result], ["a", "b"])
        self.assertEqual(result[-1]["text"], "partial")
        self.assertEqual(client.methods, ["chat.subscribe"])
        # One baseline probe plus the final confirming read: no polling to
        # the deadline happened.
        self.assertEqual(client.history_calls, 2)

    def test_watch_ignores_a_working_live_status_and_polls_to_its_deadline(self):
        # A non-terminal status is presence, not a finished turn: the watch
        # must keep polling history instead of answering on it.
        import time
        from contextlib import contextmanager
        bridge = load_bridge()
        user = {"message_id": "a", "seq": 1, "role": "user", "text": "hi"}
        reply = {"message_id": "b", "seq": 2, "role": "assistant", "text": "partial"}
        class Live(FakeClient):
            def __init__(self):
                self.history_calls = 0
                self.methods = []
            def history(self, **kwargs):
                self.history_calls += 1
                return [user, reply]
            @contextmanager
            def _gateway(self, config):
                outer = self
                class Gateway:
                    def subscribe_json(self, method, **kwargs):
                        outer.methods.append(method)
                        return [{"event": "agent.status", "seq": 9,
                                 "payload": {"status": "Is working",
                                             "session_id": "s1"}}]
                yield Gateway()
        client = Live()
        result = bridge.Adapter(client, {}).dispatch(
            {"op": "watch", "limit": 20, "timeout": 2000})
        self.assertEqual([r["message_id"] for r in result], ["a", "b"])
        self.assertEqual(client.methods, ["chat.subscribe"])
        # The status did not end the watch: history kept being polled.
        self.assertGreaterEqual(client.history_calls, 3)

    def test_watch_listener_opens_its_own_connection(self):
        # One frame consumer per connection: a listener sharing the probes'
        # connection would steal their frames and corrupt the Noise decrypt.
        # When the client offers a fresh connection, the listener must use
        # it and never touch the shared one.
        import time
        from contextlib import contextmanager
        bridge = load_bridge()
        user = {"message_id": "a", "seq": 1, "role": "user", "text": "hi"}
        reply = {"message_id": "b", "seq": 2, "role": "assistant", "text": "partial"}
        class Live(FakeClient):
            def __init__(self):
                self.history_calls = 0
                self.methods = []
            def history(self, **kwargs):
                self.history_calls += 1
                return [user, reply]
            @contextmanager
            def _gateway(self, config):
                raise AssertionError("the live listener must not share the probes' connection")
                yield
            @contextmanager
            def gateway_fresh(self, config):
                outer = self
                class Gateway:
                    def subscribe_json(self, method, **kwargs):
                        outer.methods.append(method)
                        return [{"event": "agent.status", "seq": 9,
                                 "payload": {"status": "completed",
                                             "session_id": "s1"}}]
                yield Gateway()
        client = Live()
        started = time.monotonic()
        result = bridge.Adapter(client, {}).dispatch(
            {"op": "watch", "limit": 20, "timeout": 30000})
        self.assertLess(time.monotonic() - started, 5.0)
        self.assertEqual([r["message_id"] for r in result], ["a", "b"])
        self.assertEqual(client.methods, ["chat.subscribe"])
        self.assertEqual(client.history_calls, 2)

    def test_watch_ignores_a_terminal_status_from_an_already_settled_turn(self):
        # A subscribe opened mid-watch replays recent events: a terminal
        # status from a turn the transcript already shows as settled must
        # not end the watch.
        from contextlib import contextmanager
        bridge = load_bridge()
        user = {"message_id": "a", "seq": 1, "role": "user", "text": "hi"}
        settled = {"message_id": "b", "seq": 2, "role": "assistant",
                   "text": "done", "raw": {"payload": {"status": "complete"}}}
        class Live(FakeClient):
            def __init__(self):
                self.history_calls = 0
                self.methods = []
            def history(self, **kwargs):
                self.history_calls += 1
                return [user, settled]
            @contextmanager
            def _gateway(self, config):
                outer = self
                class Gateway:
                    def subscribe_json(self, method, **kwargs):
                        outer.methods.append(method)
                        return [{"event": "agent.status", "seq": 9,
                                 "payload": {"status": "completed",
                                             "session_id": "s1"}}]
                yield Gateway()
        client = Live()
        result = bridge.Adapter(client, {}).dispatch(
            {"op": "watch", "limit": 20, "timeout": 2000})
        self.assertEqual([r["message_id"] for r in result], ["a", "b"])
        self.assertEqual(client.methods, ["chat.subscribe"])
        self.assertGreaterEqual(client.history_calls, 3)

    def test_watch_probes_cheaply_and_returns_the_requested_window(self):
        # Ticks read a small tail so a long conversation is not re-read in
        # full twice a second; the rows a watch returns are one read at the
        # requested limit.
        bridge = load_bridge()
        class Sized(FakeClient):
            def __init__(self):
                self.limits = []
                self.calls = 0
            def history(self, **kwargs):
                self.limits.append(kwargs["limit"])
                self.calls += 1
                count = kwargs["limit"]
                rows = [{"message_id": f"m{i}", "seq": i, "role": "user",
                         "text": f"row {i}"} for i in range(count)]
                if self.calls >= 2:
                    rows.append({"message_id": "new", "seq": count + 1,
                                 "role": "assistant", "text": "reply"})
                return rows
        client = Sized()
        result = bridge.Adapter(client, {}).dispatch(
            {"op": "watch", "limit": 50, "timeout": 5000})
        self.assertEqual(client.limits[0], 25)
        self.assertTrue(all(limit == 25 for limit in client.limits[:-1]))
        self.assertEqual(client.limits[-1], 50)
        self.assertEqual(result[-1]["message_id"], "new")

    def test_history_window_pages_around_an_anchor_without_a_full_reread(self):
        # The web UI pages with chat.history_window ({anchor_seq,
        # before/after_limit:20}) instead of re-reading long conversations
        # in full: the window returns the rows plus the paging flags.
        bridge = load_bridge()
        from contextlib import contextmanager
        seen = {}
        class Gateway:
            def call_json(self, method, body=None, **kwargs):
                seen["method"] = method
                seen["body"] = body
                return {"chat_events": [
                    {"message_id": "a", "seq": 8, "role": "user", "text": "eight"},
                    {"message_id": "b", "seq": 9, "role": "assistant", "text": "nine",
                     "raw": {"payload": {"status": "complete"}}},
                ], "has_older": True, "has_newer": False, "newest_seq": 12,
                    "transcript_revision": "r1"}
        class Client(FakeClient):
            @contextmanager
            def _gateway(self, config):
                yield Gateway()
        result = bridge.Adapter(Client(), {}).dispatch(
            {"op": "history_window", "thread": "t1", "anchor_seq": 9,
             "before_limit": 20, "after_limit": 20})
        self.assertEqual(seen["method"], "chat.history_window")
        self.assertEqual(seen["body"], {"anchor_seq": 9, "before_limit": 20,
                                        "after_limit": 20})
        self.assertEqual([r["message_id"] for r in result["messages"]], ["a", "b"])
        self.assertEqual(result["has_older"], True)
        self.assertEqual(result["has_newer"], False)
        self.assertEqual(result["newest_seq"], 12)
        self.assertNotIn("SECRET", str(result))

    def test_history_window_rejects_a_missing_anchor_or_bad_window(self):
        bridge = load_bridge()
        bridge.validate_request({"op": "history_window", "config_dir": "/unused",
                                 "thread": "t1", "anchor_seq": 9,
                                 "before_limit": 20, "after_limit": 20})
        for bad in [{"op": "history_window", "config_dir": "/unused",
                     "before_limit": 20, "after_limit": 20},
                    {"op": "history_window", "config_dir": "/unused",
                     "thread": "t1", "anchor_seq": True,
                     "before_limit": 20, "after_limit": 20},
                    {"op": "history_window", "config_dir": "/unused",
                     "thread": "t1", "anchor_seq": 9,
                     "before_limit": 0, "after_limit": 20},
                    {"op": "history_window", "config_dir": "/unused",
                     "thread": "t1", "anchor_seq": 9,
                     "before_limit": 20, "after_limit": 201}]:
            with self.subTest(bad=bad), self.assertRaises(bridge.BridgeFailure):
                bridge.validate_request(bad)

    def test_stream_events_replays_a_turn_from_either_anchor(self):
        # The web UI replays a turn with chat.stream_events ({message_id |
        # reply_to_message_id, stream_lane:'all'}): the bridge projects the
        # same allowlisted event shape the activity read uses.
        bridge = load_bridge()
        from contextlib import contextmanager
        seen = {}
        class Gateway:
            def call_json(self, method, body=None, **kwargs):
                seen["method"] = method
                seen["body"] = body
                return {"events": [
                    {"event": "delta.text_append", "seq": 3,
                     "occurred_at_ms": 100,
                     "payload": {"content": "Hel", "session_id": "s1",
                                 "access_token": "SECRET"}},
                    {"event": "chat.done", "seq": 4, "occurred_at_ms": 200,
                     "payload": {"status": "complete", "session_id": "s1"}},
                ]}
        class Client(FakeClient):
            @contextmanager
            def _gateway(self, config):
                yield Gateway()
        for anchor in ({"message_id": "m1"}, {"reply_to_message_id": "m1"}):
            seen.clear()
            result = bridge.Adapter(Client(), {}).dispatch(
                dict({"op": "stream_events", "limit": 100}, **anchor))
            self.assertEqual(seen["method"], "chat.stream_events")
            self.assertEqual(seen["body"]["stream_lane"], "all")
            self.assertEqual(seen["body"]["limit"], 100)
            self.assertEqual(result, {"events": [
                {"event": "delta.text_append", "seq": 3, "text": "Hel",
                 "status": None, "session_id": "s1"},
                {"event": "chat.done", "seq": 4, "text": None,
                 "status": "complete", "session_id": "s1"},
            ]})
            self.assertNotIn("SECRET", str(result))

    def test_stream_events_needs_one_anchor_and_a_bounded_limit(self):
        bridge = load_bridge()
        bridge.validate_request({"op": "stream_events", "config_dir": "/unused",
                                 "message_id": "m1", "limit": 100})
        for bad in [{"op": "stream_events", "config_dir": "/unused",
                     "limit": 100},
                    {"op": "stream_events", "config_dir": "/unused",
                     "message_id": "m1", "reply_to_message_id": "m1",
                     "limit": 100},
                    {"op": "stream_events", "config_dir": "/unused",
                     "message_id": "", "limit": 100},
                    {"op": "stream_events", "config_dir": "/unused",
                     "message_id": "m1", "limit": 0}]:
            with self.subTest(bad=bad), self.assertRaises(bridge.BridgeFailure):
                bridge.validate_request(bad)

    def test_subscribe_opens_one_per_area_subscription(self):
        # The shipped client holds one subscribe per area (activity, fs,
        # identity, ...): the bridge opens exactly the named method with a
        # small scalar body and projects the same allowlisted event shape
        # the activity read uses.
        bridge = load_bridge()
        from contextlib import contextmanager
        seen = {}
        class Gateway:
            def subscribe_json(self, method, body=None, **kwargs):
                seen["method"] = method
                seen["body"] = body
                return [{"event": "agent.status", "seq": 9,
                         "payload": {"status": "completed",
                                     "session_id": "s1",
                                     "access_token": "SECRET"}}]
        class Client(FakeClient):
            @contextmanager
            def _gateway(self, config):
                yield Gateway()
        result = bridge.Adapter(Client(), {}).dispatch(
            {"op": "subscribe", "method": "fs.subscribe",
             "body": {"paths": ["/a"]}, "limit": 50})
        self.assertEqual(seen["method"], "fs.subscribe")
        self.assertEqual(seen["body"], {"paths": ["/a"]})
        self.assertEqual(result["events"][0]["status"], "completed")
        self.assertNotIn("SECRET", str(result))

    def test_subscribe_names_an_unsupported_method_instead_of_a_bad_request(self):
        # The shipped client answers an unknown method locally with
        # unsupported_method, not a generic refusal: a well-formed name
        # outside the held subscriptions gets that code, while a
        # malformed method is still an invalid request.
        bridge = load_bridge()
        with self.assertRaises(bridge.BridgeFailure) as caught:
            bridge.validate_request({"op": "subscribe", "config_dir": "/unused",
                                     "method": "peers.subscribe", "limit": 50})
        self.assertEqual(caught.exception.code, "unsupported_method")
        for bad in [{"op": "subscribe", "config_dir": "/unused",
                     "method": "", "limit": 50},
                    {"op": "subscribe", "config_dir": "/unused",
                     "method": 7, "limit": 50},
                    {"op": "subscribe", "config_dir": "/unused",
                     "method": "m\x00s", "limit": 50}]:
            with self.subTest(bad=bad):
                with self.assertRaises(bridge.BridgeFailure) as caught:
                    bridge.validate_request(bad)
                self.assertEqual(caught.exception.code, "invalid_request")

    def test_subscribe_rejects_wild_bodies_and_bounds(self):
        bridge = load_bridge()
        bridge.validate_request({"op": "subscribe", "config_dir": "/unused",
                                 "method": "activity.subscribe",
                                 "body": {"capabilities": ["chat.send"]},
                                 "limit": 50})
        for bad in [{"op": "subscribe", "config_dir": "/unused",
                     "method": "activity.subscribe", "limit": 50,
                     "body": {"nested": {"deep": True}}},
                    {"op": "subscribe", "config_dir": "/unused",
                     "method": "activity.subscribe", "limit": 50,
                     "body": {"paths": ["x" * 5000]}},
                    {"op": "subscribe", "config_dir": "/unused",
                     "method": "activity.subscribe", "limit": 0},
                    {"op": "subscribe", "config_dir": "/unused",
                     "limit": 50}]:
            with self.subTest(bad=bad), self.assertRaises(bridge.BridgeFailure):
                bridge.validate_request(bad)

    def test_model_get_reads_the_selected_model_and_its_generation(self):
        # The web UI calls model.get ({}) on load and keeps the newest
        # generation: the bridge returns the scalar selection fields and
        # drops everything else.
        bridge = load_bridge()
        from contextlib import contextmanager
        seen = {}
        class Gateway:
            def call_json(self, method, body=None, **kwargs):
                seen["method"] = method
                seen["body"] = body
                return {"generation": 7, "selected_model": "m1",
                        "default_model": "m0",
                        "available_models": ["m0", "m1"],
                        "display_name": "Muse",
                        "access_token": "SECRET"}
        class Client(FakeClient):
            @contextmanager
            def _gateway(self, config):
                yield Gateway()
        result = bridge.Adapter(Client(), {}).dispatch({"op": "model_get"})
        self.assertEqual(seen["method"], "model.get")
        self.assertEqual(result["generation"], 7)
        self.assertEqual(result["selected_model"], "m1")
        self.assertEqual(result["default_model"], "m0")
        self.assertEqual(result["available_models"], ["m0", "m1"])
        self.assertEqual(result["display_name"], "Muse")
        self.assertNotIn("SECRET", str(result))

    def test_model_set_names_a_model_and_reports_the_new_generation(self):
        # The web UI sends model.set ({model}) and keeps the answer only
        # when its generation is newer: the bridge echoes the service's
        # own generation, selection, and display name.
        bridge = load_bridge()
        from contextlib import contextmanager
        seen = {}
        class Gateway:
            def call_json(self, method, body=None, **kwargs):
                seen["method"] = method
                seen["body"] = body
                return {"generation": 8, "selected_model": "m2",
                        "display_name": "Muse 2"}
        class Client(FakeClient):
            @contextmanager
            def _gateway(self, config):
                yield Gateway()
        result = bridge.Adapter(Client(), {}).dispatch(
            {"op": "model_set", "model": "m2"})
        self.assertEqual(seen["method"], "model.set")
        self.assertEqual(seen["body"], {"model": "m2"})
        self.assertEqual(result["generation"], 8)
        self.assertEqual(result["selected_model"], "m2")
        self.assertEqual(result["display_name"], "Muse 2")

    def test_model_set_rejects_a_missing_or_wild_model_id(self):
        bridge = load_bridge()
        bridge.validate_request({"op": "model_set", "config_dir": "/unused",
                                 "model": "m2"})
        for bad in [{"op": "model_set", "config_dir": "/unused"},
                    {"op": "model_set", "config_dir": "/unused",
                     "model": ""},
                    {"op": "model_set", "config_dir": "/unused",
                     "model": "m" * 257},
                    {"op": "model_set", "config_dir": "/unused",
                     "model": "m\x002"}]:
            with self.subTest(bad=bad), self.assertRaises(bridge.BridgeFailure):
                bridge.validate_request(bad)

    def test_grants_lists_issued_access_with_a_page_cursor(self):
        # The web UI reads grants.list ({include_revoked} + paging) and the
        # Linux Policy page needs the same inventory: typed scalar fields
        # only, the next cursor as the opaque paging token.
        bridge = load_bridge()
        from contextlib import contextmanager
        seen = {}
        class Gateway:
            def call_json(self, method, body=None, **kwargs):
                seen["method"] = method
                seen["body"] = body
                return {"items": [
                    {"grant_id": "g1", "subject_kind": "peer",
                     "subject_id": "p1", "capabilities": ["chat.send"],
                     "revision": 2, "not_before_ms": 100,
                     "expires_at_ms": 200, "revoked_at_ms": None,
                     "provenance": "invite", "created_at_ms": 50,
                     "access_token": "SECRET"},
                ], "nextCursor": "PAGE-2"}
        class Client(FakeClient):
            @contextmanager
            def _gateway(self, config):
                yield Gateway()
        result = bridge.Adapter(Client(), {}).dispatch(
            {"op": "grants", "limit": 50, "include_revoked": False})
        self.assertEqual(seen["method"], "grants.list")
        self.assertEqual(seen["body"],
                         {"limit": 50, "include_revoked": False})
        self.assertEqual(result, {"grants": [
            {"grant_id": "g1", "subject_kind": "peer", "subject_id": "p1",
             "capabilities": ["chat.send"], "revision": 2,
             "not_before_ms": 100, "expires_at_ms": 200,
             "revoked_at_ms": None, "provenance": "invite",
             "created_at_ms": 50},
        ], "next_cursor": "PAGE-2", "has_more": True})
        self.assertNotIn("SECRET", str(result))

    def test_grants_validates_limit_and_cursor(self):
        bridge = load_bridge()
        bridge.validate_request({"op": "grants", "config_dir": "/unused",
                                 "limit": 50})
        for bad in [{"op": "grants", "config_dir": "/unused", "limit": 0},
                    {"op": "grants", "config_dir": "/unused", "limit": True},
                    {"op": "grants", "config_dir": "/unused", "limit": 50,
                     "cursor": ""},
                    {"op": "grants", "config_dir": "/unused", "limit": 50,
                     "include_revoked": "yes"}]:
            with self.subTest(bad=bad), self.assertRaises(bridge.BridgeFailure):
                bridge.validate_request(bad)

    def test_scoped_permissions_reads_one_scope_with_an_echo_check(self):
        # The web UI reads GET /permissions/scopes/{type}/{id} with
        # {scope_type, scope_id, connectors} and rejects a response whose
        # scope echo does not match the request: a mix-up between scopes
        # must never surface as the wrong scope's permissions.
        bridge = load_bridge()
        from contextlib import contextmanager
        seen = {}
        class Gateway:
            def call_json(self, method, body=None, **kwargs):
                seen["method"] = method
                seen["body"] = body
                return {"scope": {"scope_type": "connector",
                                  "scope_id": "gmail"},
                        "action_groups": {
                            "read": {"label": "Read", "category": "mail",
                                     "methods": ["chat.send", 7]},
                        },
                        "access_token": "SECRET"}
        class Client(FakeClient):
            @contextmanager
            def _gateway(self, config):
                yield Gateway()
        result = bridge.Adapter(Client(), {}).dispatch(
            {"op": "scoped_permissions", "scope_type": "connector",
             "scope_id": "gmail", "connectors": ["chat"]})
        self.assertEqual(seen["method"], "egress.permissions.scoped")
        self.assertEqual(seen["body"], {"scope_type": "connector",
                                        "scope_id": "gmail",
                                        "connectors": "chat"})
        self.assertEqual(result, {"scope": {"scope_type": "connector",
                                            "scope_id": "gmail"},
                                  "groups": [{"key": "read", "label": "Read",
                                              "category": "mail",
                                              "methods": ["chat.send"]}]})
        self.assertNotIn("SECRET", str(result))

    def test_scoped_permissions_rejects_a_mismatched_scope_echo(self):
        bridge = load_bridge()
        from contextlib import contextmanager
        class Gateway:
            def call_json(self, method, body=None, **kwargs):
                return {"scope": {"scope_type": "connector",
                                  "scope_id": "other"}}
        class Client(FakeClient):
            @contextmanager
            def _gateway(self, config):
                yield Gateway()
        with self.assertRaises(bridge.BridgeFailure) as caught:
            bridge.Adapter(Client(), {}).dispatch(
                {"op": "scoped_permissions", "scope_type": "connector",
                 "scope_id": "gmail"})
        self.assertEqual(caught.exception.code, "protocol_error")

    def test_scoped_permissions_validates_scope_and_connectors(self):
        bridge = load_bridge()
        bridge.validate_request({"op": "scoped_permissions",
                                 "config_dir": "/unused",
                                 "scope_type": "connector",
                                 "scope_id": "gmail",
                                 "connectors": ["chat"]})
        for bad in [{"op": "scoped_permissions", "config_dir": "/unused",
                     "scope_id": "gmail"},
                    {"op": "scoped_permissions", "config_dir": "/unused",
                     "scope_type": "", "scope_id": "gmail"},
                    {"op": "scoped_permissions", "config_dir": "/unused",
                     "scope_type": "connector", "scope_id": "gmail",
                     "connectors": "chat"},
                    {"op": "scoped_permissions", "config_dir": "/unused",
                     "scope_type": "connector", "scope_id": "gmail",
                     "connectors": [""]},
                    {"op": "scoped_permissions", "config_dir": "/unused",
                     "scope_type": "connector", "scope_id": "gmail",
                     "connectors": ["a"] * 33}]:
            with self.subTest(bad=bad), self.assertRaises(bridge.BridgeFailure):
                bridge.validate_request(bad)

    def test_tasks_run_dispatches_one_run_for_a_listed_schedule(self):
        # The web UI runs a schedule with tasks.run ({job_id}); the answer
        # claims acceptance only, and whether a run appears is proven by
        # re-reading tasks.runs, not by this answer.
        bridge = load_bridge()
        from contextlib import contextmanager
        seen = {}
        class Gateway:
            def call_json(self, method, body=None, **kwargs):
                seen["method"] = method
                seen["kwargs"] = kwargs
                return {"ok": True}
        class Client(FakeClient):
            @contextmanager
            def _gateway(self, config):
                yield Gateway()
        result = bridge.Adapter(Client(), {}).dispatch(
            {"op": "tasks_run", "job_id": "job-1"})
        self.assertEqual(seen["method"], "tasks.run")
        self.assertEqual(seen["kwargs"].get("path_params"), {"job_id": "job-1"})
        self.assertEqual(result, {"job_id": "job-1", "accepted": True})
        bridge.validate_request({"op": "tasks_run", "config_dir": "/unused",
                                 "job_id": "job-1"})
        for bad in [{"op": "tasks_run", "config_dir": "/unused"},
                    {"op": "tasks_run", "config_dir": "/unused", "job_id": ""},
                    {"op": "tasks_run", "config_dir": "/unused",
                     "job_id": "j" * 257}]:
            with self.subTest(bad=bad), self.assertRaises(bridge.BridgeFailure):
                bridge.validate_request(bad)

    def test_scoped_update_writes_one_mode_with_its_revision(self):
        # The write half of the scoped model: one connector+method mode
        # change, guarded by the revision the read published so a stale
        # page cannot overwrite a newer one.
        bridge = load_bridge()
        from contextlib import contextmanager
        seen = {}
        class Gateway:
            def call_json(self, method, body=None, **kwargs):
                seen["method"] = method
                seen["body"] = body
                return {"ok": True}
        class Client(FakeClient):
            @contextmanager
            def _gateway(self, config):
                yield Gateway()
        result = bridge.Adapter(Client(), {}).dispatch(
            {"op": "scoped_update", "scope_type": "connector",
             "scope_id": "gmail", "expected_revision": 7,
             "connector": "gmail", "method": "chat.send", "mode": "ask",
             "display_name": "Gmail"})
        self.assertEqual(seen["method"], "egress.permissions.scoped.update")
        self.assertEqual(seen["body"], {"scope_type": "connector",
                                        "scope_id": "gmail",
                                        "expected_revision": 7,
                                        "connector": "gmail",
                                        "method": "chat.send",
                                        "mode": "ask",
                                        "display_name": "Gmail"})
        self.assertTrue(result["accepted"])
        bridge.validate_request({"op": "scoped_update",
                                 "config_dir": "/unused",
                                 "scope_type": "connector", "scope_id": "gmail",
                                 "expected_revision": 7, "connector": "gmail",
                                 "method": "chat.send", "mode": "ask",
                                 "display_name": "Gmail"})
        for bad in [{"op": "scoped_update", "config_dir": "/unused",
                     "scope_type": "connector", "scope_id": "gmail",
                     "expected_revision": True, "connector": "gmail",
                     "method": "chat.send", "mode": "ask"},
                    {"op": "scoped_update", "config_dir": "/unused",
                     "scope_type": "connector", "scope_id": "gmail",
                     "expected_revision": 7, "connector": "gmail",
                     "method": "chat.send", "mode": "sometimes"}]:
            with self.subTest(bad=bad), self.assertRaises(bridge.BridgeFailure):
                bridge.validate_request(bad)

    def test_chat_delete_removes_one_message_or_turn(self):
        # The web UI deletes with chat.delete_message vs chat.delete_turn;
        # the bridge maps one op onto both, and acceptance is not absence:
        # the caller re-reads history to prove the row is gone.
        bridge = load_bridge()
        from contextlib import contextmanager
        seen = {}
        class Gateway:
            def call_json(self, method, body=None, **kwargs):
                seen.setdefault("methods", []).append(method)
                return {"ok": True}
        class Client(FakeClient):
            @contextmanager
            def _gateway(self, config):
                yield Gateway()
        for target, method in (("message", "chat.delete_message"),
                               ("turn", "chat.delete_turn")):
            seen.clear()
            result = bridge.Adapter(Client(), {}).dispatch(
                {"op": "chat_delete", "target": target,
                 "message_id": "m1"})
            self.assertEqual(seen["methods"], [method])
            self.assertEqual(result["message_id"], "m1")
            self.assertTrue(result["accepted"])
        bridge.validate_request({"op": "chat_delete", "config_dir": "/unused",
                                 "target": "message", "message_id": "m1"})
        for bad in [{"op": "chat_delete", "config_dir": "/unused",
                     "target": "message"},
                    {"op": "chat_delete", "config_dir": "/unused",
                     "target": "thread", "message_id": "m1"},
                    {"op": "chat_delete", "config_dir": "/unused",
                     "target": "message", "message_id": ""}]:
            with self.subTest(bad=bad), self.assertRaises(bridge.BridgeFailure):
                bridge.validate_request(bad)

    def test_message_react_sends_the_probed_toggle_once(self):
        bridge = load_bridge()
        from contextlib import contextmanager
        seen = {}
        class Gateway:
            def call_json(self, method, body=None, **kwargs):
                seen.setdefault("calls", []).append((method, body))
                return {"reactions": [{"emoji": "❤"}]}
        class Client(FakeClient):
            @contextmanager
            def _gateway(self, config):
                yield Gateway()
        result = bridge.Adapter(Client(), {}).dispatch(
            {"op": "message_react", "message_id": "m1", "emoji": "❤"})
        self.assertEqual(seen["calls"], [("reactions.toggle",
                                          {"message_id": "m1", "emoji": "❤"})])
        self.assertEqual(result, {"message_id": "m1", "accepted": True,
                                  "reacted": True})
        bridge.validate_request({"op": "message_react", "config_dir": "/unused",
                                 "message_id": "m1", "emoji": "❤"})
        for bad in [{"op": "message_react", "config_dir": "/unused",
                     "message_id": "", "emoji": "❤"},
                    {"op": "message_react", "config_dir": "/unused",
                     "message_id": "m1", "emoji": "  "},
                    {"op": "message_react", "config_dir": "/unused",
                     "message_id": "m1"},
                    {"op": "message_react", "config_dir": "/unused",
                     "message_id": "m1", "emoji": "❤", "extra": 1}]:
            with self.subTest(bad=bad), self.assertRaises(bridge.BridgeFailure):
                bridge.validate_request(bad)

    def test_send_verifies_the_expected_session_before_dispatching(self):
        # The preflight rides inside the dispatch request, so it must fail before
        # anything reaches the service when the account changed underneath.
        bridge = load_bridge()
        class Other(FakeClient):
            def history(self, **kwargs):
                raise AssertionError("no history read for a changed account")
            def send(self, **kwargs):
                raise AssertionError("must not dispatch for another account")
        with self.assertRaises(bridge.BridgeFailure) as caught:
            bridge.Adapter(Other(), {}).dispatch({
                "op": "send", "message": "hello",
                "expect_vm_id": "someone-else", "expect_identity_name": "Test User"})
        self.assertEqual(caught.exception.code, "account_changed")
        self.assertFalse(caught.exception.uncertain)
        class Renamed(FakeClient):
            def send(self, **kwargs):
                raise AssertionError("must not dispatch for another identity")
        with self.assertRaises(bridge.BridgeFailure) as caught:
            bridge.Adapter(Renamed(), {}).dispatch({
                "op": "send", "message": "hello",
                "expect_vm_id": "vm-1", "expect_identity_name": "Other User"})
        self.assertEqual(caught.exception.code, "account_changed")
        class Matching(FakeClient):
            def __init__(self):
                self.calls = 0
            def history(self, **kwargs):
                self.calls += 1
                if self.calls == 1:
                    return []
                return [{"message_id": "m1", "seq": 1, "role": "user", "text": "hello"}]
            def send(self, **kwargs):
                return {"ok": True, "sent": True}
        result = bridge.Adapter(Matching(), {}, confirmation_window=0).dispatch({
            "op": "send", "message": "hello",
            "expect_vm_id": "vm-1", "expect_identity_name": "Test User"})
        self.assertTrue(result["confirmed"])
        class Padded(FakeClient):
            # The service's stored display text is not byte-identical to what
            # was typed (a trailing newline here); confirmation must still
            # land, or every healthy send reports itself unconfirmed.
            def __init__(self):
                self.calls = 0
            def history(self, **kwargs):
                self.calls += 1
                if self.calls == 1:
                    return []
                return [{"message_id": "m1", "seq": 1, "role": "user", "text": "hello\n"}]
            def send(self, **kwargs):
                return {"ok": True, "sent": True}
        result = bridge.Adapter(Padded(), {}, confirmation_window=0).dispatch(
            {"op": "send", "message": "hello"})
        self.assertTrue(result["confirmed"])
        for bad in [{"expect_vm_id": ""}, {"expect_vm_id": 3},
                    {"expect_identity_name": ""}, {"expect_identity_name": "x" * 257}]:
            with self.subTest(bad=bad), self.assertRaises(bridge.BridgeFailure):
                bridge.validate_request(dict(
                    {"op": "send", "message": "hi", "config_dir": "/unused"}, **bad))

    def test_identity_verifies_the_account_without_listing_conversations(self):
        # A send preflight must not pay for `sessions.list`: the service can take
        # ten seconds to answer it, and that delay used to sit in front of every
        # dispatch.
        bridge = load_bridge()
        class Client(FakeClient):
            def threads(self, include_archived=False):
                raise AssertionError("identity must not list conversations")
            def history(self, **kwargs):
                raise AssertionError("identity must not read history")
        result = bridge.Adapter(Client(), {}).dispatch({"op": "identity"})
        self.assertEqual(result, {"identity_name": "Test User", "vm_id": "vm-1"})
        # Only the two projection fields, never a credential-bearing one.
        self.assertEqual(set(result), {"identity_name", "vm_id"})
        bridge.validate_request({"op": "identity", "config_dir": "/unused"})
        with self.assertRaises(bridge.BridgeFailure):
            bridge.validate_request({"op": "identity", "config_dir": "/unused", "limit": 1})

    def test_identity_reads_one_route_instead_of_the_aggregate_status(self):
        # A preflight must not pay for `sessions.list`, `chat.unread_count` and
        # `vm.health`: only the identity and the resolved VM are consulted.
        from contextlib import contextmanager
        bridge = load_bridge()
        calls = []
        class Gateway:
            vm_id = "vm-9"
            def call_json(self, method):
                calls.append(method)
                return {"name": "Test User", "access_token": "SECRET"}
        class Client(FakeClient):
            def status(self):
                raise AssertionError("identity must not aggregate status")
            @contextmanager
            def _gateway(self, config):
                yield Gateway()
        result = bridge.Adapter(Client(), {}).dispatch({"op": "identity"})
        self.assertEqual(result, {"identity_name": "Test User", "vm_id": "vm-9"})
        self.assertEqual(calls, ["identity"])
        self.assertNotIn("SECRET", str(result))

    def test_identity_requires_an_authenticated_session_and_a_vm_id(self):
        bridge = load_bridge()
        class SignedOut(FakeClient):
            def status(self):
                return {"ok": True, "authenticated": False, "vm_id": "vm-1"}
        class NoVm(FakeClient):
            def status(self):
                return {"ok": True, "authenticated": True, "vm_id": ""}
            def threads(self, include_archived=False):
                raise AssertionError("a rejected identity must not list conversations")
        with self.assertRaises(bridge.BridgeFailure) as caught:
            bridge.Adapter(SignedOut(), {}).dispatch({"op": "identity"})
        self.assertEqual(caught.exception.code, "auth_required")
        with self.assertRaises(bridge.BridgeFailure) as caught:
            bridge.Adapter(NoVm(), {}).dispatch({"op": "identity"})
        self.assertEqual(caught.exception.code, "protocol_error")

    def test_history_keeps_latest_message_update_preserves_role_and_scope(self):
        bridge = load_bridge()
        class Client(FakeClient):
            def history(self, **kwargs):
                self.request = kwargs
                return [
                    {"message_id": "b", "seq": 7, "role": "assistant", "text": "done", "timestamp": 123, "raw": {"payload": {"status": "complete"}}},
                    {"message_id": "a", "seq": 3, "role": "user", "text": "hello"},
                    {"message_id": "b", "seq": 4, "role": "assistant", "text": "partial"},
                    {"message_id": None, "seq": 8, "role": "typing", "text": ""},
                ]
        client = Client()
        result = bridge.Adapter(client, {}).dispatch({"op": "history", "thread": " exact-id ", "limit": 20})
        self.assertEqual(client.request, {"thread": " exact-id ", "limit": 20})
        self.assertEqual([r["message_id"] for r in result], ["a", "b"])
        self.assertEqual(result[1], {"message_id": "b", "seq": 7, "role": "assistant",
            "text": "done", "timestamp": "123", "status": "complete"})

    def test_tasks_uses_only_read_routes_and_projects_types(self):
        from contextlib import contextmanager
        bridge = load_bridge()
        calls = []
        class Gateway:
            def call_json(self, method, timeout=None):
                calls.append(method)
                if method == "tasks.list":
                    return {"schedules": [{"id": "job-1", "title": "Daily", "enabled": True,
                        "timezone": "UTC", "next_run_at_utc": 123, "secret": "SECRET"}], "invalid": []}
                if method == "tasks.runs":
                    return {"runs": [{"run_id": "run-1", "job_id": "job-1", "status": "success",
                        "result_summary": "Done", "error_text": None, "scheduled_for_utc": 120, "raw": "SECRET"}]}
                if method == "subagents.list":
                    return {"active": [], "recent": [{"prompt": "Sweep", "status": "completed",
                        "created_at": 100, "completed_at": 200, "final_response": "Swept."}]}
                raise AssertionError("nonallowlisted route")
        class Client(FakeClient):
            @contextmanager
            def _gateway(self, config):
                yield Gateway()
        result = bridge.Adapter(Client(), {}).dispatch({"op": "tasks"})
        self.assertEqual(calls, ["tasks.list", "tasks.runs", "subagents.list"])
        self.assertEqual(result, {"schedules": [{"id": "job-1", "title": "Daily", "enabled": True,
            "timezone": "UTC", "next_run_at_utc": 123}], "runs": [{"run_id": "run-1", "job_id": "job-1",
            "status": "success", "result_summary": "Done", "error_text": None, "scheduled_for_utc": 120}],
            "agents": [{"prompt": "Sweep", "status": "completed",
            "created_at": "1970-01-01T00:01:40+00:00",
            "completed_at": "1970-01-01T00:03:20+00:00",
            "final_response": "Swept."}]})

    def test_artifacts_reads_one_route_and_projects_only_actionable_fields(self):
        from contextlib import contextmanager
        bridge = load_bridge()
        calls = []
        class Gateway:
            def call_json(self, method, **kwargs):
                calls.append(method)
                return {"total_count": 2, "edges": [
                    {"cursor": "SECRET-CURSOR", "node": {"artifact_ref": "file:workspace/a.docx",
                        "display_name": "a.docx", "type": "document", "format": "docx",
                        "status": "ready", "placement": "library", "updated_at_ms": 5,
                        "is_pinned": False, "open_target": {"kind": "file", "path": "/workspace/a.docx"},
                        "capabilities": {"can_open": True, "can_share": False}, "raw": "SECRET"}},
                    {"cursor": "PAGE-2", "node": {"artifact_ref": "file:workspace/b.pdf", "display_name": "b.pdf",
                        "type": "document", "format": "pdf", "status": "ready",
                        "placement": "library", "updated_at_ms": 7, "is_pinned": True,
                        "open_target": {"kind": "file", "path": "/workspace/b.pdf"},
                        "capabilities": {"can_open": True, "can_share": True}}},
                ], "page_info": {"has_next_page": True, "start_cursor": "SECRET-CURSOR"}}
        class Client(FakeClient):
            @contextmanager
            def _gateway(self, config):
                yield Gateway()
        result = bridge.Adapter(Client(), {}).dispatch({"op": "artifacts"})
        self.assertEqual(calls, ["artifacts.list"])
        self.assertEqual(result["total_count"], 2)
        self.assertTrue(result["has_more"])
        self.assertEqual(set(result["artifacts"][0]), set(bridge.ARTIFACT_FIELDS))
        self.assertEqual(result["artifacts"][1]["pinned"], True)
        self.assertEqual(result["artifacts"][1]["can_share"], True)
        self.assertEqual(result["artifacts"][0]["path"], "/workspace/a.docx")
        # Only the page's own last cursor travels, as the opaque
        # token the next page request hands back; per-edge cursors,
        # the start cursor and every unlisted node field stay here.
        self.assertEqual(result["next_cursor"], "PAGE-2")
        self.assertNotIn("SECRET", str(result))
        bridge.validate_request({"op": "artifacts", "config_dir": "/unused"})
        with self.assertRaises(bridge.BridgeFailure):
            bridge.validate_request({"op": "artifacts", "config_dir": "/unused", "limit": 1})

    def test_artifacts_paging_proves_continuation_before_accepting_a_page(self):
        # The route's paging parameter is recorded nowhere, so the
        # bridge probes it with a proof: a candidate page counts
        # only when its rows continue the accumulated list.
        from contextlib import contextmanager
        bridge = load_bridge()
        calls = []
        def page_of(refs, cursor):
            return {"total_count": 3, "edges": [
                {"cursor": cursor, "node": {"artifact_ref": ref,
                    "display_name": ref, "open_target": {"kind": "file", "path": "/w"},
                    "capabilities": {}}}
                for ref in refs], "page_info": {"has_next_page": False}}
        class Gateway:
            def call_json(self, method, **kwargs):
                calls.append((method, kwargs))
                query = kwargs.get("query") or {}
                if "after" in query:
                    # The service ignores this candidate and
                    # answers with the first page again.
                    return page_of(["file:workspace/a"], "c1")
                return page_of(["file:workspace/b", "file:workspace/c"], "c2")
        class Client(FakeClient):
            @contextmanager
            def _gateway(self, config):
                yield Gateway()
        result = bridge.Adapter(Client(), {}).dispatch(
            {"op": "artifacts", "cursor": "c1", "seen_refs": ["file:workspace/a"]})
        self.assertEqual([kwargs.get("query") for _, kwargs in calls],
                         [{"after": "c1"}, {"cursor": "c1"}])
        self.assertEqual(result["paging"], "continued")
        self.assertEqual([row["artifact_ref"] for row in result["artifacts"]],
                         ["file:workspace/b", "file:workspace/c"])

    def test_artifacts_paging_reports_unavailable_when_nothing_continues(self):
        from contextlib import contextmanager
        bridge = load_bridge()
        def page_of(refs):
            return {"total_count": 3, "edges": [
                {"cursor": "c1", "node": {"artifact_ref": ref,
                    "display_name": ref, "open_target": {"kind": "file", "path": "/w"},
                    "capabilities": {}}}
                for ref in refs], "page_info": {"has_next_page": True}}
        class Gateway:
            def call_json(self, method, **kwargs):
                return page_of(["file:workspace/a"])
        class Client(FakeClient):
            @contextmanager
            def _gateway(self, config):
                yield Gateway()
        result = bridge.Adapter(Client(), {}).dispatch(
            {"op": "artifacts", "cursor": "c1", "seen_refs": ["file:workspace/a"]})
        self.assertEqual(result["paging"], "unavailable")
        self.assertEqual(result["artifacts"], [])
        self.assertEqual(result["total_count"], 3)
        self.assertFalse(result["has_more"])

    def test_space_unshare_dispatches_one_path_only_delete(self):
        from contextlib import contextmanager
        bridge = load_bridge()
        calls = []
        class Gateway:
            def call_json(self, method, **kwargs):
                calls.append((method, kwargs))
                return {"whatever": "the answer shape is unrecorded"}
        class Client(FakeClient):
            @contextmanager
            def _gateway(self, config):
                yield Gateway()
        result = bridge.Adapter(Client(), {}).dispatch(
            {"op": "space_unshare", "slug": "my-space"})
        self.assertEqual(calls, [("spaces.unshare", {"path_params": {"slug": "my-space"}})])
        self.assertEqual(result, {"slug": "my-space", "accepted": True})

    def test_space_share_sends_the_probed_publish_once(self):
        from contextlib import contextmanager
        bridge = load_bridge()
        calls = []
        class Gateway:
            def call_json(self, method, **kwargs):
                calls.append((method, kwargs))
                return {"ok": True, "url": "https://example.invalid/s/1/abc",
                        "share_type": "cloudflare"}
        class Client(FakeClient):
            @contextmanager
            def _gateway(self, config):
                yield Gateway()
        result = bridge.Adapter(Client(), {}).dispatch(
            {"op": "space_share", "slug": "my-space",
             "share_type": "cloudflare", "disclosure_confirmation": None})
        self.assertEqual(
            calls,
            [("spaces.share",
              {"body": {"share_type": "cloudflare",
                        "require_disclosure_confirmation": True},
               "path_params": {"slug": "my-space"}, "timeout": 20})])
        self.assertEqual(result["slug"], "my-space")
        self.assertTrue(result["accepted"])
        self.assertTrue(result["url"].startswith("https://"))
        self.assertFalse(result["needs_disclosure"])

    def test_space_share_shapes_the_disclosure_round_trip(self):
        from contextlib import contextmanager
        bridge = load_bridge()
        token = "a" * 64
        seen = {}
        class Gateway:
            def call_json(self, method, **kwargs):
                seen["body"] = kwargs.get("body")
                return {"ok": True,
                        "disclosure_confirmation": {
                            "token": token, "summary": "What becomes public",
                            "disclosure": {"contains_chat_history": True,
                                           "something_new": True}}}
        class Client(FakeClient):
            @contextmanager
            def _gateway(self, config):
                yield Gateway()
        result = bridge.Adapter(Client(), {}).dispatch(
            {"op": "space_share", "slug": "my-space",
             "share_type": "cloudflare", "disclosure_confirmation": None})
        self.assertFalse(result["accepted"] and result["url"])
        self.assertTrue(result["needs_disclosure"])
        self.assertEqual(result["disclosure_flags"], ["contains_chat_history"])
        self.assertEqual(result["disclosure_confirmation"]["token"], token)
        confirmed = bridge.Adapter(Client(), {}).dispatch(
            {"op": "space_share", "slug": "my-space",
             "share_type": "cloudflare",
             "disclosure_confirmation": result["disclosure_confirmation"]})
        self.assertEqual(seen["body"]["disclosure_confirmation"]["token"], token)
        self.assertTrue(confirmed["needs_disclosure"])

    def test_space_share_requests_are_validated(self):
        bridge = load_bridge()
        bridge.validate_request({"op": "space_share", "slug": "ok-slug",
                                 "share_type": "cloudflare",
                                 "disclosure_confirmation": None,
                                 "config_dir": "/tmp/x"})
        token = "b" * 64
        bridge.validate_request({"op": "space_share", "slug": "ok-slug",
                                 "share_type": "cloudflare",
                                 "disclosure_confirmation": {"token": token},
                                 "config_dir": "/tmp/x"})
        for bad in ({"slug": "", "share_type": "cloudflare",
                     "disclosure_confirmation": None},
                    {"slug": "a/b", "share_type": "cloudflare",
                     "disclosure_confirmation": None},
                    {"slug": "ok", "share_type": "  ",
                     "disclosure_confirmation": None},
                    {"slug": "ok", "share_type": "cloudflare",
                     "disclosure_confirmation": {"token": "short"}},
                    {"slug": "ok", "share_type": "cloudflare",
                     "disclosure_confirmation": {"token": "z" * 64}}):
            with self.subTest(bad=bad):
                with self.assertRaises(bridge.BridgeFailure):
                    request = dict(bad, op="space_share", config_dir="/tmp/x")
                    bridge.validate_request(request)

    def test_space_unshare_and_paging_requests_are_validated(self):
        bridge = load_bridge()
        bridge.validate_request({"op": "space_unshare", "slug": "ok-slug",
                                   "config_dir": "/tmp/x"})
        for bad in ({}, {"slug": ""}, {"slug": "a/b"}, {"slug": "x" * 257}):
            with self.assertRaises(bridge.BridgeFailure):
                bridge.validate_request(dict(bad, op="space_unshare",
                                               config_dir="/tmp/x"))
        bridge.validate_request({"op": "artifacts", "cursor": "c1",
                                   "seen_refs": ["file:workspace/a"],
                                   "config_dir": "/tmp/x"})
        for bad in ({"cursor": ""}, {"cursor": 7}, {"cursor": "c", "seen_refs": "nope"},
                    {"cursor": "c", "seen_refs": ["x"] * 501}):
            with self.assertRaises(bridge.BridgeFailure):
                bridge.validate_request(dict(bad, op="artifacts",
                                               config_dir="/tmp/x"))

    def _export_dispatch(self, bridge, body):
        from contextlib import contextmanager
        calls = []
        class Gateway:
            def request(self, method, **kwargs):
                calls.append((method, kwargs))
                return body
        class Client(FakeClient):
            @contextmanager
            def _gateway(self, config):
                yield Gateway()
        result = bridge.Adapter(Client(), {}).dispatch(
            {"op": "space_export", "slug": "my-space"})
        return calls, result

    def test_space_export_returns_a_small_html_export_verbatim(self):
        bridge = load_bridge()
        calls, result = self._export_dispatch(
            bridge, b"<!DOCTYPE html><html><body>hi</body></html>")
        self.assertEqual(calls, [("spaces.export_html",
                                  {"path_params": {"slug": "my-space"},
                                   "timeout": 25})])
        self.assertEqual(result, {
            "slug": "my-space", "format": "html",
            "size": len(b"<!DOCTYPE html><html><body>hi</body></html>"),
            "truncated": False,
            "html": "<!DOCTYPE html><html><body>hi</body></html>"})

    def test_space_export_reports_binary_and_plain_text_without_a_page(self):
        bridge = load_bridge()
        _, binary = self._export_dispatch(bridge, b"\x89PNG\xff\xfe binary")
        self.assertEqual(binary["format"], "binary")
        self.assertIsNone(binary["html"])
        self.assertFalse(binary["truncated"])
        _, text = self._export_dispatch(bridge, b"just some words")
        self.assertEqual(text["format"], "text")
        self.assertEqual(text["html"], "just some words")

    def test_space_export_truncates_an_oversized_export(self):
        bridge = load_bridge()
        body = b"<" + b"a" * (600 * 1024)
        _, result = self._export_dispatch(bridge, body)
        self.assertEqual(result["size"], len(body))
        self.assertEqual(result["format"], "html")
        self.assertTrue(result["truncated"])
        self.assertIsNone(result["html"])

    def test_space_export_requests_are_validated(self):
        bridge = load_bridge()
        bridge.validate_request({"op": "space_export", "slug": "ok-slug",
                                   "config_dir": "/tmp/x"})
        for bad in ({}, {"slug": ""}, {"slug": "a/b"}, {"slug": "x" * 257}):
            with self.assertRaises(bridge.BridgeFailure):
                bridge.validate_request(dict(bad, op="space_export",
                                               config_dir="/tmp/x"))

    def test_search_probes_the_parameter_and_projects_openable_rows(self):
        # The route is recorded; its parameter's name is not. The
        # first candidate is refused, the second answers, and only
        # rows carrying a conversation survive the projection.
        from contextlib import contextmanager
        bridge = load_bridge()
        calls = []
        class Gateway:
            def call_json(self, method, **kwargs):
                calls.append((method, kwargs))
                if (kwargs.get("query") or {}).get("q"):
                    raise RuntimeError("unknown parameter")
                return {"results": [
                    {"session_id": "thread-1", "title": "River plans",
                     "snippet": "the otters return in May"},
                    {"title": "No conversation id", "snippet": "unopenable"},
                    {"session_id": "thread-2", "title": "River again",
                     "snippet": "more otters"},
                ]}
        class Client(FakeClient):
            @contextmanager
            def _gateway(self, config):
                yield Gateway()
        result = bridge.Adapter(Client(), {}).dispatch(
            {"op": "search", "query": "otters"})
        self.assertEqual([kwargs.get("query") for _, kwargs in calls],
                         [{"q": "otters"}, {"query": "otters"}])
        self.assertEqual([item["id"] for item in result["items"]],
                         ["thread-1", "thread-2"])
        self.assertEqual(result["items"][0]["title"], "River plans")
        self.assertEqual(result["items"][0]["body"], "the otters return in May")
        self.assertEqual(result["recognized"], 2)

    def test_search_surfaces_the_refusal_when_no_parameter_answers(self):
        from contextlib import contextmanager
        bridge = load_bridge()
        class Gateway:
            def call_json(self, method, **kwargs):
                raise RuntimeError("no such parameter")
        class Client(FakeClient):
            @contextmanager
            def _gateway(self, config):
                yield Gateway()
        # The refusal surfaces through the bridge's ordinary
        # failure classification, like any gateway error.
        with self.assertRaises(bridge.BridgeFailure):
            bridge.Adapter(Client(), {}).dispatch({"op": "search", "query": "otters"})

    def test_search_requests_are_validated(self):
        bridge = load_bridge()
        bridge.validate_request({"op": "search", "query": "otters",
                                   "config_dir": "/tmp/x"})
        for bad in ({}, {"query": ""}, {"query": "x" * 513}, {"query": 7}):
            with self.assertRaises(bridge.BridgeFailure):
                bridge.validate_request(dict(bad, op="search", config_dir="/tmp/x"))

    def test_artifacts_fails_closed_on_an_unidentified_row(self):
        from contextlib import contextmanager
        bridge = load_bridge()
        class Gateway:
            def call_json(self, method, **kwargs):
                return {"total_count": 1, "edges": [{"node": {"display_name": "unnamed.docx"}}]}
        class Client(FakeClient):
            @contextmanager
            def _gateway(self, config):
                yield Gateway()
        with self.assertRaises(bridge.BridgeFailure) as caught:
            bridge.Adapter(Client(), {}).dispatch({"op": "artifacts"})
        self.assertEqual(caught.exception.code, "protocol_error")
        # A rejected row must not become an empty page: an unnamed artifact is
        # missing, not absent from the Library.
        for malformed in [None, [], {"total_count": True, "edges": []},
                          {"total_count": 0, "edges": "nope"},
                          {"total_count": 1, "edges": [{"cursor": "c"}]}]:
            with self.assertRaises(bridge.BridgeFailure) as caught:
                bridge.artifact_rows(malformed)
            self.assertEqual(caught.exception.code, "protocol_error")

    def test_send_confirms_only_new_exact_user_prompt_in_same_scope(self):
        bridge = load_bridge()
        class Client(FakeClient):
            def __init__(self):
                self.sent = []
                self.reads = []
            def history(self, **kwargs):
                self.reads.append(kwargs)
                rows = [{"message_id": "old", "seq": 10, "role": "user", "text": "hello"}]
                if self.sent:
                    rows += [{"message_id": "new", "seq": 11, "role": "user", "text": "hello"}]
                return rows
            def send(self, **kwargs):
                self.sent.append(kwargs)
                return {"ok": True, "sent": True, "acknowledged": False}
        client = Client()
        result = bridge.Adapter(client, {}, confirmation_window=0).dispatch(
            {"op": "send", "thread": "scope-1", "message": "hello"})
        self.assertEqual(result, {"sent": True, "confirmed": True, "message_id": "new"})
        self.assertEqual(client.sent, [{"message": "hello", "thread": "scope-1", "fire_and_forget": True, "wait_seconds": 0}])
        self.assertTrue(all(r["thread"] == "scope-1" for r in client.reads))

    def test_send_failure_after_dispatch_is_uncertain_and_never_retried(self):
        bridge = load_bridge()
        class Client(FakeClient):
            sends = 0
            def history(self, **kwargs):
                if self.sends:
                    return {"ok": False, "error": {"code": "AUTH_REQUIRED", "message": "SECRET"}}
                return []
            def send(self, **kwargs):
                self.sends += 1
                return {"ok": True, "sent": True}
        client = Client()
        with self.assertRaises(bridge.BridgeFailure) as caught:
            bridge.Adapter(client, {}, confirmation_window=0).dispatch({"op": "send", "message": "hello"})
        self.assertEqual(caught.exception.code, "auth_required")
        self.assertTrue(caught.exception.uncertain)
        self.assertEqual(client.sends, 1)
        self.assertNotIn("SECRET", str(caught.exception))

    def test_secure_config_pins_private_cookie_file_without_writing_settings(self):
        import json, os, tempfile
        bridge = load_bridge()
        with tempfile.TemporaryDirectory(dir=os.environ.get("TMPDIR") or pathlib.Path.home()) as tmp:
            directory = pathlib.Path(tmp)
            jar = directory / "cookies.txt"
            jar.write_text("synthetic-cookie")
            jar.chmod(0o600)
            (directory / "config.json").write_text(json.dumps({"node_id": "local", "aliases": {"bad": "other"}}))
            (directory / "config.json").chmod(0o600)
            before = (directory / "config.json").read_bytes()
            with bridge.secure_config(directory) as config:
                jar.unlink()
                jar.write_text("replaced")
                with open(config["cookies_file"]) as pinned:
                    self.assertEqual(pinned.read(), "synthetic-cookie")
                self.assertEqual(config["node_id"], "local")
                self.assertNotIn("aliases", config)
            self.assertEqual((directory / "config.json").read_bytes(), before)

    def test_invalid_requests_never_load_community_client(self):
        bridge = load_bridge()
        calls = []
        def factory(config):
            calls.append(config)
            raise AssertionError("network client must not be created")
        for bad in [
            {"op": "raw", "method": "node.register"},
            {"op": "history", "limit": 0},
            {"op": "history", "limit": True},
            {"op": "history", "limit": 201},
            {"op": "history", "limit": 1, "thread": "  "},
            {"op": "send", "message": "  "},
            {"op": "connect", "raw": True},
            {"op": "tasks", "message": "unneeded"},
        ]:
            bad["config_dir"] = "/nonexistent"
            with self.subTest(bad=bad):
                result = bridge.handle_request(bad, client_factory=factory)
                self.assertEqual(result, {"ok": False, "error": {"code": "invalid_request", "uncertain": False}})
        self.assertEqual(calls, [])

    def test_sidecar_protocol_rejects_malformed_or_oversized_stdin_safely(self):
        import json, subprocess, sys
        for data in [b'{SECRET', b'x' * (1024 * 1024 + 1)]:
            with self.subTest(size=len(data)):
                result = subprocess.run([sys.executable, "-I", str(SCRIPT)], input=data,
                    capture_output=True, timeout=5)
                self.assertEqual(result.returncode, 0)
                self.assertEqual(result.stderr, b"")
                self.assertEqual(json.loads(result.stdout), {"ok": False,
                    "error": {"code": "invalid_request", "uncertain": False}})
                self.assertNotIn(b'SECRET', result.stdout)

    def test_task_error_text_never_exposes_credential_shaped_text(self):
        from contextlib import contextmanager
        bridge = load_bridge()
        class Gateway:
            def call_json(self, method):
                if method == "tasks.list":
                    return {"schedules": []}
                return {"runs": [{"run_id": "r", "job_id": "j", "status": "failed",
                    "error_text": 'transport Cookie: hatch_sess=SECRET; token=SECRET Bearer SECRET',
                    "result_summary": 'access_token=SECRET'}]}
        class Client(FakeClient):
            @contextmanager
            def _gateway(self, config):
                yield Gateway()
        result = bridge.Adapter(Client(), {}).dispatch({"op": "tasks"})
        self.assertNotIn("SECRET", str(result))
        self.assertIn("REDACTED", result["runs"][0]["error_text"])

    def test_malformed_upstream_history_is_a_safe_protocol_error(self):
        bridge = load_bridge()
        class Client(FakeClient):
            def history(self, **kwargs):
                return ["SECRET"]
        with self.assertRaises(bridge.BridgeFailure) as caught:
            bridge.Adapter(Client(), {}).dispatch({"op": "history", "limit": 20})
        self.assertEqual(caught.exception.code, "protocol_error")
        self.assertNotIn("SECRET", str(caught.exception))

    def test_connect_requires_an_authenticated_vm(self):
        bridge = load_bridge()
        cases = [({"ok": True, "authenticated": False, "vm_id": "v", "identity": {}}, "auth_required"),
                 ({"ok": True, "authenticated": True, "vm_id": "", "identity": {}}, "protocol_error")]
        for status, code in cases:
            class Client(FakeClient):
                def status(self):
                    return status
            with self.subTest(code=code):
                with self.assertRaises(bridge.BridgeFailure) as caught:
                    bridge.Adapter(Client(), {}).dispatch({"op": "connect"})
                self.assertEqual(caught.exception.code, code)

    def test_permissions_and_symlinks_are_rejected_before_client_creation(self):
        import os, tempfile
        bridge = load_bridge()
        for case in ("directory_mode", "cookie_mode", "cookie_symlink", "config_symlink", "parent_symlink", "config_mode"):
            with self.subTest(case=case), tempfile.TemporaryDirectory(dir=os.environ.get("TMPDIR") or pathlib.Path.home()) as tmp:
                root = pathlib.Path(tmp)
                directory = root / "private"
                directory.mkdir(mode=0o700)
                jar = directory / "cookies.txt"
                jar.write_text("synthetic-cookie")
                jar.chmod(0o600)
                settings = directory / "config.json"
                settings.write_text("{}")
                settings.chmod(0o644)
                chosen = directory
                if case == "directory_mode": directory.chmod(0o755)
                if case == "cookie_mode": jar.chmod(0o644)
                if case == "config_mode": settings.chmod(0o666)
                if case == "cookie_symlink":
                    jar.rename(directory / "real-cookie")
                    jar.symlink_to(directory / "real-cookie")
                if case == "config_symlink":
                    settings.rename(directory / "real-config")
                    settings.symlink_to(directory / "real-config")
                if case == "parent_symlink":
                    chosen = root / "link"
                    chosen.symlink_to(directory, target_is_directory=True)
                result = bridge.handle_request({"op": "connect", "config_dir": str(chosen)},
                    client_factory=lambda _: self.fail("must not create network client"))
                self.assertEqual(result["error"]["code"], "insecure_config")

    def test_missing_cookie_is_auth_required_and_does_not_create_files(self):
        import os, tempfile
        bridge = load_bridge()
        with tempfile.TemporaryDirectory(dir=os.environ.get("TMPDIR") or pathlib.Path.home()) as tmp:
            result = bridge.handle_request({"op": "connect", "config_dir": tmp},
                client_factory=lambda _: self.fail("must not create client"))
            self.assertEqual(result["error"]["code"], "auth_required")
            self.assertEqual(list(pathlib.Path(tmp).iterdir()), [])

    def test_send_unconfirmed_for_old_prompt_assistant_echo_or_changed_text(self):
        bridge = load_bridge()
        for candidate in [
            {"message_id": "old", "seq": 10, "role": "user", "text": "hello"},
            {"message_id": "new", "seq": 11, "role": "assistant", "text": "hello"},
            {"message_id": "new", "seq": 11, "role": "user", "text": "hello!"},
            {"message_id": "old", "seq": 11, "role": "user", "text": "hello"},
        ]:
            class Client(FakeClient):
                sends = 0
                def history(self, **kwargs):
                    if self.sends: return [candidate]
                    return [{"message_id": "old", "seq": 10, "role": "user", "text": "hello"}]
                def send(self, **kwargs):
                    self.sends += 1
                    return {"ok": True, "sent": True, "acknowledged": True}
            client = Client()
            result = bridge.Adapter(client, {}, confirmation_window=0).dispatch({"op": "send", "message": "hello"})
            self.assertEqual(result, {"sent": True, "confirmed": False, "message_id": None})
            self.assertEqual(client.sends, 1)

    def test_send_confirmation_waits_by_clock_not_by_read_count(self):
        # Regression: once confirmation reads got cheap (~0.1s instead of ~1.6s),
        # a fixed attempt count gave up a second after dispatch and reported an
        # uncertain delivery while the service was still persisting the prompt.
        bridge = load_bridge()
        class Client(FakeClient):
            def __init__(self):
                self.reads = 0
            def history(self, **kwargs):
                self.reads += 1
                rows = [{"message_id": "old", "seq": 1, "role": "user", "text": "hi"}]
                if self.reads >= 6:
                    rows.append({"message_id": "new", "seq": 2, "role": "user", "text": "hello"})
                return rows
            def send(self, **kwargs):
                return {"ok": True, "sent": True}
        client = Client()
        result = bridge.Adapter(client, {}, confirmation_window=5.0).dispatch(
            {"op": "send", "message": "hello"})
        self.assertEqual(result, {"sent": True, "confirmed": True, "message_id": "new"})
        self.assertEqual(client.reads, 6)

    def test_send_does_not_dispatch_if_baseline_fails(self):
        bridge = load_bridge()
        class Client(FakeClient):
            def history(self, **kwargs):
                return {"ok": False, "error": {"code": "VM_UNAVAILABLE", "message": "SECRET"}}
            def send(self, **kwargs):
                raise AssertionError("must not dispatch")
        with self.assertRaises(bridge.BridgeFailure) as caught:
            bridge.Adapter(Client(), {}).dispatch({"op": "send", "message": "hello"})
        self.assertEqual(caught.exception.code, "vm_unavailable")
        self.assertFalse(caught.exception.uncertain)

    def test_exceptions_and_structured_auth_vm_timeout_errors_are_safe(self):
        bridge = load_bridge()
        for code in ("AUTH_REQUIRED", "VM_UNAVAILABLE", "TIMEOUT"):
            class Client(FakeClient):
                def status(self):
                    return {"ok": False, "error": {"code": code, "message": "SECRET"}}
            with self.assertRaises(bridge.BridgeFailure) as caught:
                bridge.Adapter(Client(), {}).dispatch({"op": "connect"})
            self.assertEqual(caught.exception.code, code.lower())
            self.assertNotIn("SECRET", str(caught.exception))
        error = bridge.classify_exception(RuntimeError("Bearer SECRET; cookie=SECRET"))
        self.assertEqual(str(error), "bridge_failure")

    def test_http_only_netscape_cookie_is_compatible_with_external_loader(self):
        import os, tempfile
        bridge = load_bridge()
        with tempfile.TemporaryDirectory(dir=os.environ.get("TMPDIR") or pathlib.Path.home()) as tmp:
            jar = pathlib.Path(tmp) / "cookies.txt"
            source = b"# Netscape HTTP Cookie File\n#HttpOnly_muse.ai\tFALSE\t/\tTRUE\t0\thatch_sess\tsynthetic-only\n"
            jar.write_bytes(source)
            jar.chmod(0o600)
            with bridge.secure_config(pathlib.Path(tmp)) as config:
                with open(config["cookies_file"], "rb") as pinned:
                    data = pinned.read()
                self.assertIn(b"muse.ai\tFALSE", data)
                self.assertNotIn(b"#HttpOnly_", data)
            self.assertEqual(jar.read_bytes(), source)

    def test_send_requires_a_positive_dispatch_receipt_before_confirming(self):
        bridge = load_bridge()
        class Client(FakeClient):
            sends = 0
            def history(self, **kwargs):
                return [] if not self.sends else [{"message_id": "n", "seq": 1, "role": "user", "text": "hello"}]
            def send(self, **kwargs):
                self.sends += 1
                return {"ok": True, "sent": False}
        client = Client()
        with self.assertRaises(bridge.BridgeFailure) as caught:
            bridge.Adapter(client, {}, confirmation_window=0).dispatch({"op": "send", "message": "hello"})
        self.assertEqual(caught.exception.code, "protocol_error")
        self.assertTrue(caught.exception.uncertain)
        self.assertEqual(client.sends, 1)

    def test_real_client_dispatches_without_reading_the_turn_stream(self):
        # Regression, both directions: upstream fire-and-forget closed the
        # Noise connection immediately after writing and a live prompt
        # never reached shared history; the first fix read the chat.stream
        # response to its end, which made every send wait out the agent's
        # whole turn before the app learned the dispatch had completed.
        # The dispatch now opens the stream and returns at once — the
        # send operation's own confirmation reads keep the connection
        # busy afterwards, and history, not the transport, proves the
        # prompt landed.
        import types
        from unittest.mock import patch
        from contextlib import contextmanager
        bridge = load_bridge()
        events = []
        class Gateway:
            def _open(self, method, **kwargs):
                events.append((method, kwargs))
                return 7
            def request(self, method, **kwargs):
                raise AssertionError('the turn stream must not be read to its end')
        class BaseClient:
            def __init__(self, config_loader):
                self.config_loader = config_loader
            @contextmanager
            def _gateway(self, config):
                yield Gateway()
                events.append('closed')
            def send(self, **kwargs):
                raise AssertionError('must not use the base client send')
        module = types.ModuleType('muse_mcp.client')
        setattr(module, 'MuseClient', BaseClient)
        with patch.dict('sys.modules', {'muse_mcp.client': module}):
            client = bridge.community_client({'node_id': 'linux-test'})
            result = client.send(message='hello', thread='selected-thread', fire_and_forget=True, wait_seconds=0)
        self.assertTrue(result['sent'])
        self.assertEqual(events[-1], 'closed')
        self.assertEqual(events[0][0], 'chat.stream')
        self.assertEqual(events[0][1]['body']['session_id'], 'selected-thread')
        self.assertEqual(events[0][1]['body']['items'], [{'type': 'text', 'text': 'hello'}])
        self.assertNotIn('device_id', events[0][1]['body'])
        with patch.dict('sys.modules', {'muse_mcp.client': module}):
            config = {'node_id': 'old-config-node'}
            client = bridge.community_client(config)
            client.send(message='once', thread='selected-thread', device_id='linux-new-session')
        self.assertEqual(events[-2][1]['body']['device_id'], 'linux-new-session')
        self.assertEqual(events[-2][1]['body']['node_id'], 'old-config-node')
        self.assertEqual(config, {'node_id': 'old-config-node'})
        # A quoted send rides the same stream with the quoted turn's id; the
        # quote's text never reaches the service.
        with patch.dict('sys.modules', {'muse_mcp.client': module}):
            client = bridge.community_client({'node_id': 'linux-test'})
            client.send(message='replying to it', thread='selected-thread',
                        reply_to='m-9')
        body = events[-2][1]['body']
        self.assertEqual(body['in_reply_to'], 'm-9')
        self.assertEqual(body['items'],
                         [{'type': 'text', 'text': 'replying to it'}])
        self.assertNotIn('in_reply_to', events[0][1]['body'])

    def test_connect_projects_only_typed_identity_and_threads(self):
        bridge = load_bridge()
        response = bridge.Adapter(FakeClient(), {}).dispatch({"op": "connect"})
        self.assertEqual(response, {"identity_name": "Test User", "vm_id": "vm-1",
            "threads": [{"session_id": "thread-1", "title": "Example",
                         "archived": False, "pinned": False, "is_thread": True,
                         "unread_count": 0, "is_primary": True,
                         "snippet": None,
                         "updated_at": "2026-10-17T00:00:00Z"}]})
        self.assertNotIn("SECRET", str(response))


class WidgetProbeTests(unittest.TestCase):
    """The probe learns the widget payload's shape from key paths only."""

    def test_widget_rows_log_key_paths_and_never_values(self):
        import tempfile
        bridge = load_bridge()
        rows = [{
            "message_id": "m1", "seq": 3, "role": "assistant",
            "text": "Pick one [[hatch_widget:widget-1234]]",
            "raw": {"event": "chat.message", "payload": {
                "display_text": "Pick one",
                "widgets": [{"id": "widget-1234", "kind": "options",
                             "options": [{"label": "SENTINEL-LABEL",
                                          "reply": "SENTINEL-REPLY"}]}],
            }},
        }]
        with tempfile.TemporaryDirectory() as root:
            projected = bridge.history_rows(rows, config_dir=root)
            self.assertEqual(projected[0]["text"], "Pick one [[hatch_widget:widget-1234]]")
            log = pathlib.Path(root, "op-timing.log").read_text()
        self.assertIn("widget-probe keys=", log)
        self.assertIn("payload.widgets[].options[].label", log)
        self.assertIn("payload.widgets[].options[].reply", log)
        self.assertNotIn("SENTINEL-LABEL", log)
        self.assertNotIn("SENTINEL-REPLY", log)
        self.assertNotIn("Pick one", log)

    def test_plain_rows_write_no_probe_line(self):
        import tempfile
        bridge = load_bridge()
        rows = [{"message_id": "m2", "seq": 1, "role": "user",
                 "text": "hello", "raw": {"payload": {"display_text": "hello"}}}]
        with tempfile.TemporaryDirectory() as root:
            bridge.history_rows(rows, config_dir=root)
            self.assertFalse(pathlib.Path(root, "op-timing.log").exists())


class HistoryProbeTests(unittest.TestCase):
    """The history probe learns the attachment payload's shape
    from key paths only, and stays silent for known envelopes."""

    def test_attachment_rows_log_key_paths_and_never_values(self):
        import tempfile
        bridge = load_bridge()
        rows = [{
            "message_id": "m1", "seq": 3, "role": "user",
            "text": "see attached",
            "raw": {"event": "chat.message", "payload": {
                "display_text": "see attached",
                "attachments": [{"name": "SENTINEL-NAME",
                                 "mime": "text/plain",
                                 "size": 42,
                                 "ref": "SENTINEL-REF"}],
            }},
        }]
        with tempfile.TemporaryDirectory() as root:
            bridge.history_rows(rows, config_dir=root)
            log = pathlib.Path(root, "op-timing.log").read_text()
        self.assertIn("history-probe keys=", log)
        self.assertIn("payload.attachments[].name", log)
        self.assertIn("payload.attachments[].ref", log)
        self.assertNotIn("SENTINEL-NAME", log)
        self.assertNotIn("SENTINEL-REF", log)
        self.assertNotIn("see attached", log)

    def test_known_envelope_rows_write_no_probe_line(self):
        import tempfile
        bridge = load_bridge()
        rows = [{"message_id": "m2", "seq": 1, "role": "user",
                 "text": "hello",
                 "raw": {"event": "chat.message",
                         "payload": {"display_text": "hello"}}}]
        with tempfile.TemporaryDirectory() as root:
            bridge.history_rows(rows, config_dir=root)
            self.assertFalse(pathlib.Path(root, "op-timing.log").exists())


class AttachmentProjectionTests(unittest.TestCase):
    """Attachments project from payload.metadata.attachments
    (probe harvest, Nov 13, 2026); the service path and hash do
    not cross, and ordinary rows keep their exact wire shape."""

    def test_attachments_project_without_path_or_hash(self):
        bridge = load_bridge()
        rows = [{
            "message_id": "m1", "seq": 3, "role": "user",
            "text": "see attached",
            "raw": {"payload": {"metadata": {"attachments": [{
                "filename": "op-timing-6.log", "mime_type": "text/plain",
                "kind": "file", "bytes_len": 12345, "width": None,
                "height": None, "caption": None, "transcription": None,
                "path": "/secret/service/path", "sha256": "deadbeef",
            }, {
                "filename": "shot.png", "mime_type": "image/png",
                "kind": "image", "bytes_len": 777, "width": 1200,
                "height": 800, "caption": "the crawl",
                "transcription": "a screenshot",
                "path": "/secret/other", "sha256": "cafe",
            }]}}},
        }]
        projected = bridge.history_rows(rows)
        attachments = projected[0]["attachments"]
        self.assertEqual(len(attachments), 2)
        self.assertEqual(attachments[0]["filename"], "op-timing-6.log")
        self.assertEqual(attachments[0]["bytes_len"], 12345)
        self.assertIsNone(attachments[0]["width"])
        self.assertEqual(attachments[1]["width"], 1200)
        self.assertEqual(attachments[1]["caption"], "the crawl")
        self.assertNotIn("path", attachments[0])
        self.assertNotIn("sha256", attachments[0])
        self.assertNotIn("/secret/service/path", str(projected))

    def test_plain_rows_have_no_attachments_key(self):
        bridge = load_bridge()
        rows = [{"message_id": "m2", "seq": 1, "role": "user",
                 "text": "hello", "raw": {"payload": {}}}]
        projected = bridge.history_rows(rows)
        self.assertNotIn("attachments", projected[0])


class SubagentsProbeTests(unittest.TestCase):
    """The probe learns the subagents.list shape from key paths only."""

    def test_probe_logs_key_paths_once_and_never_values(self):
        import contextlib
        import tempfile
        import time
        bridge = load_bridge()
        bridge._SUBAGENTS_PROBED = False

        class Gateway:
            def call_json(self, route, timeout=None):
                assert route == "subagents.list"
                return {"agents": [{"id": "a1", "title": "SENTINEL-TITLE",
                                    "status": "done"}]}

        class Client:
            @contextlib.contextmanager
            def _gateway(self, config):
                yield Gateway()

        with tempfile.TemporaryDirectory() as root:
            bridge._probe_subagents(root, Client(), {})
            bridge._probe_subagents(root, Client(), {})
            deadline = time.monotonic() + 5
            log = ""
            while time.monotonic() < deadline:
                path = pathlib.Path(root, "op-timing.log")
                if path.exists():
                    log = path.read_text()
                    if "subagents-probe" in log:
                        break
                time.sleep(0.02)
        self.assertIn("subagents-probe keys=", log)
        self.assertIn("agents[].title", log)
        self.assertEqual(log.count("subagents-probe"), 1)
        self.assertNotIn("SENTINEL-TITLE", log)


class IdeasProbeTests(unittest.TestCase):
    """The ideas probe learns card field shapes: types and lengths
    only, never values."""

    def test_probe_logs_shapes_once_and_never_values(self):
        import tempfile
        import time
        bridge = load_bridge()
        bridge._IDEAS_PROBED = False
        payload = {"sections": [{"title": "S", "cards": [{
            "id": "c1",
            "title": "SENTINEL-TITLE",
            "content": "SENTINEL-CONTENT " * 10,
            "tags": ["a", "b"],
            "presentation": {"icon": "x"},
        }]}]}
        with tempfile.TemporaryDirectory() as root:
            bridge._probe_ideas_shapes(root, payload)
            bridge._probe_ideas_shapes(root, payload)
            deadline = time.monotonic() + 5
            log = ""
            while time.monotonic() < deadline:
                path = pathlib.Path(root, "op-timing.log")
                if path.exists():
                    log = path.read_text()
                    if "ideas-probe" in log:
                        break
                time.sleep(0.02)
        self.assertIn("card-shapes:", log)
        self.assertIn("content=str[", log)
        self.assertIn("tags=list[2]", log)
        self.assertEqual(log.count("ideas-probe"), 1)
        self.assertNotIn("SENTINEL", log)


class SourceManifestTests(unittest.TestCase):
    """The installed community source is tamper-evident: every request
    recomputes the installer's manifest and fails closed on mismatch."""

    def make_root(self, root):
        source = pathlib.Path(root, "source")
        (source / "muse_mcp").mkdir(parents=True)
        (source / "muse_mcp" / "__init__.py").write_text("# fake\n")
        (source / "routes.json").write_text("{}\n")
        return source

    def write_manifest(self, source):
        import hashlib
        lines = []
        for path in sorted(source.rglob("*")):
            if path.is_file() and path.name != ".manifest-sha256":
                rel = path.relative_to(source).as_posix()
                lines.append("{}  {}\n".format(
                    hashlib.sha256(path.read_bytes()).hexdigest(), rel))
        (source / ".manifest-sha256").write_text("".join(sorted(lines)))

    def test_verify_passes_pristine_skips_legacy_fails_tampered(self):
        import sys
        import tempfile
        from unittest import mock
        bridge = load_bridge()
        with tempfile.TemporaryDirectory() as root:
            source = self.make_root(root)
            # Legacy installs carry no manifest: skipped, never failed.
            with mock.patch.object(sys, "path", [str(source)]):
                self.assertEqual(bridge.verify_source_manifests(), 0)
            self.write_manifest(source)
            with mock.patch.object(sys, "path", [str(source)]):
                self.assertEqual(bridge.verify_source_manifests(), 1)
            (source / "routes.json").write_text("{\"tampered\": true}\n")
            with mock.patch.object(sys, "path", [str(source)]):
                with self.assertRaises(bridge.BridgeFailure) as caught:
                    bridge.verify_source_manifests()
            self.assertEqual(caught.exception.code, "bridge_unavailable")
            (source / "routes.json").write_text("{}\n")
            (source / "evil.py").write_text("print('x')\n")
            with mock.patch.object(sys, "path", [str(source)]):
                with self.assertRaises(bridge.BridgeFailure):
                    bridge.verify_source_manifests()


class FeedVisualTests(unittest.TestCase):
    """The web-parity Feed build: kicker/category projection and the
    shape probe that names the emoji / image / prompt fields."""

    def nested_payload(self):
        return {"days": [{"local_date": "2026-11-12", "editions": [
            {"kind": "morning", "units": [
                {"unit_id": "u1", "title": "First post", "content": "Body.",
                 "kicker": "Both recoveries failed", "category": "Development",
                 "published_at": "2026-11-12T12:00:00Z"},
                {"unit_id": "u2", "title": "Second post", "content": "More.",
                 "attachments": [{"kind": "hero_image",
                                  "url": "https://example.com/hero.webp"}]},
            ]}]}]}

    def test_feed_projection_carries_kicker_and_category(self):
        bridge = load_bridge()
        result = bridge.feed_rows(self.nested_payload())
        first, second = result["items"]
        self.assertEqual(first["kicker"], "Both recoveries failed")
        self.assertEqual(first["category"], "Development")
        # A unit without the fields projects neither key.
        self.assertNotIn("kicker", second)
        self.assertNotIn("category", second)

    def test_feed_projection_carries_action_and_why(self):
        bridge = load_bridge()
        result = bridge.feed_rows({"items": [{
            "unit_id": "u9",
            "title": "Do the thing",
            "action": {"kind": "execute", "label": "Let's do it",
                       "state": "ready"},
            "why_did_i_see_this": "Because you asked.",
        }]})
        first = result["items"][0]
        self.assertEqual(first["action_label"], "Let's do it")
        self.assertEqual(first["action_kind"], "execute")
        self.assertEqual(first["why"], "Because you asked.")
        # A unit without them projects neither key.
        plain = bridge.feed_rows({"items": [{"title": "x"}]})["items"][0]
        self.assertNotIn("action_label", plain)
        self.assertNotIn("action_kind", plain)
        self.assertNotIn("why", plain)

    def test_other_listings_gain_no_feed_extras(self):
        bridge = load_bridge()
        result = bridge.listing_rows(
            {"items": [{"title": "x", "kicker": "K", "category": "C"}]},
            ("items",))
        self.assertNotIn("kicker", result["items"][0])
        self.assertNotIn("category", result["items"][0])

    def test_feed_probe_logs_key_paths_once_and_never_values(self):
        import contextlib
        import tempfile
        import time
        bridge = load_bridge()
        bridge._FEED_PROBED = False

        class Gateway:
            def call_json(self, route, timeout=None):
                if route == "feed.prompt.get":
                    return {"prompt": {"text": "SENTINEL-PROMPT"}}
                if route == "feed.status":
                    return {"state": "SENTINEL-STATE"}
                raise AssertionError(route)

        class Client:
            @contextlib.contextmanager
            def _gateway(self, config):
                yield Gateway()

        with tempfile.TemporaryDirectory() as root:
            bridge._probe_feed_shapes(root, Client(), {}, self.nested_payload())
            bridge._probe_feed_shapes(root, Client(), {}, self.nested_payload())
            deadline = time.monotonic() + 5
            log = ""
            while time.monotonic() < deadline:
                path = pathlib.Path(root, "op-timing.log")
                if path.exists():
                    log = path.read_text()
                    if "feed-prompt-probe" in log and "feed-status-probe" in log:
                        break
                time.sleep(0.02)
        self.assertIn("feed-probe keys=", log)
        self.assertIn("days[].editions[].units", log)
        self.assertIn("kicker", log)
        # The second unit's distinct shape is probed too: its optional
        # attachments are exactly what the first unit cannot reveal.
        self.assertIn("feed-probe unit keys=", log)
        self.assertIn("attachments[].url", log)
        self.assertIn("feed-prompt-probe keys=", log)
        self.assertIn("prompt.text", log)
        self.assertIn("feed-status-probe keys=", log)
        self.assertEqual(log.count("feed-probe keys="), 1)
        self.assertNotIn("SENTINEL-PROMPT", log)
        self.assertNotIn("SENTINEL-STATE", log)
        self.assertNotIn("Both recoveries failed", log)
        self.assertNotIn("hero.webp", log)


class ApprovalTests(unittest.TestCase):
    def test_approvals_are_projected_in_the_service_vocabulary_only(self):
        bridge = load_bridge()
        payload = {
            "pending_approvals": [{
                "approval_id": "a-1", "decision": "allow_always",
                "always_scope": "destination_domain", "attention_mode": "blocking",
                "access_token": "SECRET",
                "display": {"title": "Connect to example.com", "summary": "Outbound HTTPS",
                            "detail_rows": [{"label": "Host", "value": "example.com"},
                                            {"label": "Path", "value": "/x", "raw": "SECRET"}]}}],
            "recent_approvals": [{"approval_id": "old", "decision": "allow_always"}],
        }
        self.assertEqual(bridge.approval_rows(payload), {"pending": [{
            "approval_id": "a-1", "kind": None, "decision": "allow_always",
            "always_scope": "destination_domain", "attention_mode": "blocking",
            "title": "Connect to example.com", "summary": "Outbound HTTPS",
            "details": [{"label": "Host", "value": "example.com"},
                        {"label": "Path", "value": "/x"}]}]})
        self.assertNotIn("SECRET", str(bridge.approval_rows(payload)))

    def test_decision_history_projects_narrowly_and_drops_idless_rows(self):
        bridge = load_bridge()
        payload = {"items": [{
            "approval_id": "d-1", "requested_at_ms": 1, "decided_at_ms": 2,
            "display_name": "GitHub", "action_label": "Change content",
            "decision": "allow_once", "decided_by": "user-1", "status": "approved",
            "host": "github.com", "method": "CONNECT", "path": "/",
            "decision_reason": "owner approved", "matched_policy": "rule-9",
            "binary_path": "/home/u/SECRET", "cmdline_paths": ["/SECRET"],
            "payload_preview": "SECRET", "resume_token": {"checkpoint_blob": {}},
            "rich_explanation": "SECRET", "request_preview_document": {}}]}
        rows = bridge.decision_history_rows(payload)["decisions"]
        self.assertEqual(len(rows), 1)
        row = rows[0]
        self.assertEqual(row["approval_id"], "d-1")
        self.assertEqual(row["decision"], "allow_once")
        self.assertEqual(row["decided_by"], "user-1")
        self.assertEqual(row["matched_policy"], "rule-9")
        self.assertNotIn("SECRET", str(rows))
        for key in ("binary_path", "cmdline_paths", "payload_preview",
                    "resume_token", "rich_explanation",
                    "request_preview_document"):
            self.assertNotIn(key, row)
        dropped = bridge.decision_history_rows({"items": [
            {"decision": "allow_once"}, "nope",
            {"approval_id": "  ", "decision": "deny"},
            {"approval_id": "d-2", "decision": "deny"}]})["decisions"]
        self.assertEqual([r["approval_id"] for r in dropped], ["d-2"])
        for bad in ({}, {"items": "nope"}, {"items": None}):
            with self.assertRaises(bridge.BridgeFailure):
                bridge.decision_history_rows(bad)

    def test_approval_history_reads_the_sentinel_service_and_closes_it(self):
        bridge = load_bridge()
        seen = {}

        class Gateway:
            def call_json(self, method, **kwargs):
                seen["method"] = method
                seen["timeout"] = kwargs.get("timeout")
                return {"items": []}

            def close(self):
                seen["closed"] = True

        from unittest.mock import patch
        with patch.object(bridge, "sentinel_gateway", return_value=Gateway()):
            result = bridge.Adapter(object(), {}).dispatch({"op": "approval_history"})
        self.assertEqual(result, {"decisions": []})
        self.assertEqual(seen["method"], "egress.approval.history")
        self.assertTrue(seen["closed"])
        bridge.validate_request({"op": "approval_history", "config_dir": "/tmp/x"})
        with self.assertRaises(bridge.BridgeFailure):
            bridge.validate_request({"op": "approval_history", "config_dir": "/tmp/x",
                                     "cursor": "c1"})

    def test_an_approval_without_an_id_fails_closed(self):
        bridge = load_bridge()
        for payload in ({}, {"pending_approvals": [{"decision": "allow_once"}]},
                        {"pending_approvals": "nope"}):
            with self.assertRaises(bridge.BridgeFailure) as caught:
                bridge.approval_rows(payload)
            self.assertEqual(caught.exception.code, "protocol_error")

    def test_the_approvals_route_asks_the_sentinel_service_and_closes_it(self):
        bridge = load_bridge()
        seen = {}

        class Gateway:
            def call_json(self, method, **kwargs):
                seen["method"] = method
                seen["timeout"] = kwargs.get("timeout")
                return {"pending_approvals": []}

            def close(self):
                seen["closed"] = True

        from unittest.mock import patch
        with patch.object(bridge, "sentinel_gateway", return_value=Gateway()):
            result = bridge.Adapter(FakeClient(), {}).dispatch({"op": "approvals"})
        self.assertEqual(result, {"pending": []})
        self.assertEqual(seen["method"], "egress.approvals")
        self.assertEqual(seen["timeout"], 20)
        self.assertTrue(seen["closed"])

    def test_a_decision_asks_the_sentinel_route_and_claims_acceptance_only(self):
        bridge = load_bridge()
        seen = {}

        class Gateway:
            def call_json(self, method, **kwargs):
                seen["method"] = method
                seen.update(kwargs)
                return {"decision": "allow_once", "access_token": "SECRET"}

            def close(self):
                seen["closed"] = True

        from unittest.mock import patch
        with patch.object(bridge, "sentinel_gateway", return_value=Gateway()):
            result = bridge.Adapter(FakeClient(), {}).dispatch(
                {"op": "decide", "approval_id": "a-1", "decision": "allow_once"})
        self.assertEqual(seen["method"], "egress.approval.decide")
        self.assertEqual(seen["path_params"], {"approval_id": "a-1"})
        self.assertEqual(seen["body"], {"decision": "allow_once"})
        self.assertTrue(seen["closed"])
        # Accepted is not "the action ran": the request and the service's word are
        # reported, and a 2xx never becomes a claim about the effect.
        self.assertEqual(result, {"approval_id": "a-1", "requested": "allow_once",
                                  "accepted": True, "decision": "allow_once"})
        self.assertNotIn("SECRET", str(result))

    def test_only_the_five_published_outcomes_are_accepted(self):
        bridge = load_bridge()
        for decision in bridge.DECISIONS:
            bridge.validate_request({"op": "decide", "config_dir": "/tmp",
                                     "approval_id": "a-1", "decision": decision})
        for decision in ("allow", "deny_all", "", None, 1):
            with self.assertRaises(bridge.BridgeFailure) as caught:
                bridge.validate_request({"op": "decide", "config_dir": "/tmp",
                                         "approval_id": "a-1", "decision": decision})
            self.assertEqual(caught.exception.code, "invalid_request")
        with self.assertRaises(bridge.BridgeFailure):
            bridge.validate_request({"op": "decide", "config_dir": "/tmp",
                                     "decision": "deny"})

    def test_a_sentinel_notification_is_skipped_instead_of_aborting_the_read(self):
        # The defect this guards: the pinned reader parses every frame as a
        # ServiceResponse, so a notification frame raised DecodeError and the
        # whole approvals read died.
        bridge = load_bridge()
        import sys
        import threading
        import types
        from unittest.mock import patch

        class ServiceFrame:
            def __init__(self):
                self.stream_id = 0

            def ParseFromString(self, data):
                raise ValueError("DecodeError: not a ServiceFrame")

        class Message:
            def __init__(self):
                self.payload = b""

            def ParseFromString(self, data):
                self.payload = data

        class Gateway:
            def __init__(self, **kwargs):
                self.kwargs = kwargs
                self._recv_lock = threading.Lock()
                self.noise = types.SimpleNamespace(decrypt=lambda data: data)
                self.ws = types.SimpleNamespace(recv=lambda: (b"ciphertext", 0))

        module = types.ModuleType("muse")
        module.NoiseTransportFrame = Message
        module.ServiceResponse = Message
        module.ServiceFrame = ServiceFrame
        module.Gateway = Gateway
        with patch.dict(sys.modules, {"muse": module}), \
                patch.object(bridge, "cookie_header", return_value="a=b"):
            gateway = bridge.sentinel_gateway({"cookies_file": "/dev/null"})
            frame = gateway._read_frame()
        self.assertIsInstance(frame, ServiceFrame)
        self.assertEqual(frame.stream_id, bridge.SKIPPED_FRAME_STREAM)
        self.assertNotEqual(frame.stream_id, 0, "a skipped frame must never match a reply")

    def test_a_notification_that_is_not_a_response_is_also_skipped(self):
        # The other half of the same defect: a notification whose payload is not
        # a ServiceResponse fails the *outer* parse, before any frame exists.
        # Guarding only the inner parse still killed the read with `Wire format
        # was corrupt`, which is what live `egress.permissions`,
        # `egress.approval.history` and `egress.approvals` reads kept hitting.
        bridge = load_bridge()
        import sys
        import threading
        import types
        from unittest.mock import patch

        class ServiceFrame:
            def __init__(self):
                self.stream_id = 0

            def ParseFromString(self, data):
                raise AssertionError("the inner parse must not be reached")

        class Response:
            def __init__(self):
                self.payload = b""

            def ParseFromString(self, data):
                raise ValueError("DecodeError: Wire format was corrupt")

        class Transport:
            def __init__(self):
                self.payload = b"\x00\x01not-a-protobuf"

            def ParseFromString(self, data):
                pass

        class Gateway:
            def __init__(self, **kwargs):
                self.kwargs = kwargs
                self._recv_lock = threading.Lock()
                self.noise = types.SimpleNamespace(decrypt=lambda data: data)
                self.ws = types.SimpleNamespace(recv=lambda: (b"ciphertext", 0))

        module = types.ModuleType("muse")
        module.NoiseTransportFrame = Transport
        module.ServiceResponse = Response
        module.ServiceFrame = ServiceFrame
        module.Gateway = Gateway
        with patch.dict(sys.modules, {"muse": module}), \
                patch.object(bridge, "cookie_header", return_value="a=b"):
            gateway = bridge.sentinel_gateway({"cookies_file": "/dev/null"})
            frame = gateway._read_frame()
        self.assertIsInstance(frame, ServiceFrame)
        self.assertEqual(frame.stream_id, bridge.SKIPPED_FRAME_STREAM)
        self.assertNotEqual(frame.stream_id, 0, "a skipped frame must never match a reply")

    def _chunked_gateway(self, bridge, frames):
        """A SentinelGateway over scripted Noise frames: (id, index, total, bytes)."""
        import sys
        import threading
        import types
        from unittest.mock import patch

        script = list(frames)

        class ServiceFrame:
            def __init__(self):
                self.stream_id = 0

            def ParseFromString(self, data):
                # Only the reassembled frame is a frame at all.
                if data != b"body":
                    raise AssertionError("a chunk must never be parsed on its own")
                self.stream_id = 7

        class Response:
            def __init__(self):
                self.payload = b""

            def ParseFromString(self, data):
                if data != b"Hbody":
                    raise ValueError("DecodeError: Wire format was corrupt")
                self.payload = b"body"

        class Transport:
            def __init__(self):
                self.payload = b""
                self.chunk_id = 0
                self.chunk_index = 0
                self.total_chunks = 1

            def ParseFromString(self, data):
                chunk_id, index, total, payload = script.pop(0)
                self.chunk_id, self.chunk_index, self.total_chunks = chunk_id, index, total
                self.payload = payload

        class Gateway:
            def __init__(self, **kwargs):
                self._recv_lock = threading.Lock()
                self.noise = types.SimpleNamespace(decrypt=lambda data: data)
                self.ws = types.SimpleNamespace(recv=lambda: (b"ciphertext", 0))

        module = types.ModuleType("muse")
        module.NoiseTransportFrame = Transport
        module.ServiceResponse = Response
        module.ServiceFrame = ServiceFrame
        module.Gateway = Gateway
        with patch.dict(sys.modules, {"muse": module}), \
                patch.object(bridge, "cookie_header", return_value="a=b"):
            return bridge.sentinel_gateway({"cookies_file": "/dev/null"})

    def test_a_reply_split_across_transport_chunks_is_reassembled(self):
        # Live `egress.permissions` is ~496 KB, so the sentinel service splits one
        # reply across Noise chunks sharing a chunk_id. Each chunk alone fails the
        # `ServiceResponse` parse, so the reader skipped it and the body lost the
        # bytes in between — which `call_json` reported as `non-JSON response`.
        bridge = load_bridge()
        gateway = self._chunked_gateway(bridge, [(11, 0, 2, b"Hbo"), (11, 1, 2, b"dy")])
        self.assertEqual(gateway._read_frame().stream_id, 7)

    def test_chunks_are_joined_by_index_not_by_arrival(self):
        bridge = load_bridge()
        gateway = self._chunked_gateway(bridge, [(12, 1, 2, b"dy"), (12, 0, 2, b"Hbo")])
        self.assertEqual(gateway._read_frame().stream_id, 7)

    def test_two_chunk_groups_never_mix(self):
        # Two large replies can be chunked at the same time; the first frame to
        # complete is the one that returns, whichever group it belongs to.
        bridge = load_bridge()
        gateway = self._chunked_gateway(bridge, [
            (21, 0, 2, b"Hbo"), (22, 0, 2, b"Hbo"), (22, 1, 2, b"dy"), (21, 1, 2, b"dy")])
        self.assertEqual(gateway._read_frame().stream_id, 7)
        self.assertEqual(gateway._read_frame().stream_id, 7)


class PermissionTests(unittest.TestCase):
    def test_active_grants_are_projected_in_the_service_vocabulary_only(self):
        bridge = load_bridge()
        payload = {
            "items": [{
                "permission_id": "p-1", "kind": "egress", "lifetime": "always",
                "mode": "allow", "policy_subject": "example.com",
                "expires_at_ms": 1799999999000, "revocable": True,
                "access_token": "SECRET",
                "display": {"summary_title": "example.com", "raw": "SECRET"},
            }, {
                "permission_id": "p-2", "kind": "connector", "revocable": False,
            }],
            "cursor": "SECRET",
        }
        self.assertEqual(bridge.permission_rows(payload), {"grants": [{
            "permission_id": "p-1", "kind": "egress", "lifetime": "always",
            "mode": "allow", "policy_subject": "example.com",
            "expires_at_ms": 1799999999000, "revocable": True,
            "title": "example.com",
        }, {
            "permission_id": "p-2", "kind": "connector", "lifetime": None,
            "mode": None, "policy_subject": None, "expires_at_ms": None,
            "revocable": False, "title": "",
        }]})
        self.assertNotIn("SECRET", str(bridge.permission_rows(payload)))

    def test_a_grant_without_an_id_fails_closed(self):
        bridge = load_bridge()
        for payload in ({}, {"items": [{"kind": "egress"}]}, {"items": "nope"},
                        {"items": [{"permission_id": "  "}]}):
            with self.assertRaises(bridge.BridgeFailure) as caught:
                bridge.permission_rows(payload)
            self.assertEqual(caught.exception.code, "protocol_error")

    def test_the_permissions_route_asks_the_sentinel_service_and_closes_it(self):
        bridge = load_bridge()
        seen = {}

        class Gateway:
            def call_json(self, method, **kwargs):
                seen["method"] = method
                seen["timeout"] = kwargs.get("timeout")
                return {"items": []}

            def close(self):
                seen["closed"] = True

        from unittest.mock import patch
        with patch.object(bridge, "sentinel_gateway", return_value=Gateway()):
            result = bridge.Adapter(FakeClient(), {}).dispatch({"op": "permissions"})
        self.assertEqual(result, {"grants": []})
        self.assertEqual(seen["method"], "egress.permissions.active")
        self.assertEqual(seen["timeout"], 20)
        self.assertTrue(seen["closed"])

    def test_a_revocation_asks_the_sentinel_route_once_and_claims_acceptance_only(self):
        bridge = load_bridge()
        seen = {"calls": 0}

        class Gateway:
            def call_json(self, method, **kwargs):
                seen["calls"] += 1
                seen["method"] = method
                seen.update(kwargs)
                return {"revoked": True, "access_token": "SECRET"}

            def close(self):
                seen["closed"] = True

        from unittest.mock import patch
        with patch.object(bridge, "sentinel_gateway", return_value=Gateway()):
            result = bridge.Adapter(FakeClient(), {}).dispatch(
                {"op": "revoke", "permission_id": "p-1"})
        self.assertEqual(seen["calls"], 1)
        self.assertEqual(seen["method"], "egress.permissions.revoke")
        self.assertEqual(seen["body"], {"permission_id": "p-1"})
        self.assertTrue(seen["closed"])
        # Acceptance is not disappearance: only the service's own word travels,
        # and proving the grant is gone is a fresh read's job, not this answer's.
        self.assertEqual(result, {"permission_id": "p-1", "accepted": True,
                                  "revoked": True})
        self.assertNotIn("SECRET", str(result))

    def test_a_revocation_names_only_an_id_the_listing_published(self):
        bridge = load_bridge()
        bridge.validate_request({"op": "revoke", "config_dir": "/tmp",
                                 "permission_id": "p-1"})
        for bad in ("", None, 1, "x" * 257, "a\x00b"):
            with self.assertRaises(bridge.BridgeFailure) as caught:
                bridge.validate_request({"op": "revoke", "config_dir": "/tmp",
                                         "permission_id": bad})
            self.assertEqual(caught.exception.code, "invalid_request")
        with self.assertRaises(bridge.BridgeFailure):
            bridge.validate_request({"op": "revoke", "config_dir": "/tmp"})


class BrowserProbeTests(unittest.TestCase):
    def test_the_browser_probe_logs_vocabulary_and_browser_keys_only(self):
        import os
        import tempfile
        from contextlib import contextmanager
        bridge = load_bridge()
        bridge._BROWSER_PROBED = False
        records = [
            {"event": "delta.text_append", "seq": 1,
             "payload": {"text": "SECRET", "session_id": "s-1"}},
            {"event": "browser.task_started", "seq": 2,
             "payload": {"browser_task_id": "SECRET", "url": "SECRET"}},
            {"event": "status", "seq": 3, "payload": {"status": "working"}},
        ]

        class Connection:
            def subscribe_json(self, method, **kwargs):
                return records

        class Client(FakeClient):
            @contextmanager
            def _gateway(self, config):
                yield Connection()

        with tempfile.TemporaryDirectory() as config_dir:
            result = bridge.activity_rows(Client(), {}, 1000,
                                          config_dir=config_dir)
            with open(os.path.join(config_dir, "op-timing.log")) as handle:
                logged = handle.read()
        self.assertEqual(len(result["events"]), 3)
        self.assertIn("browser-probe", logged)
        self.assertIn("browser.task_started", logged)
        self.assertIn("browser_task_id", logged)
        self.assertNotIn("SECRET", logged)


class AttachProbeTests(unittest.TestCase):
    def _client(self, write_error=None, landed=True, with_attachment=True):
        from contextlib import contextmanager
        bridge = load_bridge()

        class Gateway:
            def __init__(self):
                self.writes = []
                self.opened = []

            def call_json(self, method, body=None, **kwargs):
                self.writes.append((method, body))
                if write_error is not None:
                    raise write_error
                return {}

            def _open(self, method, body=None, **kwargs):
                self.opened.append((method, body))
                return 1

        gateway = Gateway()

        class Client(FakeClient):
            def __init__(self):
                self.history_calls = 0

            @contextmanager
            def _gateway(self, config):
                yield gateway

            def history(self, **kwargs):
                self.history_calls += 1
                if self.history_calls == 1 or not landed:
                    return []
                attachments = ([{"filename": bridge.ATTACH_PROBE_PATH,
                                 "mime_type": "text/plain", "kind": "file",
                                 "bytes_len": 12}] if with_attachment else [])
                return [{"message_id": "probe-1", "seq": 1, "role": "user",
                         "text": bridge.ATTACH_PROBE_MESSAGE,
                         "raw": {"payload": {"metadata": {
                             "attachments": attachments}}}}]

        return bridge, Client(), gateway

    def test_the_probe_writes_sends_and_verifies_from_history(self):
        bridge, client, gateway = self._client()
        answer = bridge.Adapter(client, {"node_id": "node-1"}).dispatch(
            {"op": "attach_probe", "thread": "thread-9"})
        self.assertEqual(answer["fs_write"], "accepted")
        self.assertTrue(answer["sent"])
        self.assertTrue(answer["message_seen"])
        self.assertTrue(answer["attachment_seen"])
        self.assertEqual(gateway.writes[0][0], "fs.write")
        self.assertEqual(gateway.writes[0][1]["path"], bridge.ATTACH_PROBE_PATH)
        self.assertEqual(gateway.writes[0][1]["node_id"], "node-1")
        method, body = gateway.opened[0]
        self.assertEqual(method, "chat.stream")
        self.assertEqual(body["session_id"], "thread-9")
        self.assertEqual(body["items"][0]["type"], "text")
        self.assertEqual(body["items"][1],
                         {"type": "file", "path": bridge.ATTACH_PROBE_PATH})

    def test_a_refused_write_sends_nothing(self):
        bridge, client, gateway = self._client(write_error=RuntimeError("no"))
        answer = bridge.Adapter(client, {"node_id": "node-1"}).dispatch(
            {"op": "attach_probe", "thread": "thread-9"})
        self.assertEqual(answer["fs_write"], "refused")
        self.assertFalse(answer["sent"])
        self.assertEqual(gateway.opened, [])

    def test_a_message_without_an_attachment_is_reported_as_such(self):
        bridge, client, gateway = self._client(with_attachment=False)
        answer = bridge.Adapter(client, {"node_id": "node-1"}).dispatch(
            {"op": "attach_probe", "thread": "thread-9"})
        self.assertTrue(answer["message_seen"])
        self.assertFalse(answer["attachment_seen"])

    def test_the_probe_requires_a_named_thread(self):
        bridge = load_bridge()
        with self.assertRaises(bridge.BridgeFailure):
            bridge.validate_request({"op": "attach_probe",
                                     "config_dir": "/tmp/x"})
        bridge.validate_request({"op": "attach_probe", "thread": "t-1",
                                 "config_dir": "/tmp/x"})


class PolicySettingsTests(unittest.TestCase):
    def test_settings_are_projected_as_scalar_pairs_with_names_only(self):
        bridge = load_bridge()
        payload = {
            "hitl": {"enabled": True, "snooze_until": None},
            "network": {"mode": "allowlist", "rules": ["a", "b"]},
            "connectors": {"gmail": {"grants": 2}, "bad/name": {}, "github": {}},
            "access_token": "SECRET",
            "note": "x" * 500,
        }
        result = bridge.settings_rows(payload)
        by_label = {e["label"]: e["value"] for e in result["entries"]}
        self.assertEqual(by_label["hitl.enabled"], "true")
        self.assertEqual(by_label["network.mode"], "allowlist")
        self.assertEqual(by_label["access_token"], "SECRET"[:200])
        self.assertEqual(len(by_label["note"]), 200)
        # The null and the list are counted as withheld, never
        # summarized into words of the bridge's own; the
        # connectors section is represented by its names instead.
        self.assertEqual(result["withheld"], 2)
        # Only the settings' own well-formed connector names travel.
        self.assertEqual(result["connectors"], ["github", "gmail"])
        self.assertNotIn("bad/name", str(result))

    def test_protocols_project_as_one_entry_per_row(self):
        bridge = load_bridge()
        payload = {"protocols": [
            {"key": "ssh", "mode": "ask"},
            {"key": "ftp", "mode": "deny"},
            {"key": "", "mode": "ask"},
            {"mode": "ask"},
            "nope",
        ]}
        result = bridge.settings_rows(payload)
        by_label = {e["label"]: e["value"] for e in result["entries"]}
        self.assertEqual(by_label, {"protocol.ssh": "ask", "protocol.ftp": "deny"})
        self.assertEqual(result["withheld"], 3)

    def test_settings_projection_fails_closed_on_a_non_object(self):
        bridge = load_bridge()
        with self.assertRaises(bridge.BridgeFailure):
            bridge.settings_rows(["not", "a", "settings", "object"])

    def test_policy_settings_reads_the_sentinel_route_and_closes_it(self):
        bridge = load_bridge()
        seen = {}

        class Gateway:
            def call_json(self, method, **kwargs):
                seen["method"] = method
                seen["timeout"] = kwargs.get("timeout")
                return {"hitl": {"enabled": False}}

            def close(self):
                seen["closed"] = True

        from unittest.mock import patch
        with patch.object(bridge, "sentinel_gateway", return_value=Gateway()):
            result = bridge.Adapter(FakeClient(), {}).dispatch(
                {"op": "policy_settings"})
        self.assertEqual(seen["method"], "permissions.settings")
        self.assertEqual(seen["timeout"], 20)
        self.assertTrue(seen["closed"])
        self.assertEqual(result["entries"],
                         [{"label": "hitl.enabled", "value": "false"}])

    def test_connector_reset_dispatches_one_path_only_delete(self):
        bridge = load_bridge()
        seen = {"calls": 0}

        class Gateway:
            def call_json(self, method, **kwargs):
                seen["calls"] += 1
                seen["method"] = method
                seen.update(kwargs)
                return {"whatever": "the answer shape is unrecorded"}

            def close(self):
                seen["closed"] = True

        from unittest.mock import patch
        with patch.object(bridge, "sentinel_gateway", return_value=Gateway()):
            result = bridge.Adapter(FakeClient(), {}).dispatch(
                {"op": "connector_reset", "connector": "gmail"})
        self.assertEqual(seen["calls"], 1)
        self.assertEqual(seen["method"], "egress.permissions.connector.reset")
        self.assertEqual(seen["path_params"], {"connector": "gmail"})
        self.assertTrue(seen["closed"])
        self.assertEqual(result, {"connector": "gmail", "accepted": True})

    def test_connector_reset_requests_are_validated(self):
        bridge = load_bridge()
        bridge.validate_request({"op": "connector_reset", "connector": "gmail",
                                   "config_dir": "/tmp/x"})
        for bad in ({}, {"connector": ""}, {"connector": "a/b"},
                    {"connector": "has space"}, {"connector": "x" * 129},
                    {"connector": "-leading-dash"}):
            with self.assertRaises(bridge.BridgeFailure):
                bridge.validate_request(dict(bad, op="connector_reset",
                                               config_dir="/tmp/x"))

    def test_settings_update_sends_one_patch_and_projects_the_new_state(self):
        bridge = load_bridge()
        seen = {}

        class Gateway:
            def call_json(self, method, **kwargs):
                seen["method"] = method
                seen.update(kwargs)
                return {"connector_default": "ask",
                        "network_default": "ask",
                        "protocols": {"ssh": {"mode": "deny"}}}

            def close(self):
                seen["closed"] = True

        from unittest.mock import patch
        with patch.object(bridge, "sentinel_gateway", return_value=Gateway()):
            result = bridge.Adapter(FakeClient(), {}).dispatch(
                {"op": "settings_update", "connector_default": "ask",
                 "network_default": None, "protocols": None})
        self.assertEqual(seen["method"], "permissions.settings.update")
        self.assertEqual(seen["body"], {"connector_default": "ask"})
        self.assertTrue(seen["closed"])
        self.assertIn("entries", result)
        self.assertIn({"label": "connector_default", "value": "ask"},
                      result["entries"])

    def test_settings_update_requests_are_validated(self):
        bridge = load_bridge()
        bridge.validate_request({"op": "settings_update",
                                 "connector_default": "auto_allow",
                                 "network_default": None, "protocols": None,
                                 "config_dir": "/tmp/x"})
        bridge.validate_request({"op": "settings_update",
                                 "connector_default": None,
                                 "network_default": None,
                                 "protocols": {"ssh": "ask"},
                                 "config_dir": "/tmp/x"})
        for bad in ({"connector_default": None, "network_default": None,
                     "protocols": None},
                    {"connector_default": "", "network_default": None,
                     "protocols": None},
                    {"connector_default": None, "network_default": None,
                     "protocols": {}},
                    {"connector_default": None, "network_default": None,
                     "protocols": {"x" * 65: "ask"}},
                    {"connector_default": None, "network_default": None,
                     "protocols": {"ssh": ""}},
                    {"connector_default": None, "network_default": "ok",
                     "protocols": None, "extra": 1}):
            with self.subTest(bad=bad):
                with self.assertRaises(bridge.BridgeFailure):
                    bridge.validate_request(dict(bad, op="settings_update",
                                                 config_dir="/tmp/x"))



class DeleteTaskTests(unittest.TestCase):
    def test_a_deletion_asks_the_tasks_route_once_and_claims_acceptance_only(self):
        from contextlib import contextmanager
        bridge = load_bridge()
        seen = {"calls": 0}

        class Gateway:
            def call_json(self, method, **kwargs):
                seen["calls"] += 1
                seen["method"] = method
                seen.update(kwargs)
                return {"deleted": True, "access_token": "SECRET"}

        class Client(FakeClient):
            @contextmanager
            def _gateway(self, config):
                yield Gateway()

        result = bridge.Adapter(Client(), {}).dispatch(
            {"op": "delete_task", "job_id": "job-1"})
        self.assertEqual(seen["calls"], 1)
        self.assertEqual(seen["method"], "tasks.delete")
        self.assertEqual(seen["path_params"], {"job_id": "job-1"})
        # Acceptance is not absence: only the service's own word travels, and
        # proving the schedule is gone is a fresh read's job.
        self.assertEqual(result, {"job_id": "job-1", "accepted": True,
                                  "deleted": True})
        self.assertNotIn("SECRET", str(result))

    def test_a_deletion_names_only_an_id_the_listing_published(self):
        bridge = load_bridge()
        bridge.validate_request({"op": "delete_task", "config_dir": "/tmp",
                                 "job_id": "job-1"})
        for bad in ("", None, 1, "x" * 257, "a\x00b"):
            with self.assertRaises(bridge.BridgeFailure) as caught:
                bridge.validate_request({"op": "delete_task", "config_dir": "/tmp",
                                         "job_id": bad})
            self.assertEqual(caught.exception.code, "invalid_request")
        with self.assertRaises(bridge.BridgeFailure):
            bridge.validate_request({"op": "delete_task", "config_dir": "/tmp"})


class StartSessionTests(unittest.TestCase):
    def _dispatch(self, start_answer, listings):
        from contextlib import contextmanager
        bridge = load_bridge()
        self._bridge = bridge
        seen = {"starts": [], "lists": 0}

        class Gateway:
            def call_json(self, method, **kwargs):
                if method == "sessions.list":
                    index = min(seen["lists"], len(listings) - 1)
                    seen["lists"] += 1
                    return {"sessions": listings[index]}
                if method == "session.start":
                    seen["starts"].append(kwargs)
                    return start_answer
                raise AssertionError(f"unexpected route {method}")

        class Client(FakeClient):
            @contextmanager
            def _gateway(self, config):
                yield Gateway()

        result = bridge.Adapter(Client(), {}).dispatch({"op": "start_session"})
        return result, seen

    def test_a_start_sends_the_recorded_shape_once_and_returns_its_id(self):
        existing = [{"session_id": "thread-1", "title": "Example",
                     "archived": False, "updated_at": "2026-10-17T00:00:00Z"}]
        result, seen = self._dispatch(
            {"session_id": "thread-2", "access_token": "SECRET"}, [existing])
        self.assertEqual(result, {"session_id": "thread-2"})
        self.assertNotIn("SECRET", str(result))
        self.assertEqual(len(seen["starts"]), 1)
        self.assertEqual(seen["starts"][0]["body"], {
            "method": "/api/session/start",
            "params": {"origin": "fresh", "lifecycle": "persistent"}})

    def test_an_idless_start_answer_is_proven_against_the_listing(self):
        existing = [{"session_id": "thread-1", "title": "Example",
                     "archived": False, "updated_at": "2026-10-17T00:00:00Z"}]
        grown = existing + [{"session_id": "thread-2", "title": "New chat",
                             "archived": False,
                             "updated_at": "2026-10-17T00:01:00Z"}]
        result, _seen = self._dispatch({"ok": True}, [existing, grown])
        self.assertEqual(result, {"session_id": "thread-2"})

    def test_an_unprovable_start_is_a_protocol_error(self):
        existing = [{"session_id": "thread-1", "title": "Example",
                     "archived": False, "updated_at": "2026-10-17T00:00:00Z"}]
        with self.assertRaises(Exception) as caught:
            self._dispatch({"ok": True}, [existing, existing])
        # The module instance _dispatch loaded owns the exception
        # class; a second load would be a different class object.
        self.assertIsInstance(caught.exception, self._bridge.BridgeFailure)
        self.assertEqual(caught.exception.code, "protocol_error")

    def test_validation_accepts_a_bare_start_and_nothing_extra(self):
        bridge = load_bridge()
        bridge.validate_request({"op": "start_session", "config_dir": "/tmp"})
        with self.assertRaises(bridge.BridgeFailure) as caught:
            bridge.validate_request({"op": "start_session", "config_dir": "/tmp",
                                     "title": "surprise"})
        self.assertEqual(caught.exception.code, "invalid_request")


class SessionOpTests(unittest.TestCase):
    def _dispatch(self, request):
        from contextlib import contextmanager
        bridge = load_bridge()
        seen = []

        class Gateway:
            def call_json(self, method, **kwargs):
                seen.append((method, kwargs))
                return {"ok": True}

        class Client(FakeClient):
            @contextmanager
            def _gateway(self, config):
                yield Gateway()

        result = bridge.Adapter(Client(), {}).dispatch(
            {"op": "session_op", **request})
        return result, seen

    def test_pin_sends_the_recorded_method_body_once(self):
        result, seen = self._dispatch({"session_id": "thread-1", "kind": "pin"})
        self.assertEqual(result, {"session_id": "thread-1", "kind": "pin",
                                  "accepted": True})
        self.assertEqual(seen, [("session.pin", {"body": {
            "method": "/api/session/pin", "session_id": "thread-1"}})])

    def test_rename_sends_the_flat_body(self):
        result, seen = self._dispatch(
            {"session_id": "thread-1", "kind": "rename", "title": "New name"})
        self.assertEqual(result["kind"], "rename")
        self.assertEqual(seen, [("session.rename", {"body": {
            "session_id": "thread-1", "title": "New name"}})])

    def test_delete_sends_the_recorded_method_body(self):
        _result, seen = self._dispatch({"session_id": "thread-9", "kind": "delete"})
        self.assertEqual(seen, [("session.delete", {"body": {
            "method": "/api/session/delete", "session_id": "thread-9"}})])

    def test_validation_refuses_unrecorded_kinds_and_smuggled_titles(self):
        bridge = load_bridge()
        base = {"op": "session_op", "config_dir": "/tmp",
                "session_id": "thread-1"}
        bridge.validate_request({**base, "kind": "archive"})
        bridge.validate_request({**base, "kind": "rename", "title": "Ok"})
        for bad in ({**base, "kind": "move"},
                    {**base, "kind": "rename"},
                    {**base, "kind": "rename", "title": "  "},
                    {**base, "kind": "pin", "title": "smuggled"},
                    {**base, "kind": "pin", "session_id": ""},
                    {**base, "kind": "pin", "session_id": "a\x00b"}):
            with self.assertRaises(bridge.BridgeFailure) as caught:
                bridge.validate_request(bad)
            self.assertEqual(caught.exception.code, "invalid_request")

    def test_session_rows_project_pinned(self):
        from contextlib import contextmanager
        bridge = load_bridge()

        class Gateway:
            def call_json(self, method, **kwargs):
                return {"sessions": [
                    {"session_id": "t1", "title": "Pinned one", "pinned": True},
                    {"session_id": "t2", "title": "Plain"}]}

        class Client(FakeClient):
            @contextmanager
            def _gateway(self, config):
                yield Gateway()

        rows = bridge.session_rows(Client(), {})
        self.assertIs(rows[0]["pinned"], True)
        self.assertIs(rows[1]["pinned"], False)

    def test_session_rows_project_is_thread_with_a_safe_default(self):
        # The web sidebar lists conversation threads; a seeded
        # non-thread session (the "Learn about ..." walkthrough)
        # carries is_thread false and must survive projection AS
        # false, so the app can leave it out. Absent or null reads
        # as a thread: no payload shape may empty the sidebar.
        from contextlib import contextmanager
        bridge = load_bridge()

        class Gateway:
            def call_json(self, method, **kwargs):
                return {"sessions": [
                    {"session_id": "t1", "title": "Chat", "is_thread": True},
                    {"session_id": "t2", "title": "Learn about x",
                     "is_thread": False},
                    {"session_id": "t3", "title": "Unflagged"},
                    {"session_id": "t4", "title": "Null flag",
                     "is_thread": None}]}

        class Client(FakeClient):
            @contextmanager
            def _gateway(self, config):
                yield Gateway()

        rows = bridge.session_rows(Client(), {})
        self.assertEqual([row["is_thread"] for row in rows],
                         [True, False, True, True])

    def test_session_rows_project_unread_count_with_a_safe_default(self):
        # The sidebar badge rides the listing's own unread_count;
        # anything that is not a plain non-negative int (absent, a
        # bool, a string, a negative) reads as zero, so a shape
        # drift dims the badge instead of breaking the list.
        from contextlib import contextmanager
        bridge = load_bridge()

        class Gateway:
            def call_json(self, method, **kwargs):
                return {"sessions": [
                    {"session_id": "t1", "title": "Chat", "unread_count": 3},
                    {"session_id": "t2", "title": "Read chat"},
                    {"session_id": "t3", "title": "Bool", "unread_count": True},
                    {"session_id": "t4", "title": "Text", "unread_count": "2"},
                    {"session_id": "t5", "title": "Negative",
                     "unread_count": -1}]}

        class Client(FakeClient):
            @contextmanager
            def _gateway(self, config):
                yield Gateway()

        rows = bridge.session_rows(Client(), {})
        self.assertEqual([row["unread_count"] for row in rows],
                         [3, 0, 0, 0, 0])

    def test_session_rows_project_is_primary_with_a_safe_default(self):
        # Side chats ride the same list as the main chat; the row's
        # own is_primary flag tells them apart. Absent or null reads
        # as primary: no payload shape may exile a chat to a section
        # the reader never opens.
        from contextlib import contextmanager
        bridge = load_bridge()

        class Gateway:
            def call_json(self, method, **kwargs):
                return {"sessions": [
                    {"session_id": "t1", "title": "Main",
                     "is_primary": True},
                    {"session_id": "t2", "title": "Side",
                     "is_primary": False},
                    {"session_id": "t3", "title": "Unflagged"},
                    {"session_id": "t4", "title": "Null flag",
                     "is_primary": None}]}

        class Client(FakeClient):
            @contextmanager
            def _gateway(self, config):
                yield Gateway()

        rows = bridge.session_rows(Client(), {})
        self.assertEqual([row["is_primary"] for row in rows],
                         [True, False, True, True])


class CancelTests(unittest.TestCase):
    def _dispatch(self, answer):
        bridge = load_bridge()
        from contextlib import contextmanager
        seen = {}

        class Gateway:
            def call_json(self, method, *args, **kwargs):
                seen["method"] = method
                seen["args"] = args
                return answer

        class Client(FakeClient):
            @contextmanager
            def _gateway(self, config):
                yield Gateway()

        return bridge, bridge.Adapter(Client(), {}).dispatch({"op": "cancel"}), seen

    def test_cancel_passes_the_service_answer_through_without_extra_fields(self):
        _, result, seen = self._dispatch(
            {"cancelled": True, "message_id": "m-1", "access_token": "SECRET"})
        self.assertEqual(seen["method"], "chat.cancel")
        self.assertEqual(seen["args"], ())
        self.assertEqual(result, {"cancelled": True, "message_id": "m-1"})
        self.assertNotIn("SECRET", str(result))

    def test_a_silent_or_malformed_answer_is_never_reported_as_a_stop(self):
        for answer in ({"ok": True}, {"cancelled": "true"}, {}):
            _, result, _ = self._dispatch(answer)
            self.assertFalse(result["cancelled"], answer)
            self.assertIsNone(result["message_id"])

    def test_cancel_rejects_fields_it_does_not_use(self):
        bridge = load_bridge()
        bridge.validate_request({"op": "cancel", "config_dir": "/tmp"})
        for bad in ({"op": "cancel", "config_dir": "/tmp", "message": "hi"},
                    {"op": "cancel", "config_dir": "relative"}):
            with self.assertRaises(bridge.BridgeFailure):
                bridge.validate_request(bad)


class ActivityTests(unittest.TestCase):
    def test_activity_is_a_bounded_listen_with_an_allowlisted_projection(self):
        bridge = load_bridge()
        from contextlib import contextmanager
        seen = {}
        class Gateway:
            def subscribe_json(self, method, **kwargs):
                seen["method"] = method
                seen.update(kwargs)
                return [
                    {"event": "agent.status", "seq": 5,
                     "payload": {"activity_code": "working",
                                 "activity_text": "Is working",
                                 "session_id": "s1", "access_token": "SECRET"}},
                    {"event": "task.status", "seq": 6, "payload": {"status": "completed"}},
                    {"type": "event"},
                    "not a dict",
                ]
        class Client(FakeClient):
            @contextmanager
            def _gateway(self, config):
                yield Gateway()
        result = bridge.Adapter(Client(), {}).dispatch({"op": "activity", "timeout": 8000})
        self.assertEqual(seen["method"], "chat.subscribe")
        self.assertEqual(seen["overall_timeout"], 8)
        self.assertLessEqual(seen["idle_timeout"], 6)
        self.assertEqual(result, {"events": [
            {"event": "agent.status", "seq": 5, "text": None,
             "status": "Is working", "session_id": "s1"},
            {"event": "task.status", "seq": 6, "text": None,
             "status": "completed", "session_id": None},
        ]})
        # Only the allowlisted fields: a live payload may carry far more.
        self.assertNotIn("SECRET", str(result))
        self.assertNotIn("working", str(result["events"][1]))

    def test_activity_cannot_become_a_long_poll_or_smuggle_parameters(self):
        bridge = load_bridge()
        bridge.validate_request({"op": "activity", "config_dir": "/unused", "timeout": 30000})
        for bad in [{"op": "activity", "config_dir": "/unused"},
                    {"op": "activity", "config_dir": "/unused", "timeout": 30001},
                    {"op": "activity", "config_dir": "/unused", "timeout": 0},
                    {"op": "activity", "config_dir": "/unused", "timeout": True},
                    {"op": "activity", "config_dir": "/unused", "timeout": 1000,
                     "thread": "t"}]:
            with self.subTest(bad=bad), self.assertRaises(bridge.BridgeFailure):
                bridge.validate_request(bad)


class ProbeShapeBuildTests(unittest.TestCase):
    """The Nov 12, 2026 probe harvest: feed visuals, widget options,
    subagent activity and the feed prompt card, projected from the
    shapes the probes confirmed on Patrick's own account."""

    def test_feed_units_project_emoji_hero_and_own_stamp(self):
        bridge = load_bridge()
        payload = {"days": [{"local_date": "2026-11-12", "editions": [
            {"kind": "morning", "units": [
                {"unit_id": "u1", "title": "Post", "content": "Body.",
                 "emoji": "\U0001F4CB", "created_at_ms": 1794504781613,
                 "attachment": {"kind": "image",
                                "hero_image_url": "https://example.com/h.webp"}},
                {"unit_id": "u2", "title": "Plain", "content": "More.",
                 "attachment": {"kind": "widget", "widget_html": "<div/>"}},
            ]}]}]}
        first, second = bridge.feed_rows(payload)["items"]
        self.assertEqual(first["icon"], "\U0001F4CB")
        self.assertEqual(first["image_url"], "https://example.com/h.webp")
        # The unit's own instant, not the day fallback and not raw
        # epoch digits.
        self.assertTrue(first["stamp"].startswith("20"))
        self.assertNotEqual(first["stamp"], "2026-11-12")
        self.assertNotIn("1794504781613", first["stamp"])
        self.assertNotIn("image_url", second)
        self.assertNotIn("icon", second)

    def test_feed_units_project_body_md_as_the_body_with_links_intact(self):
        # The production shape the feed probe recorded: the
        # description rides as `body_md` (markdown), a key the
        # generic listing projection does not know -- cards showed
        # titles and pictures but no descriptions until the feed
        # extras carried it. The markdown, link syntax included,
        # must survive: the app's rich label renders it live.
        bridge = load_bridge()
        body = ("Starting October 30, attackers [exploited the portal]"
                "(https://example.com/story) for initial access.")
        payload = {"days": [{"local_date": "2026-11-13", "editions": [
            {"kind": "midday", "units": [
                {"unit_id": "u1", "title": "Gateways wiped",
                 "body_md": body, "kicker": "From portal to wiper",
                 "category": "Cybersecurity", "emoji": "\U0001F6E1",
                 "created_at_ms": 1794504781613,
                 "attachment": {"kind": "image",
                                "hero_image_url": "https://example.com/h.webp"}},
                {"unit_id": "u2", "title": "No description",
                 "created_at_ms": 1794504781613},
            ]}]}]}
        first, second = bridge.feed_rows(payload)["items"]
        self.assertEqual(first["body"], body)
        self.assertIn("[exploited the portal](https://example.com/story)",
                      first["body"])
        self.assertIsNone(second["body"])

    def test_widget_attachment_projects_its_readable_text(self):
        bridge = load_bridge()
        html = (
            "<div><p>Decision attempt expected Friday</p>"
            "<h3>UFP Lead SOC offer</h3>"
            "<ul><li>Proposal submitted</li>"
            "<li>On CFO&rsquo;s desk <span>Nov 12</span></li></ul>"
            "<p>See <a href=\"https://example.com/notes\">the notes</a>"
            " today</p>"
            "<script>var secret = 1;</script>"
            "<style>.x { color: red; }</style></div>"
        )
        payload = {"days": [{"local_date": "2026-11-12", "editions": [
            {"kind": "morning", "units": [
                {"unit_id": "u1", "title": "Post", "content": "Body.",
                 "attachment": {"kind": "widget", "widget_html": html}},
                {"unit_id": "u2", "title": "Ref", "content": "More.",
                 "attachment": {"kind": "widget",
                                "widget_html": "media-handle-123"}},
            ]}]}]}
        first, second = bridge.feed_rows(payload)["items"]
        self.assertEqual(
            first["widget_text"],
            "Decision attempt expected Friday\nUFP Lead SOC offer\n"
            "Proposal submitted\nOn CFO’s desk Nov 12\n"
            "See [the notes](https://example.com/notes) today",
        )
        self.assertNotIn("secret", first["widget_text"])
        # The raw markup rides along for the live frame.
        self.assertEqual(first["widget_html"], html)
        # A non-markup value is a reference, not text: no projection.
        self.assertNotIn("widget_text", second)
        self.assertNotIn("widget_html", second)

    def test_widget_options_project_from_history_payload(self):
        bridge = load_bridge()
        rows = [{
            "message_id": "m1", "seq": 3, "role": "assistant",
            "text": "Pick one [[hatch_widget:widget-x]]",
            "raw": {"payload": {"widgets": [{
                "widget_id": "w1", "kind": "options",
                "data": {"options": ["Yes, do it", "Not now"]},
                "state": {"data": {}},
            }]}},
        }, {
            "message_id": "m2", "seq": 4, "role": "assistant",
            "text": "Done [[hatch_widget:widget-y]]",
            "raw": {"payload": {"widgets": [{
                "widget_id": "w2", "kind": "options",
                "data": {"options": ["A", "B"]},
                "state": {"data": {"selectedText": "B"}},
            }]}},
        }, {
            "message_id": "m3", "seq": 5, "role": "user", "text": "plain",
            "raw": {"payload": {}},
        }]
        result = bridge.history_rows(rows)
        self.assertEqual(result[0]["options"], ["Yes, do it", "Not now"])
        self.assertNotIn("selected_option", result[0])
        self.assertEqual(result[1]["selected_option"], "B")
        self.assertNotIn("options", result[2])

    def test_agent_rows_lead_with_active_and_normalize_stamps(self):
        bridge = load_bridge()
        payload = {
            "active": [{"prompt": "Running thing", "status": "running",
                        "created_at": 1794504781}],
            "recent": [{"prompt": "Finished thing", "status": "completed",
                        "created_at": 1794500000, "completed_at": 1794501000,
                        "final_response": "All done."}],
        }
        rows = bridge.agent_rows(payload)
        self.assertEqual([r["prompt"] for r in rows],
                         ["Running thing", "Finished thing"])
        self.assertEqual(rows[1]["final_response"], "All done.")
        self.assertTrue(rows[0]["created_at"].startswith("20"))
        self.assertEqual(bridge.agent_rows({"nope": 1}), [])
        self.assertEqual(bridge.agent_rows([1, 2]), [])

    def test_feed_prompt_info_is_fail_soft(self):
        bridge = load_bridge()

        class Gateway:
            def __init__(self, answers):
                self.answers = answers

            def call_json(self, route, timeout=None):
                answer = self.answers[route]
                if isinstance(answer, Exception):
                    raise answer
                return answer

        info = bridge._feed_prompt_info(Gateway({
            "feed.prompt.get": {"prompt": {"free_text": "Keep it brief",
                                           "enabled": True}},
            "feed.status": {"refresh_in_progress": True,
                            "next_edition_kind": "evening"},
        }))
        self.assertEqual(info["feed_prompt"],
                         {"text": "Keep it brief", "enabled": True})
        self.assertEqual(info["feed_status"],
                         {"refresh_in_progress": True,
                          "next_edition_kind": "evening"})
        self.assertEqual(bridge._feed_prompt_info(Gateway({
            "feed.prompt.get": RuntimeError("down"),
            "feed.status": RuntimeError("down"),
        })), {})


if __name__ == "__main__":
    unittest.main()


class WriteActionTests(unittest.TestCase):
    """The interactive-parity writes (Patrick, Nov 13, 2026): an
    idea card's execution and a feed unit's reaction. Both bodies
    are the recorded consumer's own, sent exactly once."""

    def _client(self, calls, answer):
        from contextlib import contextmanager
        bridge = load_bridge()

        class Gateway:
            def call_json(self, method, **kwargs):
                calls.append((method, kwargs))
                return answer

        class Client(FakeClient):
            @contextmanager
            def _gateway(self, config):
                yield Gateway()

        return bridge, Client()

    def test_idea_execute_sends_the_recorded_body_once(self):
        calls = []
        bridge, client = self._client(calls, {"session_id": "s9"})
        result = bridge.Adapter(client, {}).dispatch(
            {"op": "idea_execute", "idea_id": "card-7"})
        self.assertEqual(calls, [(
            "api.idea-cards.execute",
            {"path_params": {"ideaCardId": "card-7"},
             "body": {"ideaCardId": "card-7", "mode": "full"}})])
        self.assertEqual(result, {"idea_id": "card-7", "accepted": True,
                                  "session_id": "s9"})

    def test_feed_react_sends_the_recorded_body_once(self):
        calls = []
        bridge, client = self._client(calls, {})
        result = bridge.Adapter(client, {}).dispatch(
            {"op": "feed_react", "unit_id": "u1", "reaction": "❤"})
        self.assertEqual(calls, [(
            "feed.unit.reaction",
            {"path_params": {"unit_id": "u1"}, "body": {"reaction": "❤"}})])
        self.assertEqual(result, {"unit_id": "u1", "accepted": True,
                                  "reaction": "❤"})

    def test_goal_delete_sends_the_path_only_delete_once(self):
        calls = []
        bridge, client = self._client(calls, {"deleted": True})
        result = bridge.Adapter(client, {}).dispatch(
            {"op": "goal_delete", "goal_id": "g1"})
        self.assertEqual(calls, [("goals.delete", {"path_params": {"id": "g1"}})])
        self.assertEqual(result, {"goal_id": "g1", "accepted": True,
                                  "deleted": True})

    def test_feed_unit_delete_sends_the_path_only_delete_once(self):
        calls = []
        bridge, client = self._client(calls, {"deleted": True})
        result = bridge.Adapter(client, {}).dispatch(
            {"op": "feed_unit_delete", "unit_id": "u1"})
        self.assertEqual(calls, [("feed.unit.delete",
                                  {"path_params": {"unit_id": "u1"}})])
        self.assertEqual(result, {"unit_id": "u1", "accepted": True,
                                  "deleted": True})

    def test_artifact_pin_sends_the_probed_body_once(self):
        calls = []
        bridge, client = self._client(calls, {})
        result = bridge.Adapter(client, {}).dispatch(
            {"op": "artifact_pin", "artifact_ref": "space:x", "pinned": True})
        self.assertEqual(calls, [("artifacts.pin",
                                  {"body": {"artifact_ref": "space:x",
                                           "pinned": True}})])
        self.assertEqual(result, {"artifact_ref": "space:x",
                                  "accepted": True, "pinned": True})

    def test_file_delete_sends_the_probed_body_once(self):
        calls = []
        bridge, client = self._client(calls, {"path": "/gone.txt"})
        result = bridge.Adapter(client, {}).dispatch(
            {"op": "file_delete", "path": "gone.txt"})
        self.assertEqual(calls, [("fs.delete",
                                  {"body": {"path": "gone.txt",
                                           "recursive": False}})])
        self.assertEqual(result, {"path": "gone.txt", "accepted": True})

    def test_idea_engagement_sends_the_probed_body_once(self):
        calls = []
        bridge, client = self._client(calls, {})
        result = bridge.Adapter(client, {}).dispatch(
            {"op": "idea_engagement", "id": "idea_1"})
        self.assertEqual(calls, [("api.ideas.engagement",
                                  {"path_params": {"id": "idea_1"},
                                   "body": {"event_type": "feedback_up"}})])
        self.assertEqual(result, {"id": "idea_1", "accepted": True})

    def test_idea_feedback_sends_the_probed_body_once(self):
        calls = []
        bridge, client = self._client(calls, {})
        result = bridge.Adapter(client, {}).dispatch(
            {"op": "idea_feedback", "id": "idea_1"})
        self.assertEqual(calls, [("api.ideas.feedback",
                                  {"path_params": {"id": "idea_1"},
                                   "body": {"feedback": "feedback_down"}})])
        self.assertEqual(result, {"id": "idea_1", "accepted": True})

    def test_goal_rename_sends_the_probed_patch_once(self):
        calls = []
        bridge, client = self._client(calls, {})
        result = bridge.Adapter(client, {}).dispatch(
            {"op": "goal_rename", "goal_id": "goal_1", "title": "New name"})
        self.assertEqual(calls, [("goals.update",
                                  {"path_params": {"id": "goal_1"},
                                   "body": {"title": "New name"}})])
        self.assertEqual(result, {"goal_id": "goal_1", "accepted": True,
                                  "title": "New name"})

    def test_goal_set_complete_sends_the_probed_status_once(self):
        for completed, status in ((True, "completed"), (False, "active")):
            calls = []
            bridge, client = self._client(calls, {})
            result = bridge.Adapter(client, {}).dispatch(
                {"op": "goal_set_complete", "goal_id": "goal_1",
                 "completed": completed})
            self.assertEqual(calls, [("goals.update",
                                      {"path_params": {"id": "goal_1"},
                                       "body": {"status": status}})])
            self.assertEqual(result, {"goal_id": "goal_1", "accepted": True,
                                      "completed": completed})

    def test_feed_prompt_update_sends_the_probed_patch_once(self):
        calls = []
        bridge, client = self._client(calls, {})
        result = bridge.Adapter(client, {}).dispatch(
            {"op": "feed_prompt_update", "prompt": "new words"})
        self.assertEqual(calls, [("feed.prompt.update",
                                  {"body": {"prompt": "new words"}})])
        self.assertEqual(result, {"accepted": True})

    def test_goal_get_shapes_the_detail_answer(self):
        calls = []
        detail = {"goal": {"goal_id": "goal_1", "title": "T",
                           "status": "active", "summary": "S",
                           "description": "D", "momentum": "",
                           "momentum_status": "on_track",
                           "emoji": "X", "parent_goal_id": None,
                           "extra": "dropped"},
                  "updates": [{"update_id": "u1", "title": "UT",
                               "summary": "US", "description": "UD",
                               "effective_at": "2026-11-18",
                               "extra": "dropped"}],
                  "associations": [{"association_type": "cron",
                                    "details": {"title": "LT",
                                                "id": "1", "path": "/x"}}]}
        bridge, client = self._client(calls, detail)
        result = bridge.Adapter(client, {}).dispatch(
            {"op": "goal_get", "goal_id": "goal_1"})
        self.assertEqual(calls, [("goals.get",
                                  {"path_params": {"id": "goal_1"}})])
        self.assertNotIn("extra", result["goal"])
        self.assertNotIn("extra", result["updates"][0])
        self.assertEqual(result["associations"],
                         [{"association_type": "cron", "title": "LT"}])

    def test_goal_add_subgoal_sends_the_probed_create_once(self):
        calls = []
        bridge, client = self._client(calls, {"goal_id": "goal_9"})
        result = bridge.Adapter(client, {}).dispatch(
            {"op": "goal_add_subgoal", "parent_goal_id": "goal_1",
             "title": "Sub", "summary": "S", "description": "D",
             "emoji": "X"})
        self.assertEqual(calls, [("goals.create",
                                  {"body": {"title": "Sub", "summary": "S",
                                           "description": "D", "emoji": "X",
                                           "parent_goal_id": "goal_1"}})])
        self.assertEqual(result, {"goal_id": "goal_9",
                                  "parent_goal_id": "goal_1",
                                  "accepted": True})

    def test_goal_create_sends_the_probed_create_once(self):
        calls = []
        bridge, client = self._client(calls, {"goal_id": "goal_9"})
        result = bridge.Adapter(client, {}).dispatch(
            {"op": "goal_create",
             "title": "Top", "summary": "S", "description": "D",
             "emoji": "X"})
        self.assertEqual(calls, [("goals.create",
                                  {"body": {"title": "Top", "summary": "S",
                                           "description": "D",
                                           "emoji": "X"}})])
        self.assertEqual(result, {"goal_id": "goal_9", "accepted": True})

    def test_goals_tracking_reads_the_tracking_source(self):
        calls = []
        listing = {"goals": [{"goal_id": "goal_1", "title": "T"}]}
        bridge, client = self._client(calls, listing)
        result = bridge.Adapter(client, {}).dispatch(
            {"op": "goals_tracking"})
        self.assertEqual(calls[0][0], "goals.list")
        self.assertEqual(calls[0][1]["body"], {"source": "assistant_tracking"})
        self.assertEqual(result["items"][0]["id"], "goal_1")

    def test_idea_detail_shapes_the_dialog_sections(self):
        calls = []
        detail = {"card": {"content": {"title": "T", "summary": "S",
                                       "buildSummary": "H", "extra": 1},
                           "items": [{"content": {"title": "I",
                                                  "summary": "IS"}}],
                           "extra": 1}}
        bridge, client = self._client(calls, detail)
        result = bridge.Adapter(client, {}).dispatch(
            {"op": "idea_detail", "idea_id": "idea_1"})
        self.assertEqual(calls, [("api.idea-cards.detail",
                                  {"path_params": {"ideaCardId": "idea_1"}})])
        self.assertEqual(result["content"],
                         {"title": "T", "summary": "S", "build_summary": "H"})
        self.assertEqual(result["items"],
                         [{"title": "I", "summary": "IS",
                           "build_summary": None}])

    def test_mark_seen_sends_the_recorded_call_once(self):
        # The recorded consumer's own shape: the thread id as a
        # path param and no body. One dispatch, one call.
        calls = []
        bridge, client = self._client(calls, {})
        result = bridge.Adapter(client, {}).dispatch(
            {"op": "mark_seen", "thread_id": "t1"})
        self.assertEqual(calls, [(
            "chat.mark_seen", {"path_params": {"thread_id": "t1"}})])
        self.assertEqual(result, {"thread_id": "t1", "accepted": True})

    def test_write_ops_refuse_unpublished_shapes(self):
        bridge = load_bridge()
        for request in (
            {"op": "goal_delete", "goal_id": ""},
            {"op": "goal_delete", "goal_id": "g1", "extra": 1},
            {"op": "mark_seen", "thread_id": ""},
            {"op": "mark_seen", "thread_id": "t1", "extra": 1},
            {"op": "mark_seen"},
            {"op": "idea_execute", "idea_id": ""},
            {"op": "idea_execute", "idea_id": "x", "mode": "partial"},
            {"op": "feed_react", "unit_id": "u1", "reaction": ""},
            {"op": "feed_react", "unit_id": "u1", "reaction": "x" * 17},
            {"op": "feed_react", "unit_id": "u1", "reaction": "a\x01b"},
            {"op": "feed_react", "unit_id": "u1"},
            {"op": "feed_unit_delete", "unit_id": ""},
            {"op": "feed_unit_delete", "unit_id": "u1", "extra": 1},
            {"op": "feed_unit_delete"},
            {"op": "artifact_pin", "artifact_ref": "", "pinned": True},
            {"op": "artifact_pin", "artifact_ref": "space:x"},
            {"op": "artifact_pin", "artifact_ref": "space:x", "pinned": "yes"},
            {"op": "artifact_pin", "artifact_ref": "space:x", "pinned": True,
             "extra": 1},
            {"op": "file_delete", "path": ""},
            {"op": "file_delete", "path": "/abs.txt"},
            {"op": "file_delete", "path": "a.txt", "extra": 1},
            {"op": "file_delete"},
            {"op": "idea_engagement", "id": ""},
            {"op": "idea_engagement", "id": "idea_1", "extra": 1},
            {"op": "idea_engagement"},
            {"op": "idea_feedback", "id": ""},
            {"op": "idea_feedback"},
            {"op": "goal_rename", "goal_id": "", "title": "New"},
            {"op": "goal_rename", "goal_id": "goal_1", "title": "  "},
            {"op": "goal_rename", "goal_id": "goal_1"},
            {"op": "goal_rename", "goal_id": "goal_1", "title": "New",
             "extra": 1},
            {"op": "goal_set_complete", "goal_id": "", "completed": True},
            {"op": "goal_set_complete", "goal_id": "goal_1",
             "completed": "yes"},
            {"op": "goal_set_complete", "goal_id": "goal_1"},
            {"op": "feed_prompt_update", "prompt": ""},
            {"op": "feed_prompt_update", "prompt": "   "},
            {"op": "feed_prompt_update"},
            {"op": "goal_get", "goal_id": ""},
            {"op": "goal_get"},
            {"op": "goal_get", "goal_id": "goal_1", "extra": 1},
            {"op": "goal_add_subgoal", "parent_goal_id": "",
             "title": "Sub", "summary": "S", "description": "D",
             "emoji": "X"},
            {"op": "goal_add_subgoal", "parent_goal_id": "goal_1",
             "title": "  ", "summary": "S", "description": "D",
             "emoji": "X"},
            {"op": "goal_add_subgoal", "parent_goal_id": "goal_1",
             "title": "Sub", "summary": "S", "description": "D"},
            {"op": "goal_add_subgoal", "parent_goal_id": "goal_1",
             "title": "Sub", "summary": "S", "description": "D",
             "emoji": "X", "extra": 1},
            {"op": "goal_create", "title": "", "summary": "S",
             "description": "D", "emoji": "X"},
            {"op": "goal_create", "title": "Top", "summary": "S",
             "description": "D"},
            {"op": "goal_create", "title": "Top", "summary": "S",
             "description": "D", "emoji": "X", "extra": 1},
            {"op": "idea_detail", "idea_id": ""},
            {"op": "idea_detail"},
            {"op": "idea_detail", "idea_id": "idea_1", "extra": 1},
        ):
            with self.assertRaises(bridge.BridgeFailure) as caught:
                bridge.validate_request(dict(request, config_dir="/tmp/x"))
            self.assertEqual(caught.exception.code, "invalid_request", request)


class ListingTests(unittest.TestCase):
    def gateway_client(self, payload, calls):
        from contextlib import contextmanager
        class Gateway:
            def call_json(self, method, **kwargs):
                calls.append(method)
                return payload
        class Client(FakeClient):
            @contextmanager
            def _gateway(self, config):
                yield Gateway()
        return Client()

    def test_ideas_read_the_recorded_route_and_project_display_fields(self):
        bridge = load_bridge()
        calls = []
        client = self.gateway_client({"cards": [
            {"id": "card-1", "title": "Try a morning brief", "description": "A short brief.",
             "type": "habit", "created_at": "2026-11-08", "raw": "SECRET"},
            {"idea_card_id": "card-2", "name": "Bearer abcdef123456 in the body",
             "body": "Token is access_token=xyz987 in this text"},
        ]}, calls)
        result = bridge.Adapter(client, {}).dispatch({"op": "ideas"})
        self.assertEqual(calls, ["api.idea-cards.list"])
        self.assertEqual(result["total"], 2)
        self.assertEqual(result["recognized"], 2)
        self.assertEqual(result["skipped"], 0)
        first, second = result["items"]
        self.assertEqual(first["id"], "card-1")
        self.assertEqual(first["title"], "Try a morning brief")
        self.assertEqual(first["body"], "A short brief.")
        self.assertEqual(first["kind"], "habit")
        self.assertEqual(first["stamp"], "2026-11-08")
        self.assertEqual(second["id"], "card-2")
        # Unlisted fields never cross, and credential shapes are redacted.
        self.assertNotIn("SECRET", str(result))
        self.assertNotIn("abcdef123456", str(result))
        self.assertNotIn("xyz987", str(result))

    def test_goals_accept_a_bare_list_and_count_unshowable_rows(self):
        bridge = load_bridge()
        calls = []
        client = self.gateway_client([
            {"id": "g1", "title": "Ship the Linux client", "status": "active"},
            {"id": "g2", "description": "A goal described only by its body text"},
            {"id": "g3", "unrelated": True},
        ], calls)
        result = bridge.Adapter(client, {}).dispatch({"op": "goals"})
        self.assertEqual(calls, ["goals.list"])
        self.assertEqual(result["total"], 3)
        self.assertEqual(result["recognized"], 2)
        self.assertEqual(result["skipped"], 1)
        self.assertEqual(result["items"][0]["kind"], "active")
        # A body-only goal is shown under its own opening words, not dropped.
        self.assertEqual(result["items"][1]["title"], "A goal described only by its body text")

    def test_feed_unwraps_edges_and_fails_closed_on_a_non_list_shape(self):
        bridge = load_bridge()
        calls = []
        client = self.gateway_client({"units": [
            {"node": {"unit_id": "u1", "headline": "Morning briefing", "text": "Body."}},
        ]}, calls)
        result = bridge.Adapter(client, {}).dispatch({"op": "feed"})
        self.assertEqual(calls, ["feed.list", "feed.prompt.get", "feed.status"])
        self.assertEqual(result["items"][0]["id"], "u1")
        self.assertEqual(result["items"][0]["title"], "Morning briefing")
        with self.assertRaises(bridge.BridgeFailure) as caught:
            bridge.listing_rows({"unexpected": {}}, ("units",))
        self.assertEqual(caught.exception.code, "protocol_error")
        with self.assertRaises(bridge.BridgeFailure):
            bridge.listing_rows([{"title": "x"}] * 501, ("units",))

    def test_ideas_sections_shape_projects_the_nested_cards(self):
        # The production shape the community client itself reads: cards
        # nest inside titled sections, under "cards" or "ideas".
        bridge = load_bridge()
        calls = []
        client = self.gateway_client({
            "sections": [
                {"title": "For your family", "cards": [
                    {"ideaCardId": "c1", "title": "Plan the week"},
                    {"id": "c2", "title": "Book the dentist"},
                ]},
                {"title": "New", "ideas": [
                    {"id": "c3", "title": "Try a recipe"},
                ]},
            ]
        }, calls)
        result = bridge.Adapter(client, {}).dispatch({"op": "ideas"})
        self.assertEqual(calls, ["api.idea-cards.list"])
        self.assertEqual(result["total"], 3)
        self.assertEqual(result["recognized"], 3)
        self.assertEqual(result["skipped"], 0)
        self.assertEqual(
            [item["title"] for item in result["items"]],
            ["Plan the week", "Book the dentist", "Try a recipe"],
        )
        self.assertEqual(result["items"][0]["id"], "c1")
        # The section title is the card's kind when the card names none.
        self.assertEqual(result["items"][0]["kind"], "For your family")
        self.assertEqual(result["items"][2]["kind"], "New")

    def test_ideas_cards_carry_their_icon_fields(self):
        # The web draws an icon at each card's left edge. Under the
        # names the feed payload carries them by, the projection
        # fills the listing's icon and image slots; a card without
        # them leaves both empty.
        bridge = load_bridge()
        client = self.gateway_client({
            "sections": [
                {"title": "S", "cards": [
                    {"id": "c1", "title": "With icon", "emoji": "💡",
                     "icon_url": "https://example.com/i.png"},
                    {"id": "c2", "title": "Bare"},
                ]},
            ]
        }, [])
        result = bridge.Adapter(client, {}).dispatch({"op": "ideas"})
        self.assertEqual(result["items"][0]["icon"], "💡")
        self.assertEqual(result["items"][0]["image_url"],
                         "https://example.com/i.png")
        self.assertIsNone(result["items"][1].get("icon"))
        self.assertIsNone(result["items"][1].get("image_url"))

    def test_feed_days_shape_projects_the_nested_units(self):
        # The production shape the community client itself reads: units
        # nest inside editions inside days.
        bridge = load_bridge()
        calls = []
        client = self.gateway_client({
            "days": [
                {"local_date": "2026-11-09", "editions": [
                    {"kind": "morning", "units": [
                        {"unit_id": "u1", "title": "Morning brief",
                         "content": "What matters today."},
                        {"id": "u2", "title": "Second story",
                         "body": "More detail.", "published_at": "2026-11-09T12:00:00Z"},
                    ]},
                    {"kind": "evening", "units": [
                        {"unit_id": "u3", "title": "Evening wrap", "text": "The day, closed."},
                    ]},
                ]},
            ]
        }, calls)
        result = bridge.Adapter(client, {}).dispatch({"op": "feed"})
        self.assertEqual(calls, ["feed.list", "feed.prompt.get", "feed.status"])
        self.assertEqual(result["recognized"], 3)
        self.assertEqual(result["items"][0]["id"], "u1")
        self.assertEqual(result["items"][0]["body"], "What matters today.")
        # The day's date is the unit's stamp; the edition is its kind.
        self.assertEqual(result["items"][0]["stamp"], "2026-11-09")
        self.assertEqual(result["items"][0]["kind"], "morning")
        # A unit's own published stamp wins over the day fallback.
        self.assertEqual(result["items"][1]["stamp"], "2026-11-09T12:00:00Z")
        self.assertEqual(result["items"][2]["kind"], "evening")

    def test_goal_subtitle_rides_as_the_body(self):
        bridge = load_bridge()
        calls = []
        client = self.gateway_client({"goals": [
            {"goal_id": "g1", "title": "Ship it", "status": "active",
             "subtitle": "Three steps left"},
        ]}, calls)
        result = bridge.Adapter(client, {}).dispatch({"op": "goals"})
        self.assertEqual(result["items"][0]["body"], "Three steps left")

    def test_the_three_ops_validate_without_extra_keys(self):
        bridge = load_bridge()
        for op in ("ideas", "goals", "feed"):
            bridge.validate_request({"op": op, "config_dir": "/unused"})
            with self.assertRaises(bridge.BridgeFailure):
                bridge.validate_request({"op": op, "config_dir": "/unused", "limit": 1})

    def test_memory_search_projects_citations_and_counts_unreadable_hits(self):
        from contextlib import contextmanager
        bridge = load_bridge()
        calls = []
        class Gateway:
            def call_json(self, method, body=None, timeout=None):
                calls.append((method, body))
                return {"ok": True, "hits": [
                    {"memory_uri": "memory://md:memory/2026-10-22.md:950",
                     "text": "budget surgery", "scores": {"final": 0.5},
                     "citation": {"path": "memory/2026-10-22.md", "line_start": 950,
                                  "line_end": 950, "source_kind": "daily"},
                     "metadata": {"status": "active", "privacy_class": "default",
                                  "secret": "SECRET"}},
                    {"memory_uri": "", "text": "no address"},
                    {"memory_uri": "memory://md:x", "text": ""},
                ]}
        class Client(FakeClient):
            @contextmanager
            def _gateway(self, config):
                yield Gateway()
        result = bridge.Adapter(Client(), {}).dispatch(
            {"op": "memory_search", "query": "budget"})
        self.assertEqual(calls, [("memory.search", {"query": "budget"})])
        self.assertEqual(result["recognized"], 1)
        self.assertEqual(result["skipped"], 2)
        row = result["hits"][0]
        self.assertEqual(set(row), set(bridge.MEMORY_HIT_FIELDS))
        self.assertEqual(row["path"], "memory/2026-10-22.md")
        self.assertEqual(row["line_start"], 950)
        self.assertEqual(row["source_kind"], "daily")
        self.assertEqual(row["status"], "active")
        self.assertNotIn("SECRET", str(result))

    def test_memory_get_projects_a_missing_entry_instead_of_raising(self):
        from contextlib import contextmanager
        bridge = load_bridge()
        uri = "memory://md:memory/2026-10-22.md:950"
        class Gateway:
            def call_json(self, method, body=None, timeout=None):
                assert method == "memory.get" and body == {"memory_uri": uri}
                return {"ok": False, "memory_uri": uri, "path": "",
                        "line_start": 1, "line_end": 1, "text": "",
                        "reason": "memory entry not found"}
        class Client(FakeClient):
            @contextmanager
            def _gateway(self, config):
                yield Gateway()
        result = bridge.Adapter(Client(), {}).dispatch(
            {"op": "memory_get", "memory_uri": uri})
        self.assertEqual(result["found"], False)
        self.assertIsNone(result["text"])
        self.assertNotIn("reason", result, "the service's wording stays upstream")

    def test_memory_ops_accept_only_their_recorded_shape(self):
        bridge = load_bridge()
        bridge.validate_request({"op": "memory_search", "config_dir": "/unused",
                                 "query": "budget"})
        for bad in ("", "\x00"):
            with self.assertRaises(bridge.BridgeFailure):
                bridge.validate_request({"op": "memory_search", "config_dir": "/unused",
                                         "query": bad})
        with self.assertRaises(bridge.BridgeFailure):
            bridge.validate_request({"op": "memory_search", "config_dir": "/unused"})
        with self.assertRaises(bridge.BridgeFailure):
            bridge.validate_request({"op": "memory_search", "config_dir": "/unused",
                                     "query": "b", "q": "b"})
        uri = "memory://md:memory/2026-10-22.md:950"
        bridge.validate_request({"op": "memory_get", "config_dir": "/unused",
                                 "memory_uri": uri})
        for bad in ("", "workspace/memory/2026-10-22.md", "memory://" + "x" * 3000):
            with self.assertRaises(bridge.BridgeFailure):
                bridge.validate_request({"op": "memory_get", "config_dir": "/unused",
                                         "memory_uri": bad})


class StreamProbeTests(unittest.TestCase):
    def _client(self, records=None, error=None):
        from contextlib import contextmanager
        bridge = load_bridge()
        calls = []

        class Gateway:
            def subscribe_json(self, method, body=None, max_records=None,
                               idle_timeout=None, overall_timeout=None):
                calls.append((method, body))
                if error is not None:
                    raise error
                return records or []

        class Client(FakeClient):
            @contextmanager
            def _gateway(self, config):
                yield Gateway()

        return bridge, Client(), calls

    def test_the_probe_requires_a_named_thread(self):
        bridge = load_bridge()
        with self.assertRaises(bridge.BridgeFailure):
            bridge.validate_request({"op": "stream_probe",
                                     "config_dir": "/tmp/x"})
        bridge.validate_request({"op": "stream_probe", "thread": "t-1",
                                 "config_dir": "/tmp/x"})
        with self.assertRaises(bridge.BridgeFailure):
            bridge.validate_request({"op": "stream_probe", "thread": "t-1",
                                     "config_dir": "/tmp/x", "extra": 1})

    def test_an_accepted_stream_reports_counts_only(self):
        bridge, client, calls = self._client(records=[
            {"event": "delta.text_append", "seq": 1,
             "payload": {"text": "SECRET"}},
            {"event": "delta.text_append", "seq": 2,
             "payload": {"text": "SECRET"}},
            {"event": "run.status", "payload": {"status": "working"}},
        ])
        answer = bridge.Adapter(client, {}).dispatch(
            {"op": "stream_probe", "thread": "thread-9"})
        self.assertEqual(answer, {"thread": "thread-9", "accepted": True,
                                  "events": 3})
        self.assertEqual(calls[0], ("chat.stream_events",
                                    {"session_id": "thread-9"}))
        self.assertNotIn("SECRET", str(answer))

    def test_a_refused_stream_tries_each_candidate_then_reports(self):
        bridge, client, calls = self._client(error=RuntimeError("no"))
        answer = bridge.Adapter(client, {}).dispatch(
            {"op": "stream_probe", "thread": "thread-9"})
        self.assertEqual(answer, {"thread": "thread-9", "accepted": False,
                                  "events": 0})
        self.assertEqual(len(calls), 4)
        self.assertEqual(calls[1][1], {"thread_id": "thread-9"})


class AttachFetchProbeTests(unittest.TestCase):
    def _client(self, bridge, with_image, answer=b"\x89PNG\r\n\x1a\nrest"):
        from contextlib import contextmanager
        calls = []

        class Gateway:
            def request(self, method, body=None, timeout=None):
                calls.append((method, body))
                return answer

        class Client(FakeClient):
            def history(self, **kwargs):
                attachments = ([{
                    "filename": "shot.png", "mime_type": "image/png",
                    "kind": "image", "bytes_len": 12,
                    "path": "workspace/private/shot.png",
                }] if with_image else [])
                return [{"message_id": "m1", "seq": 1, "role": "user",
                         "text": "look",
                         "raw": {"payload": {"metadata": {
                             "attachments": attachments}}}}]

            @contextmanager
            def _gateway(self, config):
                yield Gateway()

        return Client(), calls

    def test_the_first_image_read_fetches_once_and_logs_no_path(self):
        import pathlib
        import tempfile
        bridge = load_bridge()
        client, calls = self._client(bridge, with_image=True)
        with tempfile.TemporaryDirectory() as config_dir:
            request = {"op": "history", "thread": "t-1", "limit": 20,
                       "config_dir": config_dir}
            rows = bridge.Adapter(client, {}).dispatch(request)
            self.assertEqual(rows[0]["attachments"][0]["filename"],
                             "shot.png")
            self.assertNotIn("path", rows[0]["attachments"][0])
            self.assertEqual(calls, [("fs.read", {
                "path": "workspace/private/shot.png"})])
            bridge.Adapter(client, {}).dispatch(request)
            self.assertEqual(len(calls), 1, "the probe is one-shot")
            log = (pathlib.Path(config_dir) / "op-timing.log").read_text(
                encoding="utf-8")
            self.assertIn("attach-fetch", log)
            self.assertIn("magic=png", log)
            self.assertNotIn("workspace/private", log)

    def test_a_read_without_an_image_leaves_the_probe_armed(self):
        import tempfile
        bridge = load_bridge()
        plain, plain_calls = self._client(bridge, with_image=False)
        imaged, imaged_calls = self._client(bridge, with_image=True)
        with tempfile.TemporaryDirectory() as config_dir:
            request = {"op": "history", "thread": "t-1", "limit": 20,
                       "config_dir": config_dir}
            bridge.Adapter(plain, {}).dispatch(request)
            self.assertEqual(plain_calls, [])
            bridge.Adapter(imaged, {}).dispatch(request)
            self.assertEqual(len(imaged_calls), 1)


class PasteSendTests(unittest.TestCase):
    PNG = b"\x89PNG\r\n\x1a\n" + b"0" * 40

    def _client(self, bridge, landed_bytes_len=None):
        from contextlib import contextmanager
        calls = {"writes": [], "opened": []}
        png_len = landed_bytes_len if landed_bytes_len is not None else len(self.PNG)

        class Gateway:
            def call_json(self, method, body=None, timeout=None):
                calls["writes"].append((method, body))
                return {}

            def _open(self, method, body=None):
                calls["opened"].append((method, body))
                return 1

        class Client(FakeClient):
            def __init__(self):
                self.sent = False

            def history(self, **kwargs):
                if not calls["opened"]:
                    return []
                attachments = [{
                    "filename": "pasted-images/pasted-1-1.png",
                    "mime_type": "image/png", "kind": "image",
                    "bytes_len": png_len, "width": 4, "height": 4,
                }]
                return [{"message_id": "m-new", "seq": 9, "role": "user",
                         "text": "look at this",
                         "raw": {"payload": {"metadata": {
                             "attachments": attachments}}}}]

            @contextmanager
            def _gateway(self, config):
                yield Gateway()

        return Client(), calls

    def _stage(self, config_dir, data=None):
        import pathlib
        staging = pathlib.Path(config_dir) / "paste-staging"
        staging.mkdir(parents=True)
        (staging / "pasted-1-1.png").write_bytes(
            self.PNG if data is None else data)

    def test_validation_rejects_paths_and_empty_image_lists(self):
        bridge = load_bridge()
        base = {"op": "send_images", "config_dir": "/tmp/x",
                "message": "", "thread": "t-1"}
        with self.assertRaises(bridge.BridgeFailure):
            bridge.validate_request({**base, "images": []})
        with self.assertRaises(bridge.BridgeFailure):
            bridge.validate_request(
                {**base, "images": ["../pasted-1-1.png"]})
        with self.assertRaises(bridge.BridgeFailure):
            bridge.validate_request({**base, "images": ["notes.txt"]})
        bridge.validate_request({**base, "images": ["pasted-1-1.png"]})
        with self.assertRaises(bridge.BridgeFailure):
            bridge.validate_request({
                **base, "images": ["pasted-1-1.png"], "reply_to": "x" * 257})

    def test_a_quoted_image_send_carries_the_reply_target(self):
        # The image path builds its own stream body; a quoted turn has to
        # reach that one too, or a picture sent with Reply would lose it.
        import tempfile
        bridge = load_bridge()
        client, calls = self._client(bridge)
        with tempfile.TemporaryDirectory() as config_dir:
            self._stage(config_dir)
            bridge.Adapter(client, {}).dispatch({
                "op": "send_images", "thread": "thread-9",
                "message": "look at this", "images": ["pasted-1-1.png"],
                "reply_to": "m-9", "config_dir": config_dir})
        method, body = calls["opened"][0]
        self.assertEqual(method, "chat.stream")
        self.assertEqual(body["session_id"], "thread-9")
        self.assertEqual(body["in_reply_to"], "m-9")

    def test_a_staged_image_is_written_sent_and_verified(self):
        import tempfile
        bridge = load_bridge()
        client, calls = self._client(bridge)
        with tempfile.TemporaryDirectory() as config_dir:
            self._stage(config_dir)
            answer = bridge.Adapter(client, {}).dispatch({
                "op": "send_images", "thread": "thread-9",
                "message": "look at this", "images": ["pasted-1-1.png"],
                "config_dir": config_dir})
        self.assertTrue(answer["sent"])
        self.assertTrue(answer["confirmed"])
        self.assertFalse(answer["stored_mismatch"])
        method, body = calls["writes"][0]
        self.assertEqual(method, "fs.write")
        self.assertEqual(body["path"], "pasted-images/pasted-1-1.png")
        self.assertTrue(body["overwrite"])
        self.assertFalse(body["append"])
        self.assertTrue(body["create_parent"])
        import base64
        self.assertEqual(base64.b64decode(body["data_base64"]), self.PNG)
        method, body = calls["opened"][0]
        self.assertEqual(method, "chat.stream")
        self.assertEqual(body["session_id"], "thread-9")
        self.assertEqual(body["items"][0],
                         {"type": "text", "text": "look at this"})
        self.assertEqual(body["items"][1],
                         {"type": "file",
                          "path": "pasted-images/pasted-1-1.png"})

    def test_a_different_stored_size_is_a_mismatch_not_a_confirmation(self):
        import tempfile
        bridge = load_bridge()
        client, calls = self._client(bridge, landed_bytes_len=999)
        with tempfile.TemporaryDirectory() as config_dir:
            self._stage(config_dir)
            answer = bridge.Adapter(client, {}).dispatch({
                "op": "send_images", "thread": "thread-9",
                "message": "look at this", "images": ["pasted-1-1.png"],
                "config_dir": config_dir})
        self.assertTrue(answer["sent"])
        self.assertFalse(answer["confirmed"])
        self.assertTrue(answer["stored_mismatch"])

    def test_a_missing_or_non_image_staging_file_is_refused(self):
        import tempfile
        bridge = load_bridge()
        client, calls = self._client(bridge)
        with tempfile.TemporaryDirectory() as config_dir:
            with self.assertRaises(bridge.BridgeFailure):
                bridge.Adapter(client, {}).dispatch({
                    "op": "send_images", "thread": "thread-9",
                    "message": "", "images": ["pasted-1-1.png"],
                    "config_dir": config_dir})
            self._stage(config_dir, data=b"not an image at all")
            with self.assertRaises(bridge.BridgeFailure):
                bridge.Adapter(client, {}).dispatch({
                    "op": "send_images", "thread": "thread-9",
                    "message": "", "images": ["pasted-1-1.png"],
                    "config_dir": config_dir})
        self.assertEqual(calls["writes"], [])


class AttachProbeBinaryTests(unittest.TestCase):
    def test_the_binary_leg_writes_the_verified_shape(self):
        # The write shape is no longer a ladder of guesses: a live
        # probe (Nov 18, 2026) settled {path, data_base64, overwrite,
        # append, create_parent}, and a 68-byte PNG written in that
        # shape stats back at exactly 68 bytes. The probe keeps one
        # candidate so the verdict still names what ran.
        import base64
        from contextlib import contextmanager
        bridge = load_bridge()
        png_len = len(base64.b64decode(bridge.PROBE_PNG_B64))
        writes = []
        opened = []

        class Gateway:
            def call_json(self, method, body=None, timeout=None):
                writes.append((method, body))
                return {}

            def _open(self, method, body=None):
                opened.append((method, body))
                return 1

        class Client(FakeClient):
            def history(self, **kwargs):
                rows = []
                for _method, body in opened:
                    text = body["items"][0]["text"]
                    if text == bridge.ATTACH_PROBE_MESSAGE:
                        rows.append({
                            "message_id": "probe-text", "seq": 1,
                            "role": "user", "text": text,
                            "raw": {"payload": {"metadata": {"attachments": [
                                {"filename": bridge.ATTACH_PROBE_PATH,
                                 "mime_type": "text/plain",
                                 "bytes_len": 12}]}}}})
                        continue
                    rows.append({
                        "message_id": "probe-bin-0",
                        "seq": 2, "role": "user", "text": text,
                        "raw": {"payload": {"metadata": {
                            "attachments": [{
                                "filename": "attach-probe-bin-0.png",
                                "mime_type": "image/png",
                                "bytes_len": png_len,
                                "width": 1, "height": 1}]}}}})
                return rows

            @contextmanager
            def _gateway(self, config):
                yield Gateway()

        answer = bridge.Adapter(Client(), {}).dispatch(
            {"op": "attach_probe", "thread": "thread-9"})
        verdicts = {v["candidate"]: v["verdict"] for v in answer["binary"]}
        self.assertEqual(verdicts, {0: "stored_exact"})
        bodies = [body for _m, body in writes
                  if body["path"].startswith("attach-probe-bin")]
        self.assertEqual(len(bodies), 1)
        self.assertEqual(bodies[0]["data_base64"], bridge.PROBE_PNG_B64)
        self.assertTrue(bodies[0]["overwrite"])
        self.assertFalse(bodies[0]["append"])
        self.assertTrue(bodies[0]["create_parent"])

    def test_dictate_posts_raw_pcm_with_a_sample_rate(self):
        # voice.dictation (Mac route table): raw audio bytes in the
        # body, the rate in the query the path template names. The
        # fake records the framing; the transcript rides back.
        import base64
        from contextlib import contextmanager
        bridge = load_bridge()
        pcm = bytes(range(256)) * 64
        seen = {}
        class Gateway:
            def request(self, method, body=None, **kwargs):
                seen["method"] = method
                seen["body"] = body
                seen["kwargs"] = kwargs
                return {"transcript": "hello there"}
        class Client(FakeClient):
            @contextmanager
            def _gateway(self, config):
                yield Gateway()
        answer = bridge.Adapter(Client(), {}).dispatch(
            {"op": "dictate", "pcm_b64": base64.b64encode(pcm).decode("ascii"),
             "sample_rate_hz": 16000})
        self.assertEqual(seen["method"], "voice.dictation")
        self.assertEqual(seen["body"], pcm)
        self.assertEqual(seen["kwargs"].get("path_params"),
                         {"sample_rate_hz": "16000"})
        self.assertEqual(answer["transcript"], "hello there")

    def test_dictate_reports_an_empty_answer_as_no_words(self):
        # Dictation is a noise-mode route: success may carry nothing.
        # No words is an answer, not a transport failure.
        from contextlib import contextmanager
        import base64
        bridge = load_bridge()
        class Gateway:
            def request(self, method, body=None, **kwargs):
                return None
        class Client(FakeClient):
            @contextmanager
            def _gateway(self, config):
                yield Gateway()
        answer = bridge.Adapter(Client(), {}).dispatch(
            {"op": "dictate", "pcm_b64": base64.b64encode(b"\x00\x01").decode("ascii"),
             "sample_rate_hz": 16000})
        self.assertIsNone(answer["transcript"])
        self.assertEqual(answer["keys"], [])

    def test_dictate_validation_rejects_guesswork(self):
        import base64
        bridge = load_bridge()
        for bad in ({"op": "dictate"},
                    {"op": "dictate", "pcm_b64": "!!!", "sample_rate_hz": 16000},
                    {"op": "dictate",
                     "pcm_b64": base64.b64encode(b"\x00").decode("ascii"),
                     "sample_rate_hz": 44100},
                    {"op": "dictate",
                     "pcm_b64": base64.b64encode(b"\x00").decode("ascii"),
                     "sample_rate_hz": 16000, "extra": 1}):
            with self.assertRaises(bridge.BridgeFailure):
                bridge.validate_request(dict(bad))

    def test_link_preview_posts_the_url_and_projects_fields(self):
        # link_preview (Mac route table, POST /api/link-preview): the
        # url goes out as JSON, the card fields ride back. The shape is
        # lenient -- every field is optional -- because the response
        # shape is not recorded, only the route and the body.
        from contextlib import contextmanager
        bridge = load_bridge()
        seen = {}
        class Gateway:
            def call_json(self, method, body=None, **kwargs):
                seen["method"] = method
                seen["body"] = body
                return {"title": "Ovenbird Cookware",
                        "description": "Cast iron goods",
                        "image_url": "https://example.com/pot.jpg",
                        "extra": {"nested": True}}
        class Client(FakeClient):
            @contextmanager
            def _gateway(self, config):
                yield Gateway()
        answer = bridge.Adapter(Client(), {}).dispatch(
            {"op": "link_preview", "url": "https://example.com/pot"})
        self.assertEqual(seen["method"], "link_preview")
        self.assertEqual(seen["body"], {"url": "https://example.com/pot"})
        self.assertEqual(answer["title"], "Ovenbird Cookware")
        self.assertEqual(answer["description"], "Cast iron goods")
        self.assertEqual(answer["image_url"], "https://example.com/pot.jpg")
        self.assertIn("title", answer["keys"])

    def test_link_preview_projects_a_wordless_answer_as_empty(self):
        from contextlib import contextmanager
        bridge = load_bridge()
        class Gateway:
            def call_json(self, method, body=None, **kwargs):
                return {"unrelated": 1}
        class Client(FakeClient):
            @contextmanager
            def _gateway(self, config):
                yield Gateway()
        answer = bridge.Adapter(Client(), {}).dispatch(
            {"op": "link_preview", "url": "https://example.com/pot"})
        self.assertIsNone(answer["title"])
        self.assertIsNone(answer["image_url"])
        self.assertEqual(answer["keys"], ["unrelated"])

    def test_link_preview_validation_rejects_guesswork(self):
        bridge = load_bridge()
        for bad in ({"op": "link_preview"},
                    {"op": "link_preview", "url": "ftp://example.com/x"},
                    {"op": "link_preview", "url": "https://example.com/x",
                     "profile": "ok", "extra": 1},
                    {"op": "link_preview", "url": ""}):
            with self.assertRaises(bridge.BridgeFailure):
                bridge.validate_request(dict(bad))
