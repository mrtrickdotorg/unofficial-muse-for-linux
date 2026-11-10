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

    def test_tasks_uses_only_two_read_routes_and_projects_types(self):
        from contextlib import contextmanager
        bridge = load_bridge()
        calls = []
        class Gateway:
            def call_json(self, method):
                calls.append(method)
                if method == "tasks.list":
                    return {"schedules": [{"id": "job-1", "title": "Daily", "enabled": True,
                        "timezone": "UTC", "next_run_at_utc": 123, "secret": "SECRET"}], "invalid": []}
                if method == "tasks.runs":
                    return {"runs": [{"run_id": "run-1", "job_id": "job-1", "status": "success",
                        "result_summary": "Done", "error_text": None, "scheduled_for_utc": 120, "raw": "SECRET"}]}
                raise AssertionError("nonallowlisted route")
        class Client(FakeClient):
            @contextmanager
            def _gateway(self, config):
                yield Gateway()
        result = bridge.Adapter(Client(), {}).dispatch({"op": "tasks"})
        self.assertEqual(calls, ["tasks.list", "tasks.runs"])
        self.assertEqual(result, {"schedules": [{"id": "job-1", "title": "Daily", "enabled": True,
            "timezone": "UTC", "next_run_at_utc": 123}], "runs": [{"run_id": "run-1", "job_id": "job-1",
            "status": "success", "result_summary": "Done", "error_text": None, "scheduled_for_utc": 120}]})

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
                    {"node": {"artifact_ref": "file:workspace/b.pdf", "display_name": "b.pdf",
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
        # The cursor and every unlisted node field stay in the bridge.
        self.assertNotIn("SECRET", str(result))
        bridge.validate_request({"op": "artifacts", "config_dir": "/unused"})
        with self.assertRaises(bridge.BridgeFailure):
            bridge.validate_request({"op": "artifacts", "config_dir": "/unused", "limit": 1})

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

    def test_real_client_waits_for_gateway_response_before_closing_dispatch(self):
        # Regression: upstream fire-and-forget closes Noise immediately after
        # writing the request; the live prompt never reached shared history.
        import types
        from unittest.mock import patch
        from contextlib import contextmanager
        bridge = load_bridge()
        events = []
        class Gateway:
            def request(self, method, **kwargs):
                events.append((method, kwargs))
                return b'{"ok":true}'
        class BaseClient:
            def __init__(self, config_loader):
                self.config_loader = config_loader
            @contextmanager
            def _gateway(self, config):
                yield Gateway()
                events.append('closed')
            def send(self, **kwargs):
                raise AssertionError('must not use unacknowledged fire-and-forget')
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

    def test_connect_projects_only_typed_identity_and_threads(self):
        bridge = load_bridge()
        response = bridge.Adapter(FakeClient(), {}).dispatch({"op": "connect"})
        self.assertEqual(response, {"identity_name": "Test User", "vm_id": "vm-1",
            "threads": [{"session_id": "thread-1", "title": "Example",
                         "archived": False, "updated_at": "2026-10-17T00:00:00Z"}]})
        self.assertNotIn("SECRET", str(response))


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


if __name__ == "__main__":
    unittest.main()


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
        self.assertEqual(calls, ["feed.list"])
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
        self.assertEqual(calls, ["feed.list"])
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
