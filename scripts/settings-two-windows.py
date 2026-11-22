#!/usr/bin/python3
"""Repeat the two-window Settings update check on a real seat.

The Settings page and its web view are two renderers of one updater. This
script opens both under a throwaway config, presses the update card on each in
turn, and requires that

  * each press shows the app working (the page's button goes "Working…", both
    statuses say the app's own busy line, word for word),
  * both cards settle on the same words and offer the same press,
  * neither card is left on a busy step -- the exact failure the page used to
    have when a press answered before the worker did,
  * with the harness feed (the default), both cards reach "A newer build is
    available." with "Download update", then the staged state with "Restart to
    update", and a Restart press is refused instead of replacing this copy,
  * with two Settings web views open at once, a press reaches every one of
    them -- the app keeps every window a page opened, not just the newest,
  * a Settings web view opened while a check is running boots on the busy
    snapshot ("Checking for updates…" and "Working…") and then settles on the
    same answer as the native card, like a window that was already up,
  * the failure paths: a feed that cannot be read, a feed that reads but is
    not a release, and an asset whose `file://` path is gone. Both cards carry
    the same failure sentence, and a press after the feed or the asset is put
    back still works,
  * two Check presses a double-click apart, on the native card and again on
    the page: the second is refused by the app's own busy flag, both cards show
    its sentence ("An update check is already running."), the first check still
    lands and neither card is left on "Working…", rather than the harness
    retrying the press until it slips through,
  * with `--restart`, the one step nothing else drives: the update is staged
    into a throwaway copy, Restart is pressed for real, and the staged build has
    to take over -- it records where it was started from, it is still up when
    the old process is gone, and the launcher copy now holds its bytes.

The feed is a file, not the network: the app reads `MUSE_UPDATE_FEED` (a
release JSON on disk) instead of the release API, stages the asset from the
`file://` URL the feed names, and refuses to relaunch -- so a dev build with no
compiled-in commit can still be driven to the offered and staged states, and
nothing here can replace the binary under test. `--feed real` runs the same
two Check presses against the real feed instead, asserting only agreement.

A file feed answers in microseconds, which would leave the busy step too short
to read on both cards; the script sets `MUSE_UPDATE_DELAY_MS` (`--delay-ms`,
default 2500) so the feed and the staged copy each take long enough to be seen
mid-press, the way a network answer would on a slow day. The delay also has to
outlast a window opening, since one round opens a Settings view while a check
is still running and needs it to boot on the busy snapshot.

`--no-appimage` runs with no `APPDIR`, so the app is a copy that updates
through its own channel: both cards must say exactly that and offer neither
Check nor Download -- a press nobody can act on is not an offer.

`--restart` is the opt-in half, and the two-window rounds never take it: they
refuse Restart so they cannot replace the copy under them, so this round runs a
throwaway copy of the binary in the temp dir and asks the app to relaunch for
real from the native card alone -- its subject is the relaunch, and not loading
a web view keeps it off WebKit. The feed's asset is then a stand-in build that records the path it was
started from and stays up, so the round can require that the new process is the
staged file, that the old process and its window are gone, and that the copy a
launcher would open now holds the staged bytes.

Both cards are read from their accessibility trees, never from pixels: the
native label's text is its accessible name, and the page's status paragraph is
read through Atspi.Text. Nothing here needs the window manager to place or
raise a window, so it works wherever the app can open two windows.

    scripts/settings-two-windows.py
    scripts/settings-two-windows.py --feed real --keep
    scripts/settings-two-windows.py --restart

Needs PyGObject with Atspi (Fedora: python3-gobject, Debian: python3-gi),
a logged-in desktop session, and the binary already built:

    cargo build -p muse

The app is single-instance, so close any Muse already running first. The
throwaway config copies the saved account link from the real config (the
card's page needs a linked app) and marks first run done, so the app opens
Chat with a working Settings page. Nothing in the real config or cache is
written: the copy lives in a temp dir (config, cache and staged download) that
is removed unless --keep is given or a check fails.

The app is started the way scripts/muse-gui.sh starts it: on DISPLAY (or
MUSE_DISPLAY, default :1) with the toolbox env sourced, which is what puts
WebKit on the loader's path. Pass --env-script '' to start it bare.
"""

from __future__ import annotations

import argparse
import json
import os
import shlex
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

