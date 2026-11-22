# Project instructions

## No AI attribution in commit messages

Never add an attribution trailer to a commit message: no `Generated with Codebuff`
line, no `Co-Authored-By: Codebuff <noreply@codebuff.com>`. A message ends at its
own last line.

The rule covers published history too, not only new commits: every message here is
free of the trailer, and putting one back costs a force-push that rewrites hashes
to take it out again. `scripts/check-commit-messages.sh` fails a build whose range
carries one; the same script works as a local hook
(`ln -s ../../scripts/check-commit-messages.sh .git/hooks/commit-msg`), which is
how a commit gets refused before it exists.
