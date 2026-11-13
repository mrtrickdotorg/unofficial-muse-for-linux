#!/usr/bin/env python3
"""Typed, credential-contained adapter for an external community muse-mcp install.

One stdin request/one stdout response. Never start an MCP process or expose a raw
route. Imports of the community library are deferred until explicit execution.
"""


import json
import fcntl
import os
import re
from datetime import datetime, timezone
from html.parser import HTMLParser
from contextlib import contextmanager
from pathlib import Path
import secrets
import stat
import threading


MAX_REQUEST = 1024 * 1024
MAX_RESPONSE = 4 * 1024 * 1024


# Requests whose upstream calls are cheap only on a reused connection. One
# handshake costs 1.5-2.2s and a call on an open connection ~0.1s, so these are
# the operations where the reuse pays for itself.
SHARED_GATEWAY_OPS = frozenset({"send", "watch", "tasks", "delete_task", "start_session", "session_op"})

# The session mutations the recorded route table names, with the
# body shapes its own consumer sends (docs/PROTOCOL.md in the pin):
# rename is a flat {session_id, title}; the rest ride
# {method: "/api/session/<kind>", session_id}.
SESSION_OP_KINDS = frozenset({"pin", "unpin", "archive", "unarchive", "delete", "rename"})


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


def timing_log(config_dir, line):
    """Append one timing line to <config_dir>/op-timing.log.

    Deliberately content-free: op names, durations and row counts only --
    never message text, conversation ids, or credentials. This is the
    instrument that ended the Nov 10 guessing about where a round trip
    goes: the service answered in ~3s while the app took minutes, and
    every layer's own account of itself looked fine.
    """
    if not config_dir:
        return
    try:
        import time
        path = os.path.join(str(config_dir), "op-timing.log")
        with open(path, "a") as handle:
            handle.write("{} {}\n".format(int(time.time() * 1000), line))
    except OSError:
        pass


_WIDGET_PROBED = set()


def _key_paths(value, prefix="", depth=0, out=None):
    """Every key path in a decoded JSON value, values never included:
    dict keys joined with dots, a list shown as [] over its first item's
    shape. The widget probe logs these so an unrecorded payload shape
    can be learned without logging any content."""
    if out is None:
        out = []
    if depth > 5:
        return out
    if isinstance(value, dict):
        for key in sorted(value.keys()):
            path = "{}.{}".format(prefix, key) if prefix else str(key)
            out.append(path)
            _key_paths(value[key], path, depth + 1, out)
    elif isinstance(value, list) and value:
        _key_paths(value[0], prefix + "[]", depth + 1, out)
    return out


def _probe_widget_payload(config_dir, raw):
    """Reconnaissance for the in-app option buttons (Patrick, Nov 11):
    the service's route table records no widget route, so the clickable
    options an assistant message carries must ride inside the history
    payload, in a field this projection has never read. When a row's
    text carries a widget token, record the raw event's key structure
    — keys only, never values; the row is already redacted upstream —
    once per distinct shape, so the field can be identified from the
    timing log and read properly afterwards."""
    if not config_dir:
        return
    signature = ",".join(_key_paths(raw))
    if signature in _WIDGET_PROBED:
        return
    _WIDGET_PROBED.add(signature)
    timing_log(config_dir, "widget-probe keys=" + signature[:1900])


_HISTORY_PROBED = set()

# Keys the history projection already consumes (or knows are envelope),
# at the raw-event level and inside its payload. A shape probe that
# fired on these would log every ordinary conversation's shape and
# bury the one it exists to find.
_HISTORY_RAW_KNOWN = {
    "agent_id", "chat_context", "display_text", "display_text_ready",
    "display_text_updated_at_ms", "event", "event_name", "message_id",
    "occurred_at_ms", "parent_agent_id", "parent_message_id", "payload",
    "status",
}
_HISTORY_PAYLOAD_KNOWN = {
    "agent_id", "chat_context", "chat_event_seq", "content", "data",
    "display_text", "display_text_ready", "display_text_updated_at_ms",
    "fallback_text", "host_message_id", "id", "is_thread", "kind",
    "message_id", "metadata", "parent_agent_id", "reply_to_message_id",
    "reply_to_text", "role", "seq", "session_id", "source", "state",
    "status", "transcript_surface", "visibility", "widgets",
}


def _probe_history_shape(config_dir, raw, payload):
    """Reconnaissance for message attachments (Patrick, Nov 13,
    2026): the route table records no upload or attachment route,
    and the history projection reads only text, status and widget
    options out of a row, so whatever an attached file looks like
    on the service rides inside the raw event unread -- the app
    today shows an attachment as nothing at all. When a row's raw
    event carries a key outside the known envelope, record its key
    structure -- keys only, never values -- once per distinct
    shape, so the attachment fields can be identified from the
    timing log and projected properly afterwards, the same way
    the widget options were found."""
    if not config_dir:
        return
    interesting = (
        any(key not in _HISTORY_RAW_KNOWN for key in raw)
        or any(key not in _HISTORY_PAYLOAD_KNOWN for key in payload)
    )
    if not interesting or len(_HISTORY_PROBED) > 12:
        return
    signature = ",".join(_key_paths(raw))
    if signature in _HISTORY_PROBED:
        return
    _HISTORY_PROBED.add(signature)
    timing_log(config_dir, "history-probe keys=" + signature[:1900])


_SUBAGENTS_PROBED = False


def _probe_subagents(config_dir, client, config):
    """Reconnaissance for the right panel's agent-activity feed: the
    web panel lists what the agent did today, which this service
    exposes (if anywhere) through subagents.list — a route that
    answered gateway probes in November, but whose response shape the
    community client never recorded. Log its key paths (keys only,
    never values) so the feed can be built against the real shape.
    Runs on a daemon thread off the tasks answer it rides along with,
    and any failure is the probe's alone."""
    global _SUBAGENTS_PROBED
    if _SUBAGENTS_PROBED or not config_dir:
        return
    _SUBAGENTS_PROBED = True

    def run():
        try:
            with client._gateway(config) as gateway:
                payload = gateway.call_json("subagents.list", timeout=20)
        except Exception:
            return
        signature = ",".join(_key_paths(payload))
        timing_log(config_dir, "subagents-probe keys=" + signature[:1900])

    threading.Thread(target=run, daemon=True).start()


_FEED_PROBED = False


def _feed_units(payload):
    """Up to a few raw unit dicts from a feed payload, for shape probing."""
    units = []
    if isinstance(payload, dict) and isinstance(payload.get("days"), list):
        for day in payload["days"]:
            if not isinstance(day, dict):
                continue
            editions = day.get("editions")
            if not isinstance(editions, list):
                continue
            for edition in editions:
                if not isinstance(edition, dict):
                    continue
                rows = edition.get("units")
                if isinstance(rows, list):
                    units.extend(u for u in rows if isinstance(u, dict))
    elif isinstance(payload, dict):
        for key in ("units", "items", "results", "data"):
            if isinstance(payload.get(key), list):
                units.extend(u for u in payload[key] if isinstance(u, dict))
                break
    elif isinstance(payload, list):
        units.extend(u for u in payload if isinstance(u, dict))
    return units[:6]