# The app's own words (apps/muse/src/pages.rs and update.rs).
CHECK_STATUS = "Checking for updates…"
DOWNLOAD_STATUS = "Downloading the update…"
RESTART_STATUS = "Restarting into the update…"
OFFERED_STATUS = "A newer build is available."
CHECK_REFUSAL = "An update check is already running."
NOT_APPIMAGE = "This copy updates through its own channel (store or checkout), not here."
# The app's failure prefixes; the rest of each sentence comes from the OS.
CHECK_FAILED = "Update check failed: "
INVALID_FEED = "Update feed returned invalid data."
DOWNLOAD_FAILED = "Update download failed: "
DOWNLOADED_PREFIX = "Update downloaded to "
DOWNLOADED_SUFFIX = ". Restart to switch to it."
# The asset the harness feed names, and so the name a staged copy must carry.
STAGED_ASSET = "unofficial-muse-for-linux-x86_64.AppImage"
RESTART_REFUSAL = "Restart is disabled in this harness copy."
WORKING = "Working…"

CHECK_LABELS = ("Check for updates", WORKING)
ACTION_LABELS = ("Download update", "Restart to update")
CARD_BUTTONS = CHECK_LABELS + ACTION_LABELS
BUSY_STATUSES = (CHECK_STATUS, DOWNLOAD_STATUS, RESTART_STATUS)

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


def frames_named(name):
    return [frame for frame in frames() if (frame.get_name() or "") == name]


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
    """The node to press for one of `button_names`.

    A button is preferred, but any other node carrying the name and an action
    will do: GTK versions differ in which widget ends up as the actionable
    accessible node (a rail press has been a plain button and a list row), and
    the harness must not care which.
    """
    frame = first_frame(frame_name)
    if frame is None:
        return None
    loose = None
    for node in walk(frame):
        if name(node) not in button_names:
            continue
        try:
            actions = node.get_action_iface()
        except Exception:
            actions = None
        if actions is None or actions.get_n_actions() == 0:
            continue
        if role(node) in ("push button", "toggle button"):
            return node
        if loose is None:
            loose = node
    return loose


def dump_tree(frame_name, limit=120):
    """Print what the accessibility tree holds: the only way to see a runner."""
    frame = first_frame(frame_name)
    if frame is None:
        print(f"debug: no frame named {frame_name!r}")
        return
    nodes = walk(frame)
    print(f"debug: frame {name(frame)!r} holds {len(nodes)} nodes")
    for node in nodes[:limit]:
        try:
            actions = node.get_action_iface()
            count = actions.get_n_actions() if actions is not None else 0
        except Exception:
            count = 0
        print(f"debug: {role(node)!r} {name(node)!r} actions={count}")


def button_names(frame_name):
    """The names of the buttons the `frame_name` window actually shows."""
    frame = first_frame(frame_name)
    if frame is None:
        return []
    return [
        name(node)
        for node in walk(frame)
        if role(node) in ("push button", "toggle button")
    ]


def shows_text(frame_name, sentence, index=0):
    """Whether the `index`th window named `frame_name` shows `sentence`."""
    views = frames_named(frame_name)
    if index >= len(views):
        return False
    return any(text(node) == sentence for node in walk(views[index]))


def press_twice(frame_name, button_names, gap=0.35):
    """Two presses on one button `gap` seconds apart.

    Presses closer than a double-click interval are treated as one: only the
    first activation reaches the app. `gap` is set just past that interval, so
    the second press is its own press and still lands inside the check the
    first one started -- the press the app itself has to refuse.
    """
    button = find_button(frame_name, button_names)
    if button is None:
        return False
    try:
        actions = button.get_action_iface()
        if actions is None:
            return False
        ranks = range(actions.get_n_actions())
        first = any(actions.do_action(index) for index in ranks)
        time.sleep(gap)
        second = any(actions.do_action(index) for index in ranks)
        return first and second
    except Exception:
        return False


def press(frame_name, button_names, window=2.0):
    """Press the named control, allowing the tree a moment to take it.

    A node can be handed out while its widget is still being laid out or
    repainted, and its action reports failure then, which is how a runner's
    first press of a button it could already see came back unpressed. The press
    is retried for `window` seconds -- far shorter than a reader's own reach for
    the button -- and a control that really cannot be pressed still fails.
    """
    deadline = time.monotonic() + window
    while True:
        button = find_button(frame_name, button_names)
        if button is not None:
            try:
                actions = button.get_action_iface()
                if actions is not None:
                    for index in range(actions.get_n_actions()):
                        if actions.do_action(index):
                            return True
            except Exception:
                pass
        if time.monotonic() >= deadline:
            return False
        time.sleep(0.05)


def card(frame_name, index=0):
    """The card of the `index`th window named `frame_name`, as rendered."""
    views = frames_named(frame_name)
    if index >= len(views):
        return None, None, None, None
    return card_of(views[index])


def actionable(node):
    """Whether assistive tech can press this node."""
    try:
        actions = node.get_action_iface()
    except Exception:
        return False
    return actions is not None and actions.get_n_actions() > 0


