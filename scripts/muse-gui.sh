#!/bin/sh
# Ensure Muse runs on an X display with a viewable window, then print
# "window=<id> pid=<pid> display=<d> state=<s>".
#
# Why this exists: XGetImage (scripts/x11-shot.py) returns BadMatch on a window
# that is not viewable, a click into a hidden window goes nowhere, and
# Xwayland's root window cannot be read at all -- so every capture and every
# click needs a mapped window and its id, which this prints.
#
# Usage: scripts/muse-gui.sh [--relaunch]
#   MUSE_DISPLAY  X display to use (default :1)
set -u
ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
HERE=$(dirname -- "$0")
. "$HOME/.local/share/muse-toolbox/env.sh"
# Resolved after the toolbox env: it can point TMPDIR into the session's cache
# dir, and the log the app actually writes has to be the one this prints.
LOG="${TMPDIR:-/tmp}/muse-gui.log"
DISPLAY="${MUSE_DISPLAY:-:1}"
export DISPLAY GDK_BACKEND=x11

if [ "${1:-}" = "--relaunch" ]; then
    for pid in $(pgrep -x muse); do kill "$pid"; done
    sleep 1
fi

window_id() {
    xwininfo -root -tree 2>/dev/null | awk '/"Muse": \("muse" "muse"\)/ {print $1; exit}'
}

map_state() {
    xwininfo -id "$1" 2>/dev/null | awk -F': ' '/Map State/ {print $2}'
}

launched=0
start_muse() {
    ( cd "$ROOT" && setsid nohup ./target/debug/muse >"$LOG" 2>&1 & )
    launched=1
}

pgrep -x muse >/dev/null || start_muse

tries=0
id=""
while [ "$tries" -lt 60 ]; do
    id=$(window_id)
    if [ -n "$id" ]; then
        [ "$(map_state "$id")" = "IsViewable" ] && break
        # Hidden or unmapped: ask for normal state and activation, then look again.
        if [ "$tries" -gt 4 ]; then
            python3 "$HERE/x11-window.py" unminimize "$id" >/dev/null 2>&1 || true
            python3 "$HERE/x11-window.py" raise "$id" >/dev/null 2>&1 || true
        fi
    elif [ "$tries" -ge 8 ] && [ "$launched" -eq 0 ]; then
        # A muse process with no window is a dying or wedged one (a zombie
        # still answers pgrep): start a fresh instance rather than wait out
        # the loop on a process that will never map anything.
        start_muse
    fi
    tries=$((tries + 1))
    sleep 0.5
done

if [ -z "$id" ]; then
    echo "muse-gui: no Muse window appeared on $DISPLAY (log: $LOG)" >&2
    exit 1
fi
state=$(map_state "$id")
echo "window=$id pid=$(pgrep -x muse | head -1) display=$DISPLAY state=$state log=$LOG"
[ "$state" = "IsViewable" ]