def _probe_feed_shapes(config_dir, client, config, payload):
    """Reconnaissance for the web-parity Feed page (Patrick, Nov 12):
    the web feed renders a per-post emoji, a hero image and a prompt
    card, none of whose field names the community client ever recorded.
    Log the key paths of the list payload and of the first few units
    (keys only, never values — later units carry optional fields like
    attachments that the first may lack), and, on a daemon thread once
    per host process, the prompt and status payloads' key paths, so
    the page can be built against the real shapes."""
    global _FEED_PROBED
    if _FEED_PROBED or not config_dir:
        return
    _FEED_PROBED = True
    timing_log(config_dir, "feed-probe keys=" + ",".join(_key_paths(payload))[:1900])
    seen = set()
    for unit in _feed_units(payload):
        signature = ",".join(_key_paths(unit))
        if signature and signature not in seen:
            seen.add(signature)
            timing_log(config_dir, "feed-probe unit keys=" + signature[:1900])
        if len(seen) >= 3:
            break

    def run():
        for route, label in (("feed.prompt.get", "feed-prompt-probe"),
                             ("feed.status", "feed-status-probe")):
            try:
                with client._gateway(config) as gateway:
                    answer = gateway.call_json(route, timeout=20)
            except Exception:
                continue
            signature = ",".join(_key_paths(answer))
            if signature:
                timing_log(config_dir, label + " keys=" + signature[:1900])

    threading.Thread(target=run, daemon=True).start()


def _widget_options(payload):
    """The options a history row's widget offers, when it offers any.

    Shape confirmed by the widget probe (Nov 12, 2026):
    payload.widgets[] entries carry data.options (a list of plain
    strings) and, once the reader has answered, state.data.selectedText.
    Only the first options-bearing widget projects; other widget kinds
    stay invisible here rather than guessed at.
    """
    widgets = payload.get("widgets")
    if not isinstance(widgets, list):
        return None
    for widget in widgets:
        if not isinstance(widget, dict):
            continue
        data = widget.get("data") if isinstance(widget.get("data"), dict) else {}
        options = data.get("options")
        if not isinstance(options, list):
            continue
        texts = [text for text in (optional_text(o) for o in options) if text]
        if not texts:
            continue
        state = widget.get("state") if isinstance(widget.get("state"), dict) else {}
        sdata = state.get("data") if isinstance(state.get("data"), dict) else {}
        return {"options": texts[:12],
                "selected": optional_text(sdata.get("selectedText"))}
    return None


def _nonneg_int(value):
    return value if isinstance(value, int) and not isinstance(value, bool) \
        and value >= 0 else None


def _message_attachments(payload):
    """Project a message's attachments (probe, Nov 13, 2026: they
    ride in `payload.metadata.attachments` with filename, mime_type,
    kind, bytes_len, width/height for images, caption, transcription,
    a service-side path and a sha256). The path and the hash stay
    behind -- the card shows what the reader needs (name, type,
    size, dimensions, caption) -- and strings pass the content
    redactor like every other display string. A row with no
    attachments projects nothing, keeping the wire shape unchanged
    for ordinary messages."""
    metadata = payload.get("metadata") if isinstance(
        payload.get("metadata"), dict) else {}
    raw_list = metadata.get("attachments") if isinstance(
        metadata.get("attachments"), list) else []
    attachments = []
    for item in raw_list[:8]:
        if not isinstance(item, dict):
            continue
        filename = content_text(item.get("filename"))
        if not filename:
            continue
        attachment = {
            "filename": filename,
            "mime_type": optional_text(item.get("mime_type")),
            "kind": optional_text(item.get("kind")),
            "bytes_len": _nonneg_int(item.get("bytes_len")),
            "width": _nonneg_int(item.get("width")),
            "height": _nonneg_int(item.get("height")),
            "caption": content_text(item.get("caption")),
        }
        transcription = content_text(item.get("transcription"))
        if transcription:
            attachment["transcription"] = transcription[:500]
        if attachment["caption"]:
            attachment["caption"] = attachment["caption"][:500]
        attachments.append(attachment)
    return attachments


def history_rows(rows, config_dir=None):
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
        widget = _widget_options(payload)
        if widget:
            message["options"] = widget["options"]
            if widget["selected"]:
                message["selected_option"] = widget["selected"]
        attachments = _message_attachments(payload)
        if attachments:
            message["attachments"] = attachments
        if config_dir and "[[hatch_widget" in message["text"] and raw:
            _probe_widget_payload(config_dir, raw)
        if config_dir and raw:
            _probe_history_shape(config_dir, raw, payload)
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
               "activity": {"timeout"},
               "cancel": set(),
               "approvals": set(),
               "decide": {"approval_id", "decision"},
               "permissions": set(),
               "revoke": {"permission_id"},
               "delete_task": {"job_id"},
               "start_session": set(),
               "session_op": {"session_id", "kind", "title"},
               "tasks": set(),
               "artifacts": set(),
               "ideas": set(),
               "goals": set(),
               "feed": set(),
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
    if op in ("watch", "activity"):
        timeout = request.get("timeout")
        if not isinstance(timeout, int) or isinstance(timeout, bool) or not 1 <= timeout <= 60000:
            raise BridgeFailure("invalid_request")
        # A status read is a bounded listen, not a long-poll: keep it short so
        # the shared control connection cannot be pinned for a whole minute.
        if op == "activity" and timeout > 30000:
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
    if op == "decide":
        approval_id = request.get("approval_id")
        if (not isinstance(approval_id, str) or not 1 <= len(approval_id) <= 256
                or "\x00" in approval_id):
            raise BridgeFailure("invalid_request")
        # Refuse an outcome the service never published rather than guessing a
        # mapping: a decision is a write, and a wrong word is not recoverable.
        if request.get("decision") not in DECISIONS:
            raise BridgeFailure("invalid_request")
    if op == "revoke":
        # The id is the service's own, taken from the active listing it
        # published; anything else would be revoking a guess.
        permission_id = request.get("permission_id")
        if (not isinstance(permission_id, str) or not 1 <= len(permission_id) <= 256
                or "\x00" in permission_id):
            raise BridgeFailure("invalid_request")
    if op == "delete_task":
        # The id is the service's own, taken from the schedule listing it
        # published; anything else would be deleting a guess.
        job_id = request.get("job_id")
        if (not isinstance(job_id, str) or not 1 <= len(job_id) <= 256
                or "\x00" in job_id):
            raise BridgeFailure("invalid_request")
    if op == "session_op":
        # Same rule: the id comes from the session listing the
        # service published, the kind from the recorded route table,
        # and a title belongs to rename alone -- required there,
        # refused everywhere else, so no op can smuggle fields into
        # another op's body.
        session_id = request.get("session_id")
        if (not isinstance(session_id, str) or not 1 <= len(session_id) <= 256
                or "\x00" in session_id):
            raise BridgeFailure("invalid_request")
        kind = request.get("kind")
        if kind not in SESSION_OP_KINDS:
            raise BridgeFailure("invalid_request")
        title = request.get("title")
        if kind == "rename":
            if (not isinstance(title, str) or not title.strip()
                    or len(title) > 300 or "\x00" in title):
                raise BridgeFailure("invalid_request")
        elif title is not None:
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
            # Dispatch only: open the chat stream, write the prompt, return.
            # An earlier version read the stream's response to its end to
            # keep the connection alive until the server processed the
            # request — but that response IS the agent's turn, so every send
            # waited out the whole reply before the app even learned the
            # dispatch had completed, and a quiet stream could hold it far
            # longer. The write itself is synchronous, and this send runs
            # inside the bridge's send operation, whose confirmation loop
            # keeps reading history on this same connection for seconds
            # after the dispatch: the connection closing instantly after
            # the write — the failure that once lost a live prompt — cannot
            # happen on this path. History below, not this transport
            # response, proves persistence.
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
                gateway._open("chat.stream", body=body)
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


