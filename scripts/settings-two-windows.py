#!/usr/bin/python3
"""Repeat the two-window Settings check on a real seat.

The Settings page and its web view are two renderers of one updater. This
script opens both under a throwaway config, presses Check on each card in
turn, and requires that

  * the press shows the app checking (the page's button goes "Working…" and
    the native status says "Checking for updates…"),
  * both cards say the same thing afterwards, and
  * neither card is left on that busy step -- the exact failure the page used
    to have when a press answered before the worker did.

Both cards are read from their accessibility trees, never from pixels: the
native label's text is its accessible name, and the page's status paragraph
is read through Atspi.Text. Nothing here needs the window manager to place or
raise a window, so it works wherever the app can open two windows.

    scripts/settings-two-windows.py
    scripts/settings-two-windows.py --binary target/debug/muse --keep

Needs PyGObject with Atspi (Fedora: python3-gobject, Debian: python3-gi),
a logged-in desktop session, and the binary already built:

    cargo build -p muse

The app is single-instance, so close any Muse already running first. The
throwaway config copies the saved account link from the real config (the
card's page needs a linked app) and marks first run done, so the app opens
Chat with a working Settings page. Nothing in the real config is written: the
copy lives in a temp dir that is removed unless --keep is given or a check
fails.

The app is started the way scripts/muse-gui.sh starts it: on DISPLAY (or
MUSE_DISPLAY, default :1) with the toolbox env sourced, which is what puts
WebKit on the loader's path. Pass --env-script '' to start it bare.
"""

from __future__ import annotations

import argparse
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path

try:
    import gi

    gi.require_version("Atspi", "2.0")
    from gi.repository import Atspi
except (ImportError, ValueError) as error:  # pragma: no cover - environment
    sys.exit(
        "settings-two-windows: PyGObject with Atspi is required "
        f"({error}); install python3-gobject (Fedora) or python3-gi (Debian)"
    )

APP = "muse"
MAIN_FRAME = "Muse"
WEB_FRAME = "Settings · web view"
SETTINGS_BUTTON = "Settings"
WEB_VIEW_BUTTON = "Web view"

# The app's own words: the check's busy status, and the page's in-flight label.
BUSY_STATUS = "Checking for updates…"
WORKING = "Working…"

# A card's buttons, so the status line is the last text before the press.
CARD_BUTTONS = ("Check for updates", WORKING, "Download update", "Restart to update")

# Refusals mean the press never reached the worker; the round retries instead
# of passing on a status that never changed.
REFUSAL_WORDS = ("already running", "already working")

POLL = 0.01


class Failure(Exception):
    pass


# --- the accessibility tree -------------------------------------------------


def frames():
    desktop = Atspi.get_desktop(0)
    if desktop is None:
        raise Failure("no accessibility bus: is a desktop session running?")
    for index in range(desktop.get_child_count()):
        app = desktop.get_child_at_index(index)
        if app is None or (app.get_name() or "") != APP:
            continue
        for child_index in range(app.get_child_count()):
            frame = app.get_child_at_index(child_index)
            if frame is not None:
                yield frame


def first_frame(name):
    for frame in frames():
        if (frame.get_name() or "") == name:
            return frame
    return None


def walk(node, out=None, depth=0, limit=40):
    out = [] if out is None else out
    out.append(node)
    if depth >= limit:
        return out
    for index in range(node.get_child_count()):
        try:
            child = node.get_child_at_index(index)
        except Exception:
            continue
        if child is not None:
            walk(child, out, depth + 1, limit)
    return out


def role(node):
    try:
        return node.get_role_name()
    except Exception:
        return ""


def name(node):
    try:
        return node.get_name() or ""
    except Exception:
        return ""


def text(node):
    """The node's own text: Atspi.Text for the page, the name for GTK labels."""
    try:
        value = Atspi.Text.get_text(node, 0, -1)
        if value:
            return value
    except Exception:
        pass
    return name(node)


