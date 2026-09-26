#!/usr/bin/env bash
# Print the name of every publishable workspace member, one per line.
#
# Fails closed: an empty result is an error, never "there are none". Both
# release guards branch on this list and one gates an irreversible step, so a
# silent empty list would read as "everything is already done".
#
# No `mapfile`: this is also run by hand during recovery, and macOS ships bash
# 3.2, where `mapfile` does not exist and would fail with an empty result.
set -euo pipefail

members=$(cargo metadata --no-deps --locked --format-version 1 \
  | jq -r '.packages[] | select(.publish != []) | .name')

if [ -z "$members" ]; then
  echo "::error::No publishable workspace members found — refusing to guess." >&2
  exit 1
fi

printf '%s\n' "$members"