def activity_rows(client, config, timeout_ms):
    """Bounded live-status read: what the agent is doing, not what it said.

    The pinned CLI records that threaded replies never arrive as live events
    and that live chat events use `delta.*` shapes, so a status surface listens
    here while the transcript keeps polling history. Only whitelisted fields
    leave this function: a live payload can carry more than a status line, and
    an allowlist is the difference between showing progress and echoing chat.
    """
    gateway = getattr(client, "_gateway", None)
    if gateway is None:
        raise BridgeFailure("bridge_unavailable")
    seconds = timeout_ms / 1000.0
    with gateway(config) as connection:
        records = connection.subscribe_json(
            "chat.subscribe", body={"capabilities": {}},
            max_records=200, idle_timeout=min(6.0, seconds), overall_timeout=seconds)
    rows = []
    for record in records:
        if not isinstance(record, dict):
            continue
        event = record.get("event")
        if not isinstance(event, str) or not 1 <= len(event) <= 64:
            continue
        payload = record.get("payload") if isinstance(record.get("payload"), dict) else {}
        text = payload.get("display_text") or payload.get("text") or payload.get("content")
        seq = record.get("seq")
        rows.append({
            "event": event,
            "seq": seq if isinstance(seq, int) and not isinstance(seq, bool) else None,
            "text": text[:200] if isinstance(text, str) else None,
            "status": optional_text(payload.get("status") or payload.get("activity_text")),
            "session_id": optional_text(payload.get("session_id")),
        })
        if len(rows) >= 200:
            break
    return {"events": rows}


def cookie_header(config):
    """The jar as a Cookie header, for a service that wants text not a path.

    `secure_config` sealed an HttpOnly-compatible copy at this descriptor, so the
    jar is read from memory here and nothing is written back to disk.
    """
    path = config.get("cookies_file")
    if not isinstance(path, str) or not path:
        raise BridgeFailure("invalid_request")
    try:
        with open(path, encoding="utf-8", errors="replace") as file:
            text = file.read(MAX_REQUEST)
    except OSError:
        raise BridgeFailure("auth_required") from None
    pairs = []
    for line in text.splitlines():
        if not line or line.startswith("#"):
            continue
        fields = line.split("\t")
        if len(fields) == 7 and fields[5] and fields[6]:
            pairs.append("{}={}".format(fields[5], fields[6]))
    if not pairs:
        raise BridgeFailure("auth_required")
    return "; ".join(pairs)


# A skipped notification must never be accepted as a reply. The parent loop
# matches on `stream_id`, and a fresh gateway's requests start at 0, so an
# unmarked empty frame was indistinguishable from a reply carrying no body and
# surfaced as `{}` — which `approval_rows` could only report as a failure, and
# that a caller could misread as "nothing is pending".
SKIPPED_FRAME_STREAM = (1 << 63) - 1


def skipped_frame(pinned):
    """An empty frame whose stream id no request can own."""
    frame = pinned.ServiceFrame()
    frame.stream_id = SKIPPED_FRAME_STREAM
    return frame


def sentinel_gateway_class(pinned):
    """A gateway to the sentinel service, which is the only one with approvals.

    `muse_mcp.client`, which this bridge otherwise imports, always addresses the
    daemon service, so `egress.approvals` dies there with a frame mismatch.
    `muse.py` routes by the route's declared service, but its reader parses every
    frame as a `ServiceResponse`. Two different things on that wire are not one:
    a notification, which is not a reply at all, and a chunk of a reply too
    large for a single Noise frame. Only the reader is replaced here: it
    reassembles chunks before parsing, and skips whatever still is not a reply,
    because the parent request loop already ignores frames whose `stream_id` was
    not the one it sent.
    """

    class SentinelGateway(pinned.Gateway):
        def __init__(self, *args, **kwargs):
            super().__init__(*args, **kwargs)
            # A live `egress.permissions` body is ~496 KB, so the sentinel service
            # splits one reply across Noise transport chunks sharing a
            # `chunk_id`. Parsing a mid-frame chunk as a whole `ServiceResponse`
            # raises `Wire format was corrupt`; the old guard skipped it, the
            # assembled body lost the bytes in between, and the damage surfaced
            # far away as `non-JSON response` out of `call_json`.
            self._chunk_parts = {}  # chunk_id -> {chunk_index: payload}

        def _complete_payload(self, transport):
            """The frame's payload, once every chunk of it has arrived."""
            payload = bytes(transport.payload)
            total = getattr(transport, "total_chunks", 1)
            total = total if isinstance(total, int) and total > 1 else 1
            if total == 1:
                return payload
            chunk_id = getattr(transport, "chunk_id", 0)
            index = getattr(transport, "chunk_index", 0)
            index = index if isinstance(index, int) and index >= 0 else 0
            parts = self._chunk_parts.setdefault(chunk_id, {})
            parts[index] = payload
            if len(parts) < total:
                return None
            del self._chunk_parts[chunk_id]
            # Arrival order is not guaranteed; `chunk_index` is.
            return b"".join(parts[i] for i in sorted(parts))

        def _read_frame(self):
            # A chunk that only completes a frame is not a frame, so keep reading
            # instead of handing the parent loop something it would discard.
            while True:
                with self._recv_lock:
                    data, _flags = self.ws.recv()
                    point = bytes(self.noise.decrypt(bytes(data)))
                transport = pinned.NoiseTransportFrame()
                transport.ParseFromString(point)
                payload = self._complete_payload(transport)
                if payload is None:
                    continue
                # A notification is neither a `ServiceResponse` nor a
                # `ServiceFrame`. It can fail at the outer parse or at the inner
                # one, so both are guarded; neither failure is a reply to our
                # request, and the parent loop ignores stream ids it did not
                # send, so an empty frame is skipped instead of being fatal.
                try:
                    response = pinned.ServiceResponse()
                    response.ParseFromString(payload)
                    frame = pinned.ServiceFrame()
                    frame.ParseFromString(response.payload)
                except Exception:  # noqa: BLE001 - a notification is not a reply
                    return skipped_frame(pinned)
                return frame

    return SentinelGateway


def sentinel_gateway(config):
    """Open one sentinel connection for a single read."""
    import sys

    for path in fallback_paths():
        if path not in sys.path:
            sys.path.append(path)
    try:
        import muse as pinned
    except (ImportError, OSError) as exc:
        raise BridgeFailure("bridge_unavailable", detail=import_detail(exc, sys.path)) from None
    gateway = sentinel_gateway_class(pinned)
    return gateway(cookies=cookie_header(config), vm_id=config.get("vm_id"))


ARTIFACT_FIELDS = ("artifact_ref", "title", "kind", "format", "status", "placement",
                   "path", "updated_at_ms", "pinned", "can_open", "can_share")


