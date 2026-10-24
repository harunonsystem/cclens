#!/usr/bin/env bash
# Check every commit about to leave the machine. Rebased or cherry-picked
# commits skip pre-commit and keep their original author. Reads git's pre-push
# stdin: <local ref> <local sha> <remote ref> <remote sha> per pushed ref.
set -euo pipefail

zero=0000000000000000000000000000000000000000
upstream=refs/remotes/upstream/main
while read -r _ local _ remote; do
  [[ $local == "$zero" ]] && continue # branch deletion
  range=()
  # Upstream commits are already public and not ours to fix.
  git rev-parse -q --verify "$upstream" >/dev/null && range=("^$upstream")
  if [[ $remote == "$zero" ]]; then
    range+=("$local" --not --remotes)
  else
    range+=("$remote..$local")
  fi
  scripts/check-identity.sh "${range[@]}"
  betterleaks git --log-opts="${range[*]}" --include=commit-messages \
    --config .betterleaks.toml --redact --no-banner
done