def find_button(frame_name, button_names):
    frame = first_frame(frame_name)
    if frame is None:
        return None
    for node in walk(frame):
        if role(node) in ("push button", "toggle button") and name(node) in button_names:
            return node
    return None


def press(frame_name, button_names):
    button = find_button(frame_name, button_names)
    if button is None:
        return False
    try:
        actions = button.get_action_iface()
        if actions is None:
            return False
        for index in range(actions.get_n_actions()):
            if actions.do_action(index):
                return True
    except Exception:
        return False
    return False


def card(frame_name):
    """The card as rendered: (status line, check button's label, refusal line).

    Both renderers title the section "Updates"; the status is the first text
    under that title, and the press is the first card button after it. That
    holds while the button is disabled for a check too, when the page's label
    reads "Working…".
    """
    frame = first_frame(frame_name)
    if frame is None:
        return None, None, None
    nodes = walk(frame)
    refusal = None
    for node in nodes:
        if role(node) in ("label", "paragraph"):
            value = text(node)
            if value and any(word in value for word in REFUSAL_WORDS):
                refusal = value

    title = None
    for index, node in enumerate(nodes):
        if role(node) in ("label", "heading") and text(node).strip().lower() == "updates":
            title = index
            break
    if title is None:
        return None, None, refusal

    status = None
    button = None
    for node in nodes[title + 1 :]:
        kind = role(node)
        if kind in ("label", "paragraph") and status is None:
            value = text(node)
            if value:
                status = value
        elif kind in ("push button", "toggle button") and name(node) in CARD_BUTTONS:
            button = name(node)
            break
    return status, button, refusal


def wait_until(predicate, timeout, what):
    deadline = time.monotonic() + timeout
    seen = None
    while time.monotonic() < deadline:
        seen = predicate()
        if seen:
            return seen
        time.sleep(POLL)
    raise Failure(f"{what} did not happen within {timeout:.0f}s")


# --- the two-window check ---------------------------------------------------


def launch_command(binary, env_script):
    """The app, with the seat's toolbox env sourced the way muse-gui.sh does."""
    if not env_script or not Path(env_script).is_file():
        return [binary]
    return [
        "/bin/sh",
        "-c",
        '. "$1" >/dev/null 2>&1; shift; exec "$@"',
        "muse-settings-pair",
        env_script,
        binary,
    ]


def open_settings(command, env, log_path):
    proc = subprocess.Popen(
        command,
        env=env,
        stdin=subprocess.DEVNULL,
        stdout=open(log_path, "wb"),
        stderr=subprocess.STDOUT,
        start_new_session=True,
    )
    try:
        wait_until(lambda: first_frame(MAIN_FRAME), 25, f"the {MAIN_FRAME} window")
        # The rail's Settings button, then the page's own Web view button once
        # the page is the visible stack child (its card paints by then).
        if not press(MAIN_FRAME, (SETTINGS_BUTTON,)):
            raise Failure("the native Settings button could not be pressed")
        wait_until(lambda: card(MAIN_FRAME)[0], 20, "the native card's status")
        if not press(MAIN_FRAME, (WEB_VIEW_BUTTON,)):
            raise Failure("the Settings page's Web view button could not be pressed")
        wait_until(lambda: first_frame(WEB_FRAME), 25, f"the {WEB_FRAME} window")
        wait_until(lambda: card(WEB_FRAME)[1], 20, "the web card's button")
        return proc
    except Failure:
        stop(proc)
        raise


def stop(proc):
    if proc is None or proc.poll() is not None:
        return
    proc.terminate()
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait(timeout=5)


def idle():
    page_status, page_button, _ = card(WEB_FRAME)
    native_status, _, _ = card(MAIN_FRAME)
    return (
        page_status
        and native_status
        and page_button != WORKING
        and page_status != BUSY_STATUS
        and native_status != BUSY_STATUS
    )