def artifact_rows(payload):
    """Project the Library listing into what a Linux client can act on.

    `artifacts.list` answers 50 edges per page with opaque cursors, so only the
    fields a list and a viewer need travel: the reference the service itself
    uses, the name a person sees, and the capability flags that say what the
    service would allow. The cursor stays in the bridge. A row without a
    reference or a name fails closed: an unnamed artifact can be neither shown
    nor opened, and silently dropping it would understate the Library.
    """
    if not isinstance(payload, dict):
        raise BridgeFailure("protocol_error")
    total = payload.get("total_count")
    edges = payload.get("edges")
    if not isinstance(total, int) or isinstance(total, bool) or total < 0:
        raise BridgeFailure("protocol_error")
    if not isinstance(edges, list) or len(edges) > 200:
        raise BridgeFailure("protocol_error")
    rows = []
    for edge in edges:
        node = edge.get("node") if isinstance(edge, dict) else None
        if not isinstance(node, dict):
            raise BridgeFailure("protocol_error")
        reference = required_text(node.get("artifact_ref"))
        name = required_text(node.get("display_name"))
        if not reference or not name:
            raise BridgeFailure("protocol_error")
        target = node.get("open_target") if isinstance(node.get("open_target"), dict) else {}
        capabilities = node.get("capabilities") if isinstance(node.get("capabilities"), dict) else {}
        updated = node.get("updated_at_ms")
        rows.append({
            "artifact_ref": reference,
            "title": name,
            "kind": optional_text(node.get("type")),
            "format": optional_text(node.get("format")),
            "status": optional_text(node.get("status")),
            "placement": optional_text(node.get("placement")),
            "path": optional_text(target.get("path")),
            "updated_at_ms": updated if isinstance(updated, int)
            and not isinstance(updated, bool) else None,
            "pinned": node.get("is_pinned") is True,
            "can_open": capabilities.get("can_open") is True,
            "can_share": capabilities.get("can_share") is True,
        })
    page = payload.get("page_info") if isinstance(payload.get("page_info"), dict) else {}
    return {"total_count": total, "has_more": page.get("has_next_page") is True,
            "artifacts": rows}


LISTING_ID_KEYS = ("id", "goal_id", "unit_id", "idea_card_id", "ideaCardId", "card_id", "idea_id")
LISTING_TITLE_KEYS = ("title", "name", "headline")
LISTING_BODY_KEYS = ("body", "description", "summary", "text", "content", "detail", "subtitle")
LISTING_KIND_KEYS = ("kind", "type", "status", "state")
LISTING_STAMP_KEYS = ("updated_at", "created_at", "published_at", "date",
                      "updated_at_ms", "created_at_ms")
LISTING_MAX_ROWS = 200
LISTING_MAX_CONTAINER = 500


def project_listing_rows(entries, total_hint=None, extras=None):
    """Project flattened (row, kind fallback, stamp fallback) entries.

    `extras`, when given, is called with each recognized row's source
    dict and returns surface-specific display fields to merge in (the
    feed's kicker and category); it owns its own cleaning and caps.

    Every text value passes the credential redaction applied to task
    content and is length-capped here, so nothing unbounded or
    secret-shaped can ride into the window. A row with neither a title
    nor a body cannot be shown honestly, so it is skipped AND counted:
    the caller reports the skip count, which keeps an unrecognized
    service shape visible instead of masquerading as an empty list.
    """
    rows = []
    skipped = 0
    for entry, kind_fallback, stamp_fallback in entries:
        node = entry.get("node") if isinstance(entry, dict) else None
        row = node if isinstance(node, dict) else entry
        if not isinstance(row, dict):
            skipped += 1
            continue
        title = None
        for key in LISTING_TITLE_KEYS:
            title = content_text(row.get(key))
            if title:
                break
        body = None
        for key in LISTING_BODY_KEYS:
            body = content_text(row.get(key))
            if body:
                break
        if not title and not body:
            skipped += 1
            continue
        if not title:
            title = body[:80]
        identifier = None
        for key in LISTING_ID_KEYS:
            identifier = optional_text(row.get(key))
            if identifier:
                break
        kind = None
        for key in LISTING_KIND_KEYS:
            kind = optional_text(row.get(key))
            if kind:
                break
        stamp = None
        for key in LISTING_STAMP_KEYS:
            stamp = optional_text(row.get(key))
            if stamp:
                break
        projected = {
            "id": identifier,
            "title": title[:300],
            "body": body[:4000] if body else None,
            "kind": kind or kind_fallback,
            "stamp": stamp or stamp_fallback,
        }
        if extras is not None:
            projected.update(extras(row))
        rows.append(projected)
        if len(rows) >= LISTING_MAX_ROWS:
            break
    return {
        "total": total_hint if total_hint is not None else len(entries),
        "recognized": len(rows),
        "skipped": skipped,
        "items": rows,
    }


def listing_container(payload, container_keys):
    """Resolve a listing payload to (flat entries, reported total).

    Accepts a bare list or the first list found under the surface's
    known container keys. Anything else — or an absurd size — fails
    closed rather than reading as an empty surface.
    """
    if isinstance(payload, dict):
        reported = payload.get("total_count", payload.get("total"))
        total = reported if isinstance(reported, int) and not isinstance(
            reported, bool) and reported >= 0 else None
        for key in container_keys:
            if isinstance(payload.get(key), list):
                container = payload[key]
                break
        else:
            raise BridgeFailure("protocol_error")
    else:
        total = None
        container = payload
    if not isinstance(container, list) or len(container) > LISTING_MAX_CONTAINER:
        raise BridgeFailure("protocol_error")
    return [(entry, None, None) for entry in container], total


def listing_rows(payload, container_keys, extras=None):
    """Project a flat read-only listing (goals) into display rows."""
    entries, total = listing_container(payload, container_keys)
    return project_listing_rows(entries, total, extras=extras)


def _iso_from_epoch(value):
    """ISO-8601 (UTC) for a numeric epoch stamp — seconds or
    milliseconds, told apart by magnitude — or the text itself when
    it already is a timestamp. Anything else projects as None."""
    if isinstance(value, str) and value.strip():
        return value.strip()
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return None
    seconds = value / 1000.0 if value > 10**11 else float(value)
    if seconds <= 0:
        return None
    try:
        return datetime.fromtimestamp(seconds, tz=timezone.utc).isoformat()
    except (OverflowError, OSError, ValueError):
        return None


class _WidgetTextExtractor(HTMLParser):
    """Visible text lines from a feed widget's HTML.

    The web renders a widget attachment as live HTML; this client has
    no web engine, so the bridge extracts what the widget SAYS —
    block elements break lines, script/style content never surfaces,
    entities are decoded by the parser. A timeline or countdown
    widget degrades to its labels in order, which carries the
    information even without the styling.
    """

    BLOCK = {"address", "article", "aside", "blockquote", "br", "div",
             "dl", "dt", "dd", "fieldset", "figcaption", "figure",
             "footer", "form", "h1", "h2", "h3", "h4", "h5", "h6",
             "header", "hr", "li", "main", "nav", "ol", "p", "pre",
             "section", "table", "td", "th", "tr", "ul"}
    SKIP = {"script", "style", "noscript", "template"}

    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.lines = []
        self._current = []
        self._skip = 0
        self._anchor = None

    def _flush(self):
        text = " ".join("".join(self._current).split())
        self._current = []
        if text:
            self.lines.append(text)

    def handle_starttag(self, tag, attrs):
        if tag in self.SKIP:
            self._skip += 1
        elif tag == "a" and not self._skip:
            # A link stays a link: its text is buffered and re-emitted
            # as markdown, so the card's rich rendering makes it live.
            href = dict(attrs).get("href") or ""
            if href.startswith(("https://", "http://")) \
                    and not any(c in href for c in " \t\r\n)"):
                self._anchor = (href, [])
        elif tag in self.BLOCK:
            self._flush()

    def handle_endtag(self, tag):
        if tag in self.SKIP and self._skip:
            self._skip -= 1
        elif tag == "a" and self._anchor is not None:
            href, parts = self._anchor
            self._anchor = None
            text = " ".join("".join(parts).split())
            if text:
                self._current.append(f"[{text}]({href})")
        elif tag in self.BLOCK:
            self._flush()

    def handle_data(self, data):
        if self._skip:
            return
        if self._anchor is not None:
            self._anchor[1].append(data)
        else:
            self._current.append(data)

    def text_lines(self):
        self._flush()
        return self.lines


