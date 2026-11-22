# Project instructions

## No AI attribution in commit messages

Never add an attribution trailer to a commit message: no `Generated with Codebuff`
line, no `Co-Authored-By: Codebuff <noreply@codebuff.com>`. A message ends at its
own last line.

The rule covers published history too, not only new commits: every message here is
free of the trailer, and putting one back costs a force-push that rewrites hashes
to take it out again. `scripts/check-commit-messages.sh` fails a build whose range
carries one, and `.githooks/commit-msg` refuses the commit before it exists. git
does not clone hooks, so run `scripts/setup-hooks.sh` once per checkout to point
`core.hooksPath` at `.githooks`; `scripts/setup-hooks.sh --check` says whether it
is still active.
