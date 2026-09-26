#!/usr/bin/env bash
# Print the name of every publishable workspace member, one per line.
#
# Fails closed: an empty result is an error, never "there are none". Both
# release workflows gate an irreversible step on this list, so a silently
# empty list would read as "everything is already done".
#
# No `mapfile`: this is also meant to be run by hand during recovery (see
# CONTRIBUTING.md), and macOS ships bash 3.2, which does not have it.
set -euo pipefail

members=$(cargo metadata --no-deps --format-version 1 | jq -r '.packages[] | select(.publish != []) | .name')

if [ -z "$members" ]; then
	echo "${0##*/}: no publishable workspace members found — refusing to guess" >&2
	exit 1
fi

printf '%s\n' "$members"