def _widget_text(html):
    """The readable lines of a widget attachment's HTML, joined for
    display — or None when the value is not markup at all (a bare
    reference this client cannot resolve renders as nothing rather
    than as a stray token on the card)."""
    if "<" not in html[:500]:
        return None
    parser = _WidgetTextExtractor()
    try:
        parser.feed(html[:200_000])
    except Exception:
        return None
    lines = []
    for line in parser.text_lines():
        cleaned = content_text(line)
        if cleaned and (not lines or lines[-1] != cleaned):
            lines.append(cleaned[:300])
        if len(lines) >= 40:
            break
    if not lines:
        return None
    return "\n".join(lines)[:1500]


def _feed_extras(unit):
    """The feed-only display fields, under the gateway's own names —
    the shape the feed probe confirmed on Nov 12, 2026: each unit
    carries kicker, category, an `emoji` avatar, a per-unit
    `created_at_ms`, and an optional `attachment` holding either a
    `hero_image_url` (the card's picture) or a `widget_html` (a rich
    card the web renders live; here its readable text is projected
    as `widget_text`). A unit without a field projects without it."""
    extras = {}
    body_md = content_text(unit.get("body_md"))
    if body_md:
        # The unit's description, in markdown: the web renders it
        # above the card's image with its links live, and the app's
        # rich label does the same from this text. It rides as the
        # row's body -- the generic listing keys do not know the
        # feed's `body_md` name, which is why cards showed titles
        # and pictures but no descriptions until now -- capped like
        # every projected body.
        extras["body"] = body_md[:4000]
    kicker = content_text(unit.get("kicker"))
    if kicker:
        extras["kicker"] = kicker[:160]
    category = content_text(unit.get("category"))
    if category:
        extras["category"] = category[:80]
    emoji = content_text(unit.get("emoji"))
    if emoji:
        extras["icon"] = emoji[:16]
    attachment = unit.get("attachment") if isinstance(
        unit.get("attachment"), dict) else {}
    hero = optional_text(attachment.get("hero_image_url"))
    if hero and hero.startswith(("https://", "http://")):
        extras["image_url"] = hero[:2000]
    widget_html = attachment.get("widget_html")
    if isinstance(widget_html, str) and widget_html.strip():
        text = _widget_text(widget_html)
        if text:
            extras["widget_text"] = text
        if "<" in widget_html[:500]:
            # The raw markup rides along for the client's live frame:
            # it renders in a navigation-locked WebView (links hand
            # off to the browser, nothing navigates), with
            # widget_text as the fallback view. Capped so one unit
            # cannot flood the listing.
            extras["widget_html"] = widget_html[:300_000]
    stamp = _iso_from_epoch(unit.get("created_at_ms"))
    if stamp:
        # Overrides the day-date fallback: the card shows the unit's
        # own relative time, like the web feed.
        extras["stamp"] = stamp
    return extras


_IDEAS_PROBED = False


def _probe_ideas_shapes(config_dir, payload):
    """One-shot reconnaissance for the idea-card icon (Patrick,
    Nov 13, 2026): the web draws an icon on every card; the recorded
    consumer reads only id/title/section off the list rows, so the
    field carrying the icon -- if the list payload carries one at
    all -- is unknown. Record the key structure only -- sections'
    and cards' key names, never values -- once per process, so the
    next look at his timing log names the field from evidence."""
    global _IDEAS_PROBED
    if _IDEAS_PROBED or not config_dir:
        return
    _IDEAS_PROBED = True
    try:
        signatures = []
        if isinstance(payload, dict):
            signatures.append("top:" + ",".join(sorted(payload.keys())))
            sections = payload.get("sections")
            cards_seen = 0
            if isinstance(sections, list):
                for section in sections:
                    if not isinstance(section, dict):
                        continue
                    sig = "section:" + ",".join(sorted(section.keys()))
                    if sig not in signatures:
                        signatures.append(sig)
                    cards = section.get("cards")
                    if not isinstance(cards, list):
                        cards = section.get("ideas")
                    if isinstance(cards, list):
                        for card in cards:
                            if not isinstance(card, dict):
                                continue
                            cards_seen += 1
                            sig = "card:" + ",".join(sorted(card.keys()))
                            if sig not in signatures and len(signatures) < 5:
                                signatures.append(sig)
            signatures.append("cards={}".format(cards_seen))
        timing_log(config_dir, "ideas-probe " + " | ".join(signatures)[:1500])
    except Exception:
        pass


def _ideas_extras(row):
    """Display fields for an idea card beyond the shared shape --
    the icon the web draws at the card's left edge, under the same
    two names the feed payload carries them by (an `emoji` and an
    `icon_url`): the shared listing shape has slots for both.
    Unknown until the shape probe names the fields from a live
    payload; absent fields simply leave the slots empty."""
    extras = {}
    icon = row.get("emoji")
    if isinstance(icon, str) and icon.strip():
        extras["icon"] = icon.strip()[:16]
    image = row.get("icon_url")
    if isinstance(image, str) and image.startswith(("https://", "http://")):
        extras["image_url"] = image.strip()[:2000]
    return extras


def ideas_rows(payload):
    """Project the idea-cards listing: cards nest inside sections.

    The service answers `{sections: [{title, cards|ideas: [...]}]}` — the
    shape the community client itself reads — so the section title rides
    along as each card's kind when the card names none of its own. A flat
    cards/items payload is still accepted, in case the shape flattens.
    """
    if isinstance(payload, dict) and isinstance(payload.get("sections"), list):
        sections = payload["sections"]
        if len(sections) > LISTING_MAX_CONTAINER:
            raise BridgeFailure("protocol_error")
        entries = []
        for section in sections:
            if not isinstance(section, dict):
                continue
            cards = section.get("cards")
            if not isinstance(cards, list):
                cards = section.get("ideas")
            if not isinstance(cards, list):
                continue
            heading = optional_text(section.get("title"))
            entries.extend((card, heading, None) for card in cards)
        reported = payload.get("total_count", payload.get("total"))
        total = reported if isinstance(reported, int) and not isinstance(
            reported, bool) and reported >= 0 else None
        return project_listing_rows(entries, total, _ideas_extras)
    return listing_rows(payload, ("cards", "idea_cards", "items", "results", "data"))


