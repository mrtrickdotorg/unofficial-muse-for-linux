#!/usr/bin/env bash
# Point this checkout at the repository's committed hooks.
#
# git does not clone hooks, so a fresh clone has none until this runs: the hook
# lives in .githooks/commit-msg (rejecting the Codebuff attribution that
# AGENTS.md bans) and core.hooksPath is what makes git look there. Run it once
# per clone or worktree; --check reports whether it is still active.
#
# Usage: scripts/setup-hooks.sh [--check]
set -euo pipefail

readonly HOOKS='.githooks'
root=$(git rev-parse --show-toplevel) || {
    printf 'error: not inside a git work tree\n' >&2
    exit 1
}
cd "$root"

if [[ ! -x "$HOOKS/commit-msg" ]]; then
    printf 'error: %s/commit-msg is missing or not executable\n' "$HOOKS" >&2
    exit 1
fi

configured=$(git config --get core.hooksPath || true)

if [[ "${1:-}" == '--check' ]]; then
    if [[ "$configured" != "$HOOKS" ]]; then
        printf 'hooks are not active: core.hooksPath is %s, expected %s\n' \
            "${configured:-unset}" "$HOOKS" >&2
        printf 'Run scripts/setup-hooks.sh in this checkout.\n' >&2
        exit 1
    fi
    printf 'hooks active: core.hooksPath=%s\n' "$configured"
    exit 0
fi

git config --local core.hooksPath "$HOOKS"
printf 'hooks active: core.hooksPath=%s (%s/commit-msg)\n' "$HOOKS" "$HOOKS"
