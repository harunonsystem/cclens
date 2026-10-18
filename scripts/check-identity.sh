#!/usr/bin/env bash
# Fail when a commit would publish an identity other than the GitHub noreply
# address. See .claude/rules/session-data-privacy.md.
#   check-identity.sh             the identity the next commit will use (pre-commit)
#   check-identity.sh <rev>...    every commit `git log <rev>...` lists (pre-push, CI)
set -euo pipefail

allowed='34958457+harunonsystem@users.noreply.github.com'
# Merges made in the GitHub UI are committed by GitHub itself.
github='noreply@github.com'

fail=0
check() { # <label> <author-email> <committer-email>
  if [[ $2 != "$allowed" ]]; then
    echo "$1: author email is not $allowed" >&2
    fail=1
  fi
  if [[ $3 != "$allowed" && $3 != "$github" ]]; then
    echo "$1: committer email is not $allowed" >&2
    fail=1
  fi
}
email() { sed -E 's/.*<([^>]*)>.*/\1/'; }

if (($# == 0)); then
  check "next commit" "$(git var GIT_AUTHOR_IDENT | email)" "$(git var GIT_COMMITTER_IDENT | email)"
else
  while read -r sha ae ce; do
    check "$sha" "$ae" "$ce"
  done < <(git log --format='%h %ae %ce' "$@")
fi

if ((fail)); then
  echo "Set it with: git config user.email $allowed" >&2
fi
exit "$fail"