def feed_rows(payload):
    """Project the feed listing: units nest inside days and editions.

    The service answers `{days: [{local_date, editions: [{kind, units:
    [...]}]}]}` — again the shape the community client itself reads — so
    the day's date and the edition's kind ride along as each unit's stamp
    and kind fallbacks. A flat units/items payload is still accepted.
    """
    if isinstance(payload, dict) and isinstance(payload.get("days"), list):
        days = payload["days"]
        if len(days) > LISTING_MAX_CONTAINER:
            raise BridgeFailure("protocol_error")
        entries = []
        for day in days:
            if not isinstance(day, dict):
                continue
            date = optional_text(day.get("local_date")) or optional_text(day.get("date"))
            editions = day.get("editions")
            if not isinstance(editions, list):
                continue
            for edition in editions:
                if not isinstance(edition, dict):
                    continue
                kind = optional_text(edition.get("kind")) or optional_text(
                    edition.get("edition"))
                units = edition.get("units")
                if not isinstance(units, list):
                    continue
                entries.extend((unit, kind, date) for unit in units)
        reported = payload.get("total_count", payload.get("total"))
        total = reported if isinstance(reported, int) and not isinstance(
            reported, bool) and reported >= 0 else None
        return project_listing_rows(entries, total, extras=_feed_extras)
    return listing_rows(payload, ("units", "items", "results", "data"),
                        extras=_feed_extras)


def _feed_prompt_info(gateway):
    """The feed's prompt card, read on the feed's own pass: what
    generates the feed (feed.prompt.get) and whether a new edition is
    being generated right now (feed.status). Both route shapes were
    confirmed by the feed probe on Nov 12, 2026. Fail-soft by design:
    a feed that cannot describe its prompt is still a feed, so any
    failure projects as no card rather than a failed read."""
    info = {}
    try:
        payload = gateway.call_json("feed.prompt.get", timeout=8)
        prompt = payload.get("prompt") if isinstance(payload, dict) else None
        if isinstance(prompt, dict):
            text = content_text(prompt.get("free_text")) or content_text(
                prompt.get("full_prompt"))
            info["feed_prompt"] = {
                "text": text[:2000] if text else None,
                "enabled": bool(prompt.get("enabled", False)),
            }
    except Exception:
        pass
    try:
        payload = gateway.call_json("feed.status", timeout=8)
        if isinstance(payload, dict):
            info["feed_status"] = {
                "refresh_in_progress": bool(
                    payload.get("refresh_in_progress", False)),
                "next_edition_kind": optional_text(
                    payload.get("next_edition_kind")),
            }
    except Exception:
        pass
    return info


def agent_rows(payload):
    """Project subagents.list for the side panel's Today list.

    Shape confirmed by the subagents probe (Nov 12, 2026):
    {active: [...], recent: [...], active_count, recent_count}, each
    run carrying prompt, status, created_at, completed_at and a
    final_response. Active runs lead; both lists keep the service's
    own order. Content passes the same redaction and caps as task
    content."""
    if not isinstance(payload, dict):
        return []
    rows = []
    for key in ("active", "recent"):
        entries = payload.get(key)
        if not isinstance(entries, list):
            continue
        for entry in entries:
            if not isinstance(entry, dict):
                continue
            prompt = content_text(entry.get("prompt"))
            if not prompt:
                continue
            final = content_text(entry.get("final_response"))
            rows.append({
                "prompt": prompt[:300],
                "status": optional_text(entry.get("status")),
                "created_at": _iso_from_epoch(entry.get("created_at")),
                "completed_at": _iso_from_epoch(entry.get("completed_at")),
                "final_response": final[:600] if final else None,
            })
            if len(rows) >= 8:
                return rows
    return rows


def approval_rows(payload):
    """Project pending approvals, keeping the service's own vocabulary.

    The Mac five-outcome words (`allow_once` ... `deny`) are the service's, so
    they travel unchanged instead of being translated into a parallel dialect.
    Only what a person needs in order to decide is kept: what is asking, for
    what, and every detail row the request published about itself.
    """
    if not isinstance(payload, dict):
        raise BridgeFailure("protocol_error")
    pending = payload.get("pending_approvals")
    if not isinstance(pending, list):
        raise BridgeFailure("protocol_error")
    rows = []
    for item in pending:
        if not isinstance(item, dict):
            raise BridgeFailure("protocol_error")
        display = item.get("display") if isinstance(item.get("display"), dict) else {}
        details = []
        for row in display.get("detail_rows") or []:
            if not isinstance(row, dict):
                continue
            label = optional_text(row.get("label") or row.get("name"))
            value = optional_text(row.get("value") or row.get("text"))
            if label and value:
                details.append({"label": label[:80], "value": value[:400]})
        approval_id = item.get("approval_id")
        if not isinstance(approval_id, str) or not approval_id.strip():
            # An approval with no id cannot be decided safely or at all.
            raise BridgeFailure("protocol_error")
        rows.append({
            "approval_id": approval_id,
            "kind": optional_text(item.get("kind") or item.get("action")),
            "decision": optional_text(item.get("decision")),
            "always_scope": optional_text(item.get("always_scope")),
            "attention_mode": optional_text(item.get("attention_mode")),
            "title": optional_text(display.get("title")) or "",
            "summary": optional_text(display.get("summary")) or "",
            "details": details,
        })
    return {"pending": rows}


DECISIONS = ("allow_once", "allow_task", "allow_site", "allow_always", "deny")


def decision_answer(approval_id, decision, answer):
    """What came back from a decision, without overstating it.

    A 2xx means the service accepted the decision; it does not mean the approved
    action then succeeded, so nothing here claims an effect. The service's own
    word for the outcome travels unchanged when it supplies one.
    """
    if not isinstance(answer, dict):
        raise BridgeFailure("protocol_error")
    return {
        "approval_id": approval_id,
        "requested": decision,
        "accepted": True,
        "decision": optional_text(answer.get("decision")),
    }


def permission_rows(payload):
    """Project the grants the service says are still in force.

    The active listing is the only evidence of what an earlier "always" left
    behind, so it is projected as-is: the service's id (the only thing a
    revocation can name), its own kind/lifetime words, the subject the grant
    covers, its expiry, and whether the service itself marks it revocable.
    A grant with no id can be neither revoked nor safely acted on, so it fails
    closed like an approval with no id, rather than being silently dropped
    from a list whose whole point is completeness.
    """
    if not isinstance(payload, dict):
        raise BridgeFailure("protocol_error")
    items = payload.get("items")
    if not isinstance(items, list) or len(items) > 200:
        raise BridgeFailure("protocol_error")
    rows = []
    for item in items:
        if not isinstance(item, dict):
            raise BridgeFailure("protocol_error")
        permission_id = item.get("permission_id")
        if not isinstance(permission_id, str) or not permission_id.strip():
            raise BridgeFailure("protocol_error")
        display = item.get("display") if isinstance(item.get("display"), dict) else {}
        expires = item.get("expires_at_ms")
        rows.append({
            "permission_id": permission_id,
            "kind": optional_text(item.get("kind")),
            "lifetime": optional_text(item.get("lifetime")),
            "mode": optional_text(item.get("mode")),
            "policy_subject": optional_text(item.get("policy_subject")),
            "expires_at_ms": expires if isinstance(expires, int)
            and not isinstance(expires, bool) else None,
            "revocable": item.get("revocable") is True,
            "title": optional_text(display.get("summary_title")) or "",
        })
    return {"grants": rows}


def revoke_answer(permission_id, answer):
    """What came back from a revocation, without overstating it.

    Acceptance is not disappearance: whether the grant is actually gone is a
    fact only a fresh read of the active listing holds, so the caller re-reads
    and reports that. The service's own `revoked` word travels when it gives
    one; nothing here invents it.
    """
    if not isinstance(answer, dict):
        raise BridgeFailure("protocol_error")
    revoked = answer.get("revoked")
    return {
        "permission_id": permission_id,
        "accepted": True,
        "revoked": revoked if isinstance(revoked, bool) else None,
    }