def labelled(nodes, labels):
    """The name of the first node carrying one of `labels`.

    A node that can be pressed wins; failing that, any node with the name will
    do, because which of the two a widget tree names is a toolkit's choice and
    the round is about the words a reader sees. A press that this matched but
    cannot be made still fails the press itself.
    """
    fallback = None
    for node in nodes:
        value = name(node)
        if value not in labels:
            continue
        if actionable(node):
            return value
        if fallback is None:
            fallback = value
    return fallback


def card_of(frame):
    """The card as rendered: (status, check label, action label, refusal).

    Both renderers title the section "Updates" and the status is the first
    text under that title; the check press and the action press (absent when
    there is nothing to install) are found by their words rather than by where
    a toolkit happened to put them in the tree -- the same card reads the same
    on GTK 4.14 and 4.22 only that way. The check button reads "Working…"
    while a press is in flight.
    """
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
        return None, None, None, refusal

    status = None
    for node in nodes[title + 1 :]:
        if role(node) in ("label", "paragraph"):
            value = text(node).strip()
            if value:
                status = value
                break

    return (
        status,
        labelled(nodes, CHECK_LABELS),
        labelled(nodes, ACTION_LABELS),
        refusal,
    )


def wait_web_windows(status, action, timeout, label):
    """Every open Settings web view shows `status` offering `action`.

    Two windows open at once is the case the app's window registry has to get
    right: only the newest one is tracked unless every window is, so an older
    window silently stopping its nudges shows up here as a stale card.
    """
    deadline = time.monotonic() + timeout
    seen = []
    while time.monotonic() < deadline:
        views = frames_named(WEB_FRAME)
        if len(views) >= 2:
            seen = [(value[0], value[2]) for value in (card_of(view) for view in views)]
            if all(value == (status, action) for value in seen):
                return len(views)
        time.sleep(POLL)
    detail = ", ".join(f"{value!r}" for value in seen) or "no cards read"
    raise Failure(
        f"the Settings web views did not all show {status!r} with {action!r} "
        f"after the {label} press: {detail}"
    )


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


def open_settings(command, env, log_path, with_page=True):
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
        # The rail and its buttons reach the accessibility tree a moment after
        # the window does, and a loaded runner can take seconds to paint them:
        # wait for the press to exist instead of racing the first frame.
        try:
            wait_until(
                lambda: find_button(MAIN_FRAME, (SETTINGS_BUTTON,)) is not None,
                20,
                f"the {SETTINGS_BUTTON} button",
            )
        except Failure:
            # A runner's tree is not this seat's tree: say what it held.
            dump_tree(MAIN_FRAME)
            raise
        # The rail's Settings button, then the page's own Web view button once
        # the page is the visible stack child (its card paints by then).
        if not press(MAIN_FRAME, (SETTINGS_BUTTON,)):
            raise Failure("the native Settings button could not be pressed")
        wait_until(lambda: card(MAIN_FRAME)[0], 20, "the native card's status")
        if not with_page:
            # The restart round drives the native card only; nothing it checks
            # needs a web view, and not loading one keeps it off WebKit.
            return proc

        # The page's own Web view button. A button that is there but not yet
        # pressable -- insensitive while the page settles -- is waited for
        # rather than pressed once and given up on; the window it opens ends
        # the wait even if the press did not report itself.
        def web_view_opened():
            if first_frame(WEB_FRAME) is not None:
                return True
            return press(MAIN_FRAME, (WEB_VIEW_BUTTON,), window=0.25)

        try:
            wait_until(web_view_opened, 20, f"the {WEB_VIEW_BUTTON} press to land")
        except Failure:
            dump_tree(MAIN_FRAME)
            raise
        wait_until(lambda: first_frame(WEB_FRAME), 25, f"the {WEB_FRAME} window")
        wait_until(lambda: card(WEB_FRAME)[0], 20, "the web card's status")
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


def cards():
    """Both cards in one read: (page status, page check, page action, refusal)."""
    page_status, page_check, page_action, refusal = card(WEB_FRAME)
    native_status, _, native_action, _ = card(MAIN_FRAME)
    return page_status, page_check, page_action, native_status, native_action, refusal


def quiet():
    """Both cards off the busy step, if both have a status: (page, native)."""
    page_status, page_check, page_action, native_status, native_action, _ = cards()
    if not page_status or not native_status:
        return None
    if page_check == WORKING or page_status in BUSY_STATUSES or native_status in BUSY_STATUSES:
        return None
    return page_status, page_action, native_status, native_action


def idle():
    """Both cards off the busy step and agreeing word for word."""
    pair = quiet()
    return pair is not None and pair[0] == pair[2] and pair[1] == pair[3]


