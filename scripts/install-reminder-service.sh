#!/usr/bin/env bash
# Install the reminder runner as a systemd --user service.
#
# Usage: scripts/install-reminder-service.sh [path-to-muse-reminder-runner]
#
# Without an argument the binary is taken from target/release (build it
# with: cargo build --release -p muse-reminder-runner). AppImage users can
# extract the bundled binary first (it ships in usr/bin inside the image).
# The script installs the binary to ~/.local/bin, writes the unit with the
# real path, then enables and starts the service. Nothing runs as root and
# nothing outside the user's own directories is touched.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_SRC="${1:-$ROOT/target/release/muse-reminder-runner}"
BIN_DST="$HOME/.local/bin/muse-reminder-runner"
UNIT_DIR="$HOME/.config/systemd/user"
UNIT_DST="$UNIT_DIR/muse-reminder-runner.service"

if [[ ! -x "$BIN_SRC" ]]; then
    printf 'Runner binary not found or not executable: %s\n' "$BIN_SRC" >&2
    printf 'Build it with: cargo build --release -p muse-reminder-runner\n' >&2
    exit 2
fi
if ! command -v systemctl >/dev/null 2>&1; then
    printf 'systemctl not found; this installer needs a systemd user session.\n' >&2
    printf 'You can still run the runner by hand: %s\n' "$BIN_SRC" >&2
    exit 2
fi

mkdir -p "$HOME/.local/bin" "$UNIT_DIR"
install -m 755 "$BIN_SRC" "$BIN_DST"
# The unit template's ExecStart uses the default install path; rewrite it
# to the path actually installed so a moved HOME never silently breaks.
sed "s|^ExecStart=.*|ExecStart=$BIN_DST|" \
    "$ROOT/resources/systemd/muse-reminder-runner.service" > "$UNIT_DST"
chmod 644 "$UNIT_DST"

systemctl --user daemon-reload
systemctl --user enable --now muse-reminder-runner.service
printf 'Reminder runner installed and started. Status:\n'
systemctl --user --no-pager status muse-reminder-runner.service | head -5
printf 'Reminders fire from the same store the app uses (~/.config/unofficial-muse-for-linux/tasks).\n'
printf 'Stop it any time with: systemctl --user disable --now muse-reminder-runner.service\n'