def delete_answer(job_id, answer):
    """What came back from a schedule deletion, without overstating it.

    Acceptance is not absence: whether the schedule is actually gone is a
    fact only a fresh read of the listing holds, so the caller re-reads and
    reports that. The service's own `deleted` word travels when it gives
    one; nothing here invents it.
    """
    if not isinstance(answer, dict):
        raise BridgeFailure("protocol_error")
    deleted = answer.get("deleted")
    return {
        "job_id": job_id,
        "accepted": True,
        "deleted": deleted if isinstance(deleted, bool) else None,
    }


def started_session_id(answer):
    """The id a `session.start` answer carries, when it carries one.

    The start route's answer shape is the service's own; the id is
    read from the places such an answer puts it (flat, or nested
    under its session row) and nowhere else. None means the answer
    did not name the chat it started -- the caller then proves the
    id against the session listing instead of guessing.
    """
    if not isinstance(answer, dict):
        return None
    for candidate in (answer.get("session_id"), answer.get("id")):
        if isinstance(candidate, str) and candidate:
            return candidate
    session = answer.get("session")
    if isinstance(session, dict):
        for candidate in (session.get("session_id"), session.get("id")):
            if isinstance(candidate, str) and candidate:
                return candidate
    return None


def updated_at_text(session):
    millis = session.get("updated_at_ms")
    if isinstance(millis, (int, float)) and not isinstance(millis, bool):
        import time
        return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(millis / 1000.0))
    return optional_text(session.get("updated_at"))


_SESSIONS_PROBED = False


def _thread_flag(row):
    """Whether a sessions.list row is a conversation thread.

    The recorded consumer reads `is_thread` off each row; the web
    sidebar lists threads, and seeded surfaces that ride the same
    list (the "Learn about ..." walkthrough) are not threads --
    the app listed them as chats until it read this flag (Patrick,
    Nov 13, 2026). Absent or null reads as a thread: the sidebar
    showed every row before the flag was read, and a missing flag
    must not be able to empty it."""
    value = row.get("is_thread")
    return True if value is None else bool(value)