def exercise(label, frame_name, buttons, timeout, busy_window):
    """Press one card's Check, watch both cards during the check, settle."""
    wait_until(idle, timeout, f"the cards to be idle before the {label} press")

    presses = 0
    page_seen = native_seen = False
    mid_check = None
    while presses < 3 and mid_check is None:
        presses += 1
        if not press(frame_name, buttons):
            raise Failure(f"the {label} card's Check button could not be pressed")
        started = time.monotonic()
        while time.monotonic() - started < busy_window:
            page_status, page_button, _ = card(WEB_FRAME)
            native_status, _, _ = card(MAIN_FRAME)
            page_status = page_status or ""
            native_status = native_status or ""
            page_busy = page_button == WORKING or BUSY_STATUS in page_status
            native_busy = native_status == BUSY_STATUS
            page_seen = page_seen or page_busy
            native_seen = native_seen or native_busy
            # Both cards caught in the check itself: the page's own line must
            # be the app's, word for word. A decoration the page adds (the old
            # "Checking now…" suffix) is exactly what this catches.
            if BUSY_STATUS in page_status and native_busy:
                if page_status != native_status:
                    raise Failure(
                        f"mid-check the cards disagree: page {page_status!r} "
                        f"vs native {native_status!r}"
                    )
                mid_check = page_status
                break
            _, _, refusal = card(WEB_FRAME)
            if refusal:
                break
            time.sleep(POLL)
        if mid_check is None:
            wait_until(idle, timeout, f"the cards to be idle after the {label} press")
    if mid_check is None:
        raise Failure(
            f"the two cards were never read mid-check at the same moment after the "
            f"{label} press (page {'busy' if page_seen else 'quiet'}, "
            f"native {'busy' if native_seen else 'quiet'}): the check finished too "
            "fast or one surface never took the busy state"
        )

    # The check has started; it must finish, and both cards must leave the
    # busy step together.
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        settled = idle()
        if settled:
            page_status, page_button, _ = card(WEB_FRAME)
            native_status, _, _ = card(MAIN_FRAME)
            if page_button == WORKING or page_status == BUSY_STATUS:
                raise Failure(f"the page is still on the busy step after the {label} press")
            if native_status == BUSY_STATUS:
                raise Failure(
                    f"the native card is still on the busy step after the {label} press"
                )
            if page_status != native_status:
                raise Failure(
                    f"the cards disagree after the {label} press: "
                    f"page {page_status!r} vs native {native_status!r}"
                )
            return presses, page_status, mid_check
        time.sleep(POLL)

    page_status, page_button, _ = card(WEB_FRAME)
    native_status, _, _ = card(MAIN_FRAME)
    if page_button == WORKING:
        raise Failure(
            f"the page was left on {WORKING!r} after the {label} press "
            f"(status {page_status!r}, native {native_status!r})"
        )
    raise Failure(
        f"the cards did not settle after the {label} press "
        f"(page {page_status!r}, native {native_status!r})"
    )