def settled(timeout, label):
    """The (status, action) both cards agree on, once neither is mid-press.

    A window's accessibility tree can trail its own DOM by a frame, so a pair
    counts only when two reads a moment apart both carry it, with the same
    press on both: one lucky read in the middle of a repaint is not agreement,
    and a page whose button has not landed yet reads as no press at all rather
    than as a card that offers something else.
    """
    deadline = time.monotonic() + timeout
    seen = None
    while time.monotonic() < deadline:
        pair = quiet()
        if pair is not None and pair == seen and pair[1] == pair[3]:
            return pair[0], pair[1]
        seen = pair
        time.sleep(0.05)
    page_status, page_check, page_action, native_status, native_action, _ = cards()
    raise Failure(
        f"the cards never agreed after the {label} press: "
        f"page {(page_status, page_check, page_action)!r}, "
        f"native {(native_status, native_action)!r}"
    )


def exercise(label, frame_name, buttons, busy_text, timeout, busy_window):
    """Press one card's button, watch both cards during it, settle.

    Returns (presses, mid_text, status, action). The busy step must be seen on
    both cards at the same moment, with the page's line the native card's word
    for word -- a decoration the page added would fail here -- and both cards
    must leave it agreeing.
    """
    wait_until(idle, timeout, f"the cards to be idle before the {label} press")

    for presses in range(1, 4):
        if not press(frame_name, buttons):
            raise Failure(f"the {label} press could not be made")
        started = time.monotonic()
        while time.monotonic() - started < busy_window:
            page_status, _, _, native_status, _, refusal = cards()
            page_status = page_status or ""
            native_status = native_status or ""
            if busy_text in page_status and native_status == busy_text:
                if page_status != native_status:
                    raise Failure(
                        f"mid-press the cards disagree: page {page_status!r} "
                        f"vs native {native_status!r}"
                    )
                status, action = settled(timeout, label)
                return presses, page_status, status, action
            if refusal:
                break
            time.sleep(POLL)
        wait_until(idle, timeout, f"the cards to be idle after the {label} press")

    raise Failure(
        f"the {label} press never showed {busy_text!r} on both cards "
        f"within {busy_window:.1f}s"
    )


def staged_copy(status, asset):
    """The staged file the app named in `status`, checked against `asset`.

    A staged state is a sentence and a file: the file has to be there, carry the
    feed's asset name, hold the asset's bytes, and be executable -- otherwise
    the update is announced but not installable, which is exactly what a status
    line alone cannot tell.
    """
    if not status.startswith(DOWNLOADED_PREFIX) or not status.endswith(DOWNLOADED_SUFFIX):
        raise Failure(f"the download did not report a staged copy: {status!r}")
    path = Path(status[len(DOWNLOADED_PREFIX) : -len(DOWNLOADED_SUFFIX)])
    if not path.is_file():
        raise Failure(f"the app named a staged copy that is not there: {path}")
    if path.name != STAGED_ASSET:
        raise Failure(f"the staged copy is named {path.name!r}, not {STAGED_ASSET!r}")
    if path.read_bytes() != asset.read_bytes():
        raise Failure(f"the staged copy is not the feed's asset: {path}")
    mode = path.stat().st_mode & 0o777
    if not mode & 0o111:
        raise Failure(f"the staged copy is not executable (mode {mode:o}): {path}")
    return path


def write_feed(root, marker=None):
    """A newer release whose asset is a local file: the feed and the asset.

    With `marker`, the asset is a stand-in build for the restart round: it
    records the path it was started from and stays up, so the round can see the
    staged copy run and stop it again afterwards.
    """
    asset = root / "muse-harness.AppImage"
    if marker is None:
        body = b"#!/bin/sh\n# harness asset: never executed, only staged\nexit 0\n" * 2048
    else:
        head = (
            "#!/bin/sh\n"
            "# the staged build the restart round relaunches: say where it ran\n"
            f'printf "%s %s\\n" "$0" "$$" >> {shlex.quote(str(marker))}\n'
            "sleep 30\n"
        ).encode()
        body = head + b"# padding: the download round stages a file of some size\n" * 512
    asset.write_bytes(body)
    sha = "0f1e2d3c4b5a69788796a5b4c3d2e1f0a1b2c3d4"
    feed = {
        "body": f"Rolling x86_64 AppImage built from {sha} by the AppImage workflow.",
        "assets": [
            {
                "name": STAGED_ASSET,
                "browser_download_url": asset.as_uri(),
                "size": len(body),
            }
        ],
    }
    feed_path = root / "feed.json"
    feed_path.write_text(json.dumps(feed))
    return feed_path, asset


