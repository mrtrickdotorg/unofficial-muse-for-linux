#!/usr/bin/env bash
# Layout gate: the shell-fit proofs, run against the GTK this
# environment provides. In CI this runs inside the AppImage builder
# container — Ubuntu 24.04 GTK 4.14, the libraries the AppImage actually
# ships; a proof against a newer host GTK is how the Nov 12, 2026
# panel blowout shipped green. Locally it runs against the host
# GTK, which still catches budget regressions.
#
# Virtual screens, because a window-manager-less Xvfb sizes
# windows to their content: the display IS the width being proven.
# Each proof gets its OWN display — reusing one across proofs
# raced the previous server's shutdown in the container's first
# CI run (Nov 12, 2026): the second server never came up and its
# proof died at GTK init, which is a harness failure, not a fit
# failure — but it blocks the release all the same, so displays
# are unique and each server is awaited, not assumed.
set -euo pipefail
cd "$(dirname "$0")/.."

export GSK_RENDERER=cairo GTK_A11Y=none
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/tmp/runtime-root}"
mkdir -p "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR" || true

run_one() {
    local display=$1 screen=$2 test_name=$3
    local number=${display#:}
    Xvfb "$display" -screen 0 "$screen" &
    local pid=$!
    # Wait for the server to accept connections (its socket to
    # appear); a server that lost a display race leaves no socket.
    local waited=0
    while [ ! -S "/tmp/.X11-unix/X${number}" ] && [ "$waited" -lt 20 ]; do
        sleep 0.5
        waited=$((waited + 1))
    done
    local status=0
    DISPLAY="$display" dbus-run-session -- \
        cargo test -p unofficial-muse-for-linux --locked "$test_name" \
        -- --ignored --exact --test-threads=1 || status=$?
    kill "$pid" 2>/dev/null || true
    if [ "$status" -ne 0 ]; then
        printf 'layout gate FAILED: %s on %s (%s)\n' "$test_name" "$display" "$screen" >&2
        exit "$status"
    fi
}

run_one :97 2560x1200x24 account_ui::native::tests::shell_row_fits_at_monitor_width
run_one :98 1280x800x24 account_ui::native::tests::side_panel_steps_aside_in_a_narrow_window
run_one :99 1920x1200x24 account_ui::native::tests::side_panel_fits_beside_the_page_at_a_laptop_width
run_one :100 1920x1200x24 account_ui::native::tests::side_panel_scrolls_instead_of_pushing_the_window
printf 'layout gate: all widths and heights fit\n'