def run(binary, timeout, busy_window, keep, env_script):
    if not os.access(binary, os.X_OK):
        raise Failure(f"{binary} is not executable; build it with `cargo build -p muse`")
    running = subprocess.run(
        ["pgrep", "-x", APP], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL
    )
    if running.returncode == 0:
        raise Failure("another Muse is running (the app is single-instance); close it first")

    real_config = Path(
        os.environ.get("XDG_CONFIG_HOME") or Path.home() / ".config"
    ) / "unofficial-muse-for-linux"
    saved_link = real_config / "account" / "account-connection.json"
    if not saved_link.is_file():
        raise Failure(
            f"no saved account link at {saved_link}; sign in once so the card's page "
            "opens on a linked app"
        )

    root = Path(tempfile.mkdtemp(prefix="muse-settings-pair-"))
    config = root / "config" / "unofficial-muse-for-linux"
    (config / "account").mkdir(parents=True)
    shutil.copy2(saved_link, config / "account" / "account-connection.json")
    (config / "first-run-done").touch()
    log_path = root / "muse.log"

    env = os.environ.copy()
    env.pop("MUSE_SPIKE_CMD", None)
    env.setdefault("DISPLAY", os.environ.get("MUSE_DISPLAY", ":1"))
    env.update(
        {
            "XDG_CONFIG_HOME": str(root / "config"),
            "GDK_BACKEND": env.get("GDK_BACKEND", "x11"),
            # A non-empty APPDIR is what makes this copy update itself, so the
            # card offers a real Check. The dir itself is never used.
            "APPDIR": str(root),
            "MUSE_PROBE": "1",
        }
    )

    proc = None
    ok = False
    try:
        proc = open_settings(launch_command(binary, env_script), env, log_path)

        wait_until(idle, timeout, "both cards to show a settled status")
        first_status, _, _ = card(WEB_FRAME)
        native_status, _, _ = card(MAIN_FRAME)
        if first_status != native_status:
            raise Failure(
                f"the cards disagree before any press: "
                f"page {first_status!r} vs native {native_status!r}"
            )
        print(f"opened: both cards read {first_status!r}")

        rounds = [
            ("page", WEB_FRAME, (WORKING, "Check for updates")),
            ("native", MAIN_FRAME, (WORKING, "Check for updates")),
        ]
        for label, frame_name, buttons in rounds:
            presses, status, mid_check = exercise(
                label, frame_name, buttons, timeout, busy_window
            )
            note = "" if presses == 1 else f" after {presses} presses"
            print(
                f"press {label}: both cards mid-check on {mid_check!r}; "
                f"both settled on {status!r}{note}"
            )

        page_status, page_button, _ = card(WEB_FRAME)
        native_status, _, _ = card(MAIN_FRAME)
        if page_status != native_status:
            raise Failure(
                f"the cards disagree at the end: page {page_status!r} vs native {native_status!r}"
            )
        if page_button == WORKING:
            raise Failure("the page was left on 'Working…'")
        print(f"PASS: both cards read {page_status!r}; neither is on the busy step")
        ok = True
    except Failure as failure:
        print(f"FAIL: {failure}", file=sys.stderr)
        log_text = ""
        if log_path.exists():
            log_text = log_path.read_text(errors="replace")
            tail = log_text.splitlines()[-20:]
            if tail:
                print("--- muse log (last 20 lines) ---", file=sys.stderr)
                print("\n".join(tail), file=sys.stderr)
            if "error while loading shared libraries" in log_text:
                print(
                    "--- the app could not find its libraries: source the toolbox "
                    f"env or pass --env-script {Path.home()}/.local/share/muse-toolbox/env.sh "
                    "---",
                    file=sys.stderr,
                )
        print(f"--- temp config kept at {root} ---", file=sys.stderr)
        keep = True
    finally:
        stop(proc)
        if not keep:
            shutil.rmtree(root, ignore_errors=True)
    return ok


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--binary", default="target/debug/muse", help="the built app (default: %(default)s)"
    )
    parser.add_argument(
        "--timeout", type=float, default=25.0, help="settle budget in seconds (default: 25)"
    )
    parser.add_argument(
        "--busy-window",
        type=float,
        default=5.0,
        help="how long a press may take to show a check starting (default: 5)",
    )
    parser.add_argument("--keep", action="store_true", help="keep the temp config and log")
    parser.add_argument(
        "--env-script",
        default=str(Path.home() / ".local/share/muse-toolbox/env.sh"),
        help="sourced before the app starts, as muse-gui.sh does (default: %(default)s)",
    )
    args = parser.parse_args()

    def interrupted(signum, frame):
        raise Failure("interrupted")

    signal.signal(signal.SIGINT, interrupted)
    try:
        return (
            0
            if run(args.binary, args.timeout, args.busy_window, args.keep, args.env_script)
            else 1
        )
    except Failure as failure:
        print(f"FAIL: {failure}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