def session_rows(client, config, config_dir=None):
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
            "pinned": bool(row.get("pinned", False)),
            "is_thread": _thread_flag(row),
            "updated_at": optional_text(row.get("updated_at")),
        } for row in object_rows(client.threads(include_archived=True))]
    with gateway_scope(config) as gateway:
        data = gateway.call_json("sessions.list")
    sessions = data.get("sessions", []) if isinstance(data, dict) else None
    if not isinstance(sessions, list):
        raise BridgeFailure("protocol_error")
    global _SESSIONS_PROBED
    if not _SESSIONS_PROBED and config_dir:
        _SESSIONS_PROBED = True
        nonthread = sum(1 for s in sessions
                        if isinstance(s, dict) and not _thread_flag(s))
        keys = ",".join(sorted(sessions[0].keys())) if sessions and isinstance(
            sessions[0], dict) else ""
        timing_log(config_dir, "sessions-shape rows={} nonthread={} keys={}".format(
            len(sessions), nonthread, keys)[:600])
    rows = []
    for session in sessions:
        if not isinstance(session, dict):
            raise BridgeFailure("protocol_error")
        rows.append({
            "session_id": required_text(session.get("session_id")),
            "title": required_text(session.get("title")),
            "archived": bool(session.get("archived", False)),
            "pinned": bool(session.get("pinned", False)),
            "is_thread": _thread_flag(session),
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
                "threads": session_rows(self.client, self.config,
                                        request.get("config_dir")),
            }
        if request["op"] == "identity":
            # The same verification as `connect` minus `sessions.list`, which the
            # service can take ten seconds to answer. A send preflight only has
            # to prove the saved session still belongs to the same account.
            return session_facts(self.client, self.config)
        if request["op"] == "history":
            return history_rows(self.client.history(thread=request.get("thread"), limit=request["limit"]), config_dir=request.get("config_dir"))
        if request["op"] == "tasks":
            with self.client._gateway(self.config) as gateway:
                schedules = gateway.call_json("tasks.list")["schedules"]
                runs = gateway.call_json("tasks.runs")["runs"]
                # Agent activity rides the same read (one snapshot for
                # the Tasks page and the side panel), fail-soft: the
                # schedules must not fail because the activity read did.
                try:
                    agents = agent_rows(
                        gateway.call_json("subagents.list", timeout=20))
                except Exception:
                    agents = []
            answer = {
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
                "agents": agents,
            }
            _probe_subagents(request.get("config_dir"), self.client, self.config)
            return answer
        if request["op"] == "delete_task":
            # A write on the shared gateway: one deletion, never retried here.
            # The id is one the schedule listing published, and whether the
            # schedule is gone afterwards is proven by re-reading that
            # listing, not by this answer.
            with self.client._gateway(self.config) as gateway:
                answer = gateway.call_json(
                    "tasks.delete",
                    path_params={"job_id": request["job_id"]},
                )
            return delete_answer(request["job_id"], answer)
        if request["op"] == "start_session":
            # One press, one new side chat, never retried here. The
            # body is the shape the recorded route table's own
            # consumer sends: a fresh, persistent session with no
            # title -- the service names the chat from its first
            # message, as it does on the web. The id the caller
            # lands in is proven, not echoed: the start answer's own
            # id when it carries one, otherwise the single session
            # the listing gained across the call.
            before = {row["session_id"]
                      for row in session_rows(self.client, self.config)}
            with self.client._gateway(self.config) as gateway:
                answer = gateway.call_json(
                    "session.start",
                    body={"method": "/api/session/start",
                          "params": {"origin": "fresh",
                                     "lifecycle": "persistent"}},
                )
            session_id = started_session_id(answer)
            if session_id is None:
                fresh = [row["session_id"]
                         for row in session_rows(self.client, self.config)
                         if row["session_id"] not in before]
                if len(fresh) == 1:
                    session_id = fresh[0]
            if session_id is None:
                raise BridgeFailure("protocol_error")
            return {"session_id": session_id}
        if request["op"] == "session_op":
            # One menu press, one session mutation, never retried
            # here. The body is the recorded shape for the kind:
            # rename flat, the rest under their /api/session method.
            # The answer claims acceptance only -- the caller's
            # refresh of the session listing is what proves the new
            # state, never this echo.
            kind = request["kind"]
            if kind == "rename":
                body = {"session_id": request["session_id"],
                        "title": request["title"]}
            else:
                body = {"method": f"/api/session/{kind}",
                        "session_id": request["session_id"]}
            with self.client._gateway(self.config) as gateway:
                gateway.call_json(f"session.{kind}", body=body)
            return {"session_id": request["session_id"], "kind": kind,
                    "accepted": True}
        if request["op"] == "artifacts":
            # One read of the Library listing. Listing is never consent: nothing
            # here opens, shares or deletes, and the projection keeps the cursor
            # and the raw node shape inside the bridge.
            with self.client._gateway(self.config) as gateway:
                payload = gateway.call_json("artifacts.list", timeout=20)
            return artifact_rows(payload)
        if request["op"] == "ideas":
            # Read-only: the idea cards the service recorded a list route
            # for. Executing or rating a card is a write and stays out of
            # this client until it gets its own explicit press.
            with self.client._gateway(self.config) as gateway:
                payload = gateway.call_json("api.idea-cards.list", timeout=20)
            _probe_ideas_shapes(request.get("config_dir"), payload)
            return ideas_rows(payload)
        if request["op"] == "goals":
            # Read-only: the account's goals as the recorded list route
            # reports them. Editing or deleting a goal is a write and is
            # not offered here.
            with self.client._gateway(self.config) as gateway:
                payload = gateway.call_json("goals.list", timeout=20)
            return listing_rows(payload, ("goals", "items", "results", "data"))
        if request["op"] == "feed":
            # Read-only: the feed units the recorded list route returns.
            # Reactions, reordering and regeneration are writes and stay
            # on the Muse side.
            with self.client._gateway(self.config) as gateway:
                payload = gateway.call_json("feed.list", timeout=20)
                prompt_info = _feed_prompt_info(gateway)
            _probe_feed_shapes(request.get("config_dir"), self.client,
                               self.config, payload)
            rows = feed_rows(payload)
            rows.update(prompt_info)
            return rows
        if request["op"] == "approvals":
            # Read-only, and deliberately on its own connection: listing what the
            # agent is asking for must never decide it, and the sentinel service
            # is not the daemon connection the rest of this adapter shares.
            gateway = sentinel_gateway(self.config)
            try:
                payload = gateway.call_json("egress.approvals", timeout=20)
            finally:
                gateway.close()
            return approval_rows(payload)
        if request["op"] == "decide":
            # A write on the sentinel service: one decision, one connection, and
            # never retried here. The caller shows the request and presses once.
            gateway = sentinel_gateway(self.config)
            try:
                answer = gateway.call_json(
                    "egress.approval.decide",
                    path_params={"approval_id": request["approval_id"]},
                    body={"decision": request["decision"]},
                    timeout=20,
                )
            finally:
                gateway.close()
            return decision_answer(request["approval_id"], request["decision"], answer)
        if request["op"] == "permissions":
            # Read-only, on the sentinel service like the approvals it came
            # from: listing a grant never revokes it, and the projection keeps
            # the raw listing (which runs to hundreds of KB) inside the bridge.
            gateway = sentinel_gateway(self.config)
            try:
                payload = gateway.call_json("egress.permissions.active", timeout=20)
            finally:
                gateway.close()
            return permission_rows(payload)
        if request["op"] == "revoke":
            # A write on the sentinel service: one revocation, one connection,
            # never retried here. The id is one the active listing published,
            # and whether the grant is gone afterwards is proven by re-reading
            # that listing, not by this answer.
            gateway = sentinel_gateway(self.config)
            try:
                answer = gateway.call_json(
                    "egress.permissions.revoke",
                    body={"permission_id": request["permission_id"]},
                    timeout=20,
                )
            finally:
                gateway.close()
            return revoke_answer(request["permission_id"], answer)
        if request["op"] == "activity":
            return activity_rows(self.client, self.config, request["timeout"])
        if request["op"] == "cancel":
            # The service answers whether anything was actually stopped. A press
            # is a request, never proof, so `cancelled` travels as data and the
            # caller has to report what came back.
            with self.client._gateway(self.config) as gateway:
                answer = gateway.call_json("chat.cancel")
            if not isinstance(answer, dict):
                raise BridgeFailure("protocol_error")
            return {
                "cancelled": answer.get("cancelled") is True,
                "message_id": optional_text(answer.get("message_id")),
            }
        if request["op"] == "watch":
            # Hold the request open on one connection and re-read cheaply: a
            # fresh history read per tick costs a handshake, which is what made
            # an answer take seconds to appear.
            import time
            timeout = request["timeout"] / 1000.0
            began = time.monotonic()
            deadline = began + timeout
            # A caller that already knows the newest message it has seen sends
            # it as `after`: the first read then answers at once when anything
            # newer is already there, so nothing that arrived while the request
            # was being set up is skipped.
            after = request.get("after")
            timing_log(request.get("config_dir"),
                       "bridge watch begin limit={} after={}".format(
                           request["limit"], after is not None))
            # Ticks probe the tail with a small read: the full window of a
            # long conversation is heavy to re-read twice a second, and the
            # probe only has to notice a change. The rows a watch returns are
            # always a read at the requested limit.
            probe_limit = min(request["limit"], 25)

            def probe():
                return history_rows(self.client.history(
                    thread=request.get("thread"), limit=probe_limit), config_dir=request.get("config_dir"))

            full_ms = []

            def full():
                if probe_limit == request["limit"]:
                    return None
                started = time.monotonic()
                rows = history_rows(self.client.history(
                    thread=request.get("thread"), limit=request["limit"]), config_dir=request.get("config_dir"))
                full_ms.append(int((time.monotonic() - started) * 1000))
                return rows

            probes = [0]

            def done(why, rows):
                timing_log(request.get("config_dir"),
                           "bridge watch return why={} ms={} probes={} "
                           "full_ms={} rows={}".format(
                               why, int((time.monotonic() - began) * 1000),
                               probes[0], full_ms[0] if full_ms else "-",
                               len(rows)))
                return rows

            def fingerprint(rows):
                # A reply being produced mutates in place: same message id and
                # seq while its text grows and its status flips at the end.
                # Keying the wait on the id alone made a watch sleep through
                # the whole reply and answer only at its deadline, which is
                # why a finished answer could take the better part of a minute
                # to reach the screen after it existed.
                if not rows:
                    return None
                last = rows[-1]
                return (len(rows), last["message_id"], last["seq"],
                        last["status"], last["text"])

            baseline = None
            while True:
                rows = probe()
                probes[0] += 1
                mark = fingerprint(rows)
                if baseline is None:
                    if after is not None and (mark is None or mark[1] != after):
                        return done("after", full() or rows)
                    baseline = mark
                elif mark != baseline:
                    return done("change", full() or rows)
                if time.monotonic() >= deadline:
                    return done("deadline", full() or rows)
                time.sleep(0.4)
        if request["op"] == "send":
            import time
            send_began = time.monotonic()
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
            baseline = history_rows(self.client.history(thread=thread, limit=25), config_dir=request.get("config_dir"))
            baseline_seq = max((r["seq"] for r in baseline), default=0)
            baseline_ids = {r["message_id"] for r in baseline}
            self.send_started = True
            device = {"device_id": request["device_id"]} if "device_id" in request else {}
            result = checked(self.client.send(message=message, thread=thread, fire_and_forget=True, wait_seconds=0, **device), dict)
            if result.get("sent") is not True:
                raise BridgeFailure("protocol_error", uncertain=True)
            deadline = time.monotonic() + self.confirmation_window
            while True:
                rows = history_rows(self.client.history(thread=thread, limit=25), config_dir=request.get("config_dir"))
                for row in rows:
                    # The text comparison ignores surrounding whitespace: the
                    # service's stored display text is not guaranteed to be
                    # byte-identical to what was typed, and an exact match
                    # that never lands reports every healthy send as
                    # unconfirmed.
                    if (row["seq"] > baseline_seq and row["message_id"] not in baseline_ids
                            and row["role"] == "user"
                            and row["text"].strip() == message.strip()):
                        timing_log(request.get("config_dir"),
                                   "bridge send return confirmed ms={}".format(
                                       int((time.monotonic() - send_began) * 1000)))
                        return {"sent": True, "confirmed": True, "message_id": row["message_id"]}
                if time.monotonic() >= deadline:
                    break
                time.sleep(0.2)
            timing_log(request.get("config_dir"),
                       "bridge send return unconfirmed ms={}".format(
                           int((time.monotonic() - send_began) * 1000)))
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
