#!/usr/bin/env bash
# Fail when a commit message carries the Codebuff attribution.
#
# Every message in this history is free of the "Generated with Codebuff" and
# "Co-Authored-By: Codebuff" lines (AGENTS.md records the rule), and a message
# that puts one back means rewriting published hashes again to take it out.
# This walks a revision range, or -- as a commit-msg hook -- the message file
# git hands the hook, and names the offending commits.
#
# Usage: scripts/check-commit-messages.sh [<rev-range>|<message-file>]
#
# As a hook: ln -s ../../scripts/check-commit-messages.sh .git/hooks/commit-msg
#
# With no argument it reads the CI environment: GitHub's push before/after pair
# when the before commit is real, the pull request's base ref, then
# origin/main..HEAD locally, and the head commit alone as the last resort.
#
# A range that resolves to no commits is an error in CI. A stale or missing base
# ref is exactly how a pull request ends up comparing a branch against itself,
# and a gate that reports "clean" without reading one message is worse than no
# gate. Locally the same case is quiet: a clean tree legitimately has nothing
# ahead of origin/main.
set -euo pipefail

readonly ZEROES=0000000000000000000000000000000000000000
# The trailer lines themselves, not the word: a message that *describes* the
# rule ("reject the Codebuff attribution") is not an attribution, and banning
# the token outright made this rule impossible to write about.
readonly SIGNATURE='^(🤖[[:space:]]*)?Generated with Codebuff([[:space:]]*🤖)?[[:space:]]*$'
readonly CO_AUTHOR='^[[:space:]]*Co-Authored-By:[[:space:]]*Codebuff[[:space:]]*<[^>]*>[[:space:]]*$'

is_attributed() {
    grep -E -i -e "$SIGNATURE" -e "$CO_AUTHOR" >/dev/null
}

in_ci() {
    [[ -n "${GITHUB_ACTIONS:-}" || -n "${CI:-}" ]]
}

# commit-msg hook: one message file, no history to walk.
if [[ $# -gt 0 && -f "$1" ]]; then
    if is_attributed <"$1"; then
        printf 'commit message must not carry the Codebuff trailer (see AGENTS.md)\n' >&2
        grep -E -i -e "$SIGNATURE" -e "$CO_AUTHOR" "$1" | sed 's/^/    /' >&2
        exit 1
    fi
    printf 'commit message has no attribution: %s\n' "$1"
    exit 0
fi

range="${1:-}"
basis=""
if [[ -z "$range" ]]; then
    before="${BEFORE:-}"
    after="${AFTER:-HEAD}"
    if [[ -n "$before" && "$before" != "$ZEROES" ]]; then
        range="$before..$after"
        basis="the push's before/after pair"
    elif [[ -n "${BASE_REF:-}" ]] && git rev-parse --verify --quiet "origin/$BASE_REF" >/dev/null; then
        range="origin/$BASE_REF..$after"
        basis="the pull request base origin/$BASE_REF"
    elif [[ -n "${BASE_REF:-}" ]] && in_ci; then
        # Do not fall through to another base here: silently comparing against
        # something else is how this gate ends up checking nothing.
        printf 'error: base ref origin/%s is missing, so there is nothing to compare against\n' "$BASE_REF" >&2
        printf 'The checkout needs that branch and its history (actions/checkout with fetch-depth: 0, or an explicit fetch of %s).\n' "$BASE_REF" >&2
        printf 'Refusing to check nothing (see AGENTS.md).\n' >&2
        exit 1
    elif git rev-parse --verify --quiet origin/main >/dev/null; then
        range="origin/main..$after"
        basis="origin/main"
    else
        range="$after"
        basis="the head commit alone, with no base ref available"
    fi
fi

# Merges are walked too: a merge message is a message.
commits=$(git rev-list "$range")
checked=0
failed=0
while read -r sha; do
    [[ -z "$sha" ]] && continue
    checked=$((checked + 1))
    message=$(git log -1 --format=%B "$sha")
    if is_attributed <<<"$message"; then
        subject=$(git log -1 --format=%s "$sha")
        printf '  %s %s\n' "${sha:0:12}" "$subject" >&2
        grep -E -i -e "$SIGNATURE" -e "$CO_AUTHOR" <<<"$message" | sed 's/^/    /' >&2
        failed=1
    fi
done <<<"$commits"

if ((failed)); then
    printf '\nCommit messages must not carry the Codebuff trailer (see AGENTS.md).\n' >&2
    exit 1
fi

if ((checked == 0)); then
    if in_ci; then
        printf 'error: %s resolved no commits, so nothing was checked\n' "$range" >&2
        [[ -n "$basis" ]] && printf 'The range came from %s.\n' "$basis" >&2
        printf 'A stale or missing base ref looks exactly like this, and a green gate must mean commits were read.\n' >&2
        printf 'Check out with full history (fetch-depth: 0) and confirm the base branch is present, then push again.\n' >&2
        exit 1
    fi
    printf 'no commits in %s; nothing to check (local run)\n' "$range"
    exit 0
fi

printf 'checked %d commit message(s) in %s for attribution: clean\n' "$checked" "$range"
if [[ -n "$basis" ]]; then
    printf 'range came from %s\n' "$basis"
fi
