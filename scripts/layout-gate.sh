#!/usr/bin/env bash
# Layout gate: the shell-fit proofs, run against the GTK this
# environment provides. In CI this runs inside the AppImage builder
# container — bookworm GTK 4.8, the libraries the AppImage actually
# ships; a proof against a newer host GTK is how the Nov 12, 2026
# panel blowout shipped green. Locally it runs against the host
# GTK, which still catches budget regressions.
#
# Three virtual screens, because a window-manager-less Xvfb sizes
# windows to their content: the display IS the width being proven.
# Each test's ignore note names the display it expects.
set -euo pipefail
cd "$(dirname "$0")/.."

export GSK_RENDERER=cairo GTK_A11Y=none
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/tmp/runtime-root}"
mkdir -p "$XDG_RUNTIME_DIR"
chmod 700 "$XDG_RUNTIME_DIR" || true

run_one() {
    local display=$1 screen=$2 test_name=$3
    Xvfb "$display" -screen 0 "$screen" &
    local pid=$!
    sleep 2
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
printf 'layout gate: all three widths fit\n'
