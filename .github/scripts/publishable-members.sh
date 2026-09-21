#!/usr/bin/env bash
# Print the name of every publishable workspace member, one per line.
#
# Fails closed: an empty result is an error, never "there are none". Both
# release guards branch on this list, and one of them gates an irreversible
# step — a silent empty list would read as "everything is already done".
set -euo pipefail

mapfile -t members < <(cargo metadata --no-deps --locked --format-version 1 \
  | jq -r '.packages[] | select(.publish != []) | .name')

if [ "${#members[@]}" -eq 0 ]; then
  echo "::error::No publishable workspace members found — refusing to guess." >&2
  exit 1
fi

printf '%s\n' "${members[@]}"