def restart_round(proc, root, copy, asset, timeout):
    """Stage a harness release, press Restart, require the copy to take over.

    The native card drives it: the page window is the two-window rounds'
    subject, and leaving it out keeps this round on the relaunch -- and off
    WebKit, which a copy posing as a bundle has no reason to load here. The
    staged file must be what runs, it must still be up when the old process and
    its window are gone, and the file a launcher would open must now hold the
    staged bytes: the update is in place, not merely announced.
    """
    print(f"restart: staging into the throwaway copy at {copy}")
    if not press(MAIN_FRAME, CHECK_LABELS):
        raise Failure("the native Check press could not be made")
    wait_until(
        lambda: card(MAIN_FRAME)[0] == OFFERED_STATUS, timeout, "the update to be offered"
    )
    if not press(MAIN_FRAME, ("Download update",)):
        raise Failure("the native Download press could not be made")
    wait_until(
        lambda: card(MAIN_FRAME)[2] == "Restart to update", timeout, "the staged card"
    )
    staged = staged_copy(card(MAIN_FRAME)[0] or "", asset)

    if not press(MAIN_FRAME, ("Restart to update",)):
        raise Failure("the native Restart press could not be made")
    try:
        code = proc.wait(timeout=timeout)
    except subprocess.TimeoutExpired:
        raise Failure(f"the app was still running {timeout:.0f}s after the Restart press")
    if code != 0:
        raise Failure(f"the app exited {code} on the Restart press, not cleanly")
    wait_until(lambda: first_frame(MAIN_FRAME) is None, timeout, "the old Settings window to go")
    print("restart: the old app exited cleanly and its window is gone")

    marker = root / "relaunched"
    wait_until(marker.exists, timeout, "the staged copy to run")
    ran, pid = marker.read_text().split()[-2:]
    if Path(ran) != staged:
        raise Failure(f"the relaunched copy ran from {ran!r}, not the staged {staged!r}")
    try:
        os.kill(int(pid), 0)
    except OSError:
        raise Failure(f"the relaunched copy (pid {pid}) did not stay up")
    wait_until(
        lambda: copy.read_bytes() == staged.read_bytes(),
        timeout,
        f"{copy} to hold the staged build",
    )
    print(
        f"restart: the staged copy runs as pid {pid} from {ran}, and {copy} "
        "now holds its bytes"
    )
    os.kill(int(pid), signal.SIGTERM)
    print("PASS: the Restart press relaunched the staged copy and left no old process")
    return True


