#!/usr/bin/env bash
# Offline account gates: stdlib unittest with fakes, no network or credentials.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
export PYTHONDONTWRITEBYTECODE=1

# The tests create a private config dir and prove it: every component of its path
# must be owned by us with no group/other bits. /tmp is not that on every host
# (it can belong to nobody and be merely sticky), and a checkout can sit in a
# group-writable tree, so the scratch dir goes under the session runtime dir or
# $HOME instead. The proof itself is untouched: only where the tests work
# changes.
private_base() {
    local base=$1 owner mode
    [ -n "$base" ] && [ -d "$base" ] || return 1
    case "$base" in /*) ;; *) return 1 ;; esac
    owner=$(stat -c %u -- "$base" 2>/dev/null) || return 1
    [ "$owner" = "$(id -u)" ] || return 1
    mode=$(stat -c %a -- "$base" 2>/dev/null) || return 1
    [ $((0$mode & 077)) -eq 0 ] || return 1
}

SCRATCH=
for base in "${XDG_RUNTIME_DIR:-}" "${HOME:-}"; do
    private_base "$base" || continue
    SCRATCH=$(mktemp -d -- "$base/muse-account-gate.XXXXXXXX" 2>/dev/null) && break
    SCRATCH=
done
if [ -n "$SCRATCH" ]; then
    TMPDIR="$SCRATCH"
    export TMPDIR
    # EXIT only: an INT/TERM handler would remove the scratch dir under the
    # suite still running in the foreground. Bash runs the EXIT trap on
    # interruption too.
    trap 'rm -rf -- "$TMPDIR"' EXIT
    echo "Test scratch directory: $TMPDIR"
else
    echo "warning: neither XDG_RUNTIME_DIR nor HOME is a private directory;" >&2
    echo "         tests fall back to ${TMPDIR:-/tmp}, where the config-dir proof may fail." >&2
fi
shopt -s globstar nullglob
for script in "$ROOT"/scripts/**/*.sh; do
    bash -n "$script"
done
/usr/bin/python3 - <<'PY'
from pathlib import Path

scripts = sorted(Path("scripts").rglob("*.py"))
for script in scripts:
    compile(script.read_bytes(), str(script), "exec")
print(f"Python syntax checked: {len(scripts)} files")
PY
# Also parse the installer's embedded Python, without installing anything.
PATH="/usr/bin:/bin:$PATH" bash "$ROOT/scripts/setup-account-bridge.sh" --help >/dev/null
# Wire-contract tests use the selected external runtime's real protobuf classes,
# but fake all gateway IO. Nothing authenticates or reads an account session.
# Those tests import the bridge's own `muse` module, so an interpreter without
# the bridge skips them -- and this gate fails a skip. Rather than requiring
# every caller to export the path, find the interpreter the bridge was
# installed with: an explicit MUSE_ACCOUNT_TEST_PYTHON wins, otherwise the
# installation scripts/setup-account-bridge.sh writes is used, at the same path
# and revision that installer pins. A missing, stale or unusable bridge fails
# the gate here, with what to run, rather than passing on skipped tests.
if [ -n "${XDG_DATA_HOME:-}" ]; then
    BRIDGE_ROOT="$XDG_DATA_HOME/unofficial-muse-for-linux/account-bridge"
else
    BRIDGE_ROOT="$HOME/.local/share/unofficial-muse-for-linux/account-bridge"
fi
PIN_COMMIT=$(sed -n 's/^SOURCE_COMMIT = "\([0-9a-f]\{40\}\)"$/\1/p' "$ROOT/scripts/setup-account-bridge.sh")
[ -n "$PIN_COMMIT" ] || {
    echo "error: scripts/setup-account-bridge.sh no longer states SOURCE_COMMIT," >&2
    echo "       so this gate cannot check which bridge revision the wire tests need." >&2
    exit 1
}

gate_stop() {
    printf 'error: %s\n' "$1" >&2
    shift
    for line in "$@"; do
        printf '       %s\n' "$line" >&2
    done
    exit 1
}

imports_bridge() {
    [ -n "${1:-}" ] && [ -x "$1" ] || return 1
    "$1" -I -c 'import muse' >/dev/null 2>&1
}

if [ -n "${MUSE_ACCOUNT_TEST_PYTHON:-}" ]; then
    TEST_PYTHON=$MUSE_ACCOUNT_TEST_PYTHON
    imports_bridge "$TEST_PYTHON" || gate_stop \
        "MUSE_ACCOUNT_TEST_PYTHON=$TEST_PYTHON cannot import the bridge module 'muse'." \
        "Point it at the .venv/bin/python that scripts/setup-account-bridge.sh printed." \
        "The wire tests need that runtime; they cannot be allowed to skip here."
else
    if [ ! -f "$BRIDGE_ROOT/installation.json" ]; then
        gate_stop "no account bridge is installed at $BRIDGE_ROOT." \
            "Install the pinned bridge as a regular user (it needs network and Python 3.10+):" \
            "    ./scripts/setup-account-bridge.sh" \
            "This gate then uses that installation by itself; no variable needs exporting." \
            "An installation elsewhere is selected with MUSE_ACCOUNT_TEST_PYTHON."
    fi
    INSTALLED_COMMIT=$(sed -n 's/.*"commit": "\([^"]*\)".*/\1/p' "$BRIDGE_ROOT/installation.json")
    INSTALLED_PYTHON=$(sed -n 's/.*"python": "\([^"]*\)".*/\1/p' "$BRIDGE_ROOT/installation.json")
    if [ "$INSTALLED_COMMIT" != "$PIN_COMMIT" ]; then
        gate_stop "the bridge at $BRIDGE_ROOT is commit ${INSTALLED_COMMIT:-unknown}, but scripts/setup-account-bridge.sh pins $PIN_COMMIT." \
            "The wire tests run against the pinned revision, so the installation and the pin must agree." \
            "Install the pinned revision into a new --prefix (the installer refuses an existing" \
            "directory) and point MUSE_ACCOUNT_TEST_PYTHON at the .venv/bin/python it prints, or" \
            "move the old installation aside and install again at $BRIDGE_ROOT."
    fi
    imports_bridge "$INSTALLED_PYTHON" || gate_stop \
        "the bridge at $BRIDGE_ROOT records interpreter ${INSTALLED_PYTHON:-<none>}, which cannot import 'muse'." \
        "A venv only works with the interpreter version that built it; a toolbox or container" \
        "python differs from the host's. Rebuild it where you run this gate:" \
        "    ./scripts/setup-account-bridge.sh --prefix <new directory>" \
        "then select that .venv/bin/python with MUSE_ACCOUNT_TEST_PYTHON."
    TEST_PYTHON=$INSTALLED_PYTHON
fi
printf 'Account wire tests use %s (bridge %s)\n' "$TEST_PYTHON" "$PIN_COMMIT"
"$TEST_PYTHON" -I - <<'PY'
import sys
import unittest

suite = unittest.defaultTestLoader.discover("scripts/tests", pattern="test_*.py")
result = unittest.TextTestRunner(verbosity=2).run(suite)
if result.skipped:
    print("Full offline gate rejects skipped tests. The selected interpreter could not "
          "import the installed bridge's `muse` module: reinstall the pinned bridge with "
          "scripts/setup-account-bridge.sh and select its .venv/bin/python.", file=sys.stderr)
sys.exit(0 if result.wasSuccessful() and not result.skipped else 1)
PY
