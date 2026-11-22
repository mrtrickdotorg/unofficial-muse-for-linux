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
# when the before commit is real, the pull request's base ref otherwise, then
# origin/main..HEAD locally, and the head commit alone as the last resort -- so
# a shallow or unusual clone still checks something rather than nothing.
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
if [[ -z "$range" ]]; then
    before="${BEFORE:-}"
    after="${AFTER:-HEAD}"
    if [[ -n "$before" && "$before" != "$ZEROES" ]]; then
        range="$before..$after"
    elif [[ -n "${BASE_REF:-}" ]] && git rev-parse --verify --quiet "origin/$BASE_REF" >/dev/null; then
        range="origin/$BASE_REF..$after"
    elif git rev-parse --verify --quiet origin/main >/dev/null; then
        range="origin/main..HEAD"
    else
        range="$after"
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
printf 'checked %d commit message(s) in %s for attribution: clean\n' "$checked" "$range"