def run(binary, timeout, busy_window, keep, env_script, feed_mode, delay_ms, no_appimage, restart=False):
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
    cache = root / "cache"
    cache.mkdir()
    log_path = root / "muse.log"

    env = os.environ.copy()
    env.pop("MUSE_SPIKE_CMD", None)
    env.pop("MUSE_UPDATE_FEED", None)
    env.setdefault("DISPLAY", os.environ.get("MUSE_DISPLAY", ":1"))
    env.update(
        {
            "XDG_CONFIG_HOME": str(root / "config"),
            # The staged download lands under the cache home; keep the real one
            # out of it.
            "XDG_CACHE_HOME": str(cache),
            "GDK_BACKEND": env.get("GDK_BACKEND", "x11"),
            "MUSE_PROBE": "1",
        }
    )
    if no_appimage:
        # No APPDIR: this copy does not update itself, so both cards must say
        # so and offer no press at all.
        env.pop("APPDIR", None)
    else:
        # A non-empty APPDIR is what makes this copy update itself, so the
        # card offers a real Check. The dir itself is never used.
        env["APPDIR"] = str(root)
    copy = None
    if restart:
        # The copy the Restart press replaces: a throwaway, so this round can
        # never move the binary the two-window rounds press. It keeps the name
        # `muse`: a copy under another name registers with the accessibility
        # bus as that other name, and the rounds look for this app by name.
        copy = root / "copy" / "muse"
        copy.parent.mkdir()
        shutil.copy2(binary, copy)
        copy.chmod(0o755)
        env["APPIMAGE"] = str(copy)
        env["MUSE_UPDATE_RESTART"] = "1"
        binary = str(copy)
    feed_path = asset = None
    if feed_mode == "newer" and not no_appimage:
        feed_path, asset = write_feed(root, root / "relaunched" if restart else None)
        env["MUSE_UPDATE_FEED"] = str(feed_path)
        env["MUSE_UPDATE_DELAY_MS"] = str(delay_ms)

    proc = None
    ok = False
    try:
        proc = open_settings(
            launch_command(binary, env_script), env, log_path, with_page=not restart
        )

        if restart:
            ok = restart_round(proc, root, copy, asset, timeout)
            return ok

        if no_appimage:
            wait_until(
                lambda: card(MAIN_FRAME)[0] == NOT_APPIMAGE
                and card(WEB_FRAME)[0] == NOT_APPIMAGE,
                timeout,
                "both cards to say this copy updates through its own channel",
            )
            offered = [
                button
                for frame_name in (WEB_FRAME, MAIN_FRAME)
                for button in button_names(frame_name)
                if button in CARD_BUTTONS
            ]
            if offered:
                raise Failure(
                    f"a copy that does not update itself still offered {offered!r}"
                )
            # Nothing is drawn to press, and the app refuses one anyway: an
            # attempted press on either card leaves both on its own sentence,
            # never on a busy step and never with a press offered.
            if press(WEB_FRAME, CHECK_LABELS) or press(MAIN_FRAME, CHECK_LABELS):
                raise Failure("a copy that updates elsewhere still took a check press")
            time.sleep(1.0)
            for frame_name in (WEB_FRAME, MAIN_FRAME):
                status, check, action, _ = card(frame_name)
                if status != NOT_APPIMAGE or check or action:
                    raise Failure(
                        f"the {frame_name} card moved off the refusal after a press: "
                        f"{status!r} with press {check!r} and action {action!r}"
                    )
            print(
                f"PASS: both cards read {NOT_APPIMAGE!r}, offered none of "
                f"{CARD_BUTTONS}, and stayed on it after an attempted press"
            )
            return True

        wait_until(idle, timeout, "both cards to show a settled status")
        page_status, _, page_action, native_status, native_action, _ = cards()
        print(f"opened: both cards read {page_status!r} (action {page_action!r})")

        for label, frame_name in (("page", WEB_FRAME), ("native", MAIN_FRAME)):
            presses, mid, status, action = exercise(
                f"{label} check", frame_name, CHECK_LABELS, CHECK_STATUS, timeout, busy_window
            )
            print(
                f"press {label} check: both mid-check on {mid!r}; both read "
                f"{status!r} with {action!r} in {presses} "
                f"{'press' if presses == 1 else 'presses'}"
            )
            if feed_mode == "newer" and (status, action) != (OFFERED_STATUS, "Download update"):
                dump_tree(MAIN_FRAME)
                dump_tree(WEB_FRAME)
                raise Failure(
                    f"the {label} press should offer {OFFERED_STATUS!r} with "
                    f"'Download update', got {status!r} with {action!r}"
                )

        if feed_mode == "newer":
            # Two presses in quick succession on each card: the second lands
            # while the first check is running, so the app's own busy flag
            # refuses it. Both cards must show that sentence, and neither may
            # be left on the busy step once the first check lands. The round
            # presses to see the refusal, rather than retrying the press until
            # it slips through.
            for pressed_frame in (MAIN_FRAME, WEB_FRAME):
                pressed = "native" if pressed_frame == MAIN_FRAME else "page"
                if not press_twice(pressed_frame, CHECK_LABELS):
                    raise Failure(
                        f"the {pressed} check could not be pressed twice in quick succession"
                    )
                wait_until(
                    lambda: card(MAIN_FRAME)[0] == CHECK_REFUSAL,
                    timeout,
                    f"the native card to refuse the {pressed} double press",
                )
                wait_until(
                    lambda: shows_text(WEB_FRAME, CHECK_REFUSAL),
                    timeout,
                    f"the page to show the refusal for the {pressed} double press",
                )
                wait_until(
                    idle,
                    timeout,
                    f"the refused {pressed} press to leave the running check intact",
                )
                status, check, action, native_status, native_action, _ = cards()
                if check == WORKING or status != native_status or action != native_action:
                    raise Failure(
                        f"after the {pressed} double press the cards read page "
                        f"{status!r}/{check!r} and native {native_status!r}/{native_action!r}"
                    )
                # The refusal described the step it was refused in: once the
                # check has landed, no refusal line may still be on screen.
                if shows_text(WEB_FRAME, CHECK_REFUSAL):
                    raise Failure(
                        f"a refusal line survived the settled {pressed} double press "
                        f"({status!r})"
                    )
                print(
                    f"pressed {pressed} check twice: both cards refused with "
                    f"{CHECK_REFUSAL!r}, then settled on {status!r} with {action!r}, "
                    f"neither left on {WORKING!r}"
                )

            # A window opened while a check is still running boots on the busy
            # snapshot: it must say so on its first paint, and the answer has
            # to reach it the way it reaches a window that was already up.
            if not press(MAIN_FRAME, CHECK_LABELS):
                raise Failure("the native check could not be pressed to open a window on")
            if not press(MAIN_FRAME, (WEB_VIEW_BUTTON,)):
                raise Failure("a Settings web view could not be opened during the check")

            def booted_busy():
                views = frames_named(WEB_FRAME)
                if len(views) < 2:
                    return False
                return any(
                    card_of(view)[0] == CHECK_STATUS and card_of(view)[1] == WORKING
                    for view in views
                )

            wait_until(
                booted_busy, busy_window, "the new Settings web view to boot on the running check"
            )
            views = len(frames_named(WEB_FRAME))
            print(
                f"opened a Settings web view mid-check: {views} windows, the new one "
                f"booted on {CHECK_STATUS!r} with {WORKING!r}"
            )
            views = wait_web_windows(OFFERED_STATUS, "Download update", timeout, "mid-check boot")

            # Settled means off the busy step and on the native card's own
            # words -- not merely on the constant the round expected.
            native_status, _, native_action, _ = card(MAIN_FRAME)
            rendered = [card_of(view) for view in frames_named(WEB_FRAME)]
            busy = [
                value for value in rendered
                if value[0] in BUSY_STATUSES or value[1] == WORKING
            ]
            if busy:
                raise Failure(
                    f"a Settings window that booted mid-check was left on the busy "
                    f"step: {busy!r}"
                )
            disagree = [
                value for value in rendered
                if value[0] != native_status or value[2] != native_action
            ]
            if disagree:
                raise Failure(
                    f"the windows that booted mid-check settled on {disagree!r} while "
                    f"the native card read {native_status!r} with {native_action!r}"
                )
            print(
                f"the running check's answer reached all {views} Settings web views: "
                f"none left on {WORKING!r}, all on the native card's {native_status!r}"
            )

            # With both windows already up, a native press has to reach every
            # one of them: each takes the busy step and each leaves it for the
            # same answer. A registry that keeps only the newest window leaves
            # the older one showing what it already had -- never busy, never
            # told the answer.
            if not press(MAIN_FRAME, CHECK_LABELS):
                raise Failure("the native check could not be pressed with two windows up")

            def every_window_working():
                views = frames_named(WEB_FRAME)
                if len(views) < 2:
                    return False
                return all(
                    card_of(view)[1] == WORKING or card_of(view)[0] == CHECK_STATUS
                    for view in views
                )

            wait_until(
                every_window_working,
                busy_window,
                "every Settings window to take the busy step for a native press",
            )
            views = len(frames_named(WEB_FRAME))
            print(f"pressed check on the native card: all {views} Settings windows took {WORKING!r}")
            views = wait_web_windows(
                OFFERED_STATUS, "Download update", timeout, "native check with two windows up"
            )
            print(f"all {views} Settings windows left {WORKING!r} for {OFFERED_STATUS!r}")

            # The press below has to reach both windows, so neither may be
            # left out of the registry: open one only if the round above did
            # not already leave two up.
            if len(frames_named(WEB_FRAME)) < 2:
                if not press(MAIN_FRAME, (WEB_VIEW_BUTTON,)):
                    raise Failure("a second Settings web view could not be opened")
                wait_until(
                    lambda: len(frames_named(WEB_FRAME)) >= 2, 20, "the second Settings window"
                )
            print(f"opened {len(frames_named(WEB_FRAME))} Settings web views")

            # The seam only ever served a healthy release, so the updater's
            # failure sentences had never been seen live: a feed that cannot be
            # read, a feed that reads but is not a release, and a release whose
            # asset is gone. Each must land on every card as the app's own
            # sentence, with the press its kind of failure leaves still there,
            # and each must leave a press working once the seam is whole again.
            def round_failure(label, buttons, busy_text, expected, offered):
                """One broken-seam press: its sentence on every card."""
                _, _, status, action = exercise(
                    label, WEB_FRAME, buttons, busy_text, timeout, busy_window
                )
                if not status.startswith(expected) or action != offered:
                    raise Failure(f"the {label} settled on {status!r} with {action!r}")
                views = wait_web_windows(status, offered, timeout, label)
                print(
                    f"{label}: all {views} Settings views read {status!r}, "
                    f"offering {action!r}"
                )

            feed_text = feed_path.read_text()
            asset_bytes = asset.read_bytes()

            def whole_feed():
                if feed_path.is_dir():
                    feed_path.rmdir()
                feed_path.write_text(feed_text)

            def unreadable_feed():
                feed_path.unlink()
                feed_path.mkdir()

            def junk_feed():
                feed_path.write_text("this is not a release")

            for label, break_it, expected in (
                ("unreadable feed", unreadable_feed, CHECK_FAILED),
                ("invalid feed", junk_feed, INVALID_FEED),
            ):
                break_it()
                try:
                    round_failure(label, CHECK_LABELS, CHECK_STATUS, expected, None)
                finally:
                    whole_feed()

                _, _, status, action = exercise(
                    f"{label} with the feed whole again",
                    WEB_FRAME,
                    CHECK_LABELS,
                    CHECK_STATUS,
                    timeout,
                    busy_window,
                )
                if status != OFFERED_STATUS or action != "Download update":
                    raise Failure(
                        f"the press after the {label} settled on {status!r} with {action!r}"
                    )
                print(f"after the {label}: a press still works, reading {status!r}")

            asset.unlink()
            try:
                round_failure(
                    "missing asset",
                    ("Download update",),
                    DOWNLOAD_STATUS,
                    DOWNLOAD_FAILED,
                    "Download update",
                )
            finally:
                asset.write_bytes(asset_bytes)

            presses, mid, status, action = exercise(
                "page download",
                WEB_FRAME,
                ("Download update",),
                DOWNLOAD_STATUS,
                timeout,
                busy_window,
            )
            staged = staged_copy(status, asset)
            if action != "Restart to update":
                raise Failure(f"the staged card should offer Restart, got {action!r}")
            views = wait_web_windows(status, action, timeout, "download")
            print(
                f"press page download: both mid-download on {mid!r}; "
                f"all {views} Settings web views staged {status!r} with {action!r}"
            )
            print(
                f"staged copy: {staged.name} holds the feed's {staged.stat().st_size} "
                f"bytes and is executable (mode {staged.stat().st_mode & 0o777:o})"
            )

            # The press the harness must not let through: a relaunch would
            # replace the copy under test.
            if not press(WEB_FRAME, ("Restart to update",)):
                raise Failure("the staged card's Restart button could not be pressed")
            def refused():
                native, _, native_action, _ = card(MAIN_FRAME)
                views = [card_of(view) for view in frames_named(WEB_FRAME)]
                if native != RESTART_REFUSAL:
                    return False
                return bool(views) and all(
                    view[0] == RESTART_REFUSAL and view[2] == native_action for view in views
                )

            wait_until(refused, timeout, "every card to report the restart refusal")
            if proc.poll() is not None:
                raise Failure("the app exited on the Restart press")
            if first_frame(MAIN_FRAME) is None:
                raise Failure("the app's window went away on the Restart press")
            print(
                f"press page restart: every card refused with {RESTART_REFUSAL!r}; "
                "the app is still running"
            )

        page_status, page_check, page_action, native_status, native_action, _ = cards()
        if page_status != native_status or page_action != native_action:
            raise Failure(
                f"the cards disagree at the end: page {page_status!r}/{page_action!r} "
                f"vs native {native_status!r}/{native_action!r}"
            )
        if page_check == WORKING:
            raise Failure("the page was left on 'Working…'")
        print(
            f"PASS: both cards read {page_status!r} with {page_action!r}; "
            "neither is on the busy step"
        )
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
        "--feed",
        choices=("newer", "real"),
        default="newer",
        help="newer: a local fake release that offers an update (default); "
        "real: the release API, asserting only that the cards agree",
    )
    parser.add_argument(
        "--timeout", type=float, default=25.0, help="settle budget in seconds (default: 25)"
    )
    parser.add_argument(
        "--busy-window",
        type=float,
        default=5.0,
        help="how long a press may take to show its work starting (default: 5)",
    )
    parser.add_argument(
        "--delay-ms",
        type=int,
        default=2500,
        help="how long the harness feed takes to answer, so the busy step can be read "
        "(default: %(default)s)",
    )
    parser.add_argument(
        "--no-appimage",
        action="store_true",
        help="run as a copy that does not update itself: both cards must say so and "
        "offer no Check or Download press",
    )
    parser.add_argument(
        "--restart",
        action="store_true",
        help="run only the restart round: stage a harness release into a throwaway "
        "copy, press Restart, and require the staged copy to take over",
    )
    parser.add_argument("--keep", action="store_true", help="keep the temp config and log")
    parser.add_argument(
        "--env-script",
        default=str(Path.home() / ".local/share/muse-toolbox/env.sh"),
        help="sourced before the app starts, as muse-gui.sh does (default: %(default)s)",
    )
    args = parser.parse_args()
    if args.restart and (args.feed != "newer" or args.no_appimage):
        parser.error(
            "--restart stages the harness's own release into a throwaway copy, so it "
            "needs the default feed and an AppImage copy"
        )

    def interrupted(signum, frame):
        raise Failure("interrupted")

    signal.signal(signal.SIGINT, interrupted)
    try:
        return (
            0
            if run(
                args.binary,
                args.timeout,
                args.busy_window,
                args.keep,
                args.env_script,
                args.feed,
                args.delay_ms,
                args.no_appimage,
                args.restart,
            )
            else 1
        )
    except Failure as failure:
        print(f"FAIL: {failure}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
