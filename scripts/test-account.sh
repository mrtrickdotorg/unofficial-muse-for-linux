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
TEST_PYTHON="${MUSE_ACCOUNT_TEST_PYTHON:-/usr/bin/python3}"
"$TEST_PYTHON" -I - <<'PY'
import sys
import unittest

suite = unittest.defaultTestLoader.discover("scripts/tests", pattern="test_*.py")
result = unittest.TextTestRunner(verbosity=2).run(suite)
if result.skipped:
    print("Full offline gate rejects skipped tests. Install the pinned bridge with "
          "scripts/setup-account-bridge.sh and set MUSE_ACCOUNT_TEST_PYTHON to its "
          "absolute .venv/bin/python path.", file=sys.stderr)
sys.exit(0 if result.wasSuccessful() and not result.skipped else 1)
PY
