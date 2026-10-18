#!/usr/bin/env bash
# Check every commit about to leave the machine. Rebased or cherry-picked
# commits skip pre-commit and keep their original author. Reads git's pre-push
# stdin: <local ref> <local sha> <remote ref> <remote sha> per pushed ref.
set -euo pipefail

zero=0000000000000000000000000000000000000000
while read -r _ local _ remote; do
  [[ $local == "$zero" ]] && continue # branch deletion
  if [[ $remote == "$zero" ]]; then
    range=("$local" --not --remotes)
  else
    range=("$remote..$local")
  fi
  scripts/check-identity.sh "${range[@]}"
  betterleaks git --log-opts="${range[*]}" --include=commit-messages \
    --config .betterleaks.toml --redact --no-banner
done
