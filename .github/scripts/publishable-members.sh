#!/usr/bin/env bash
# Print the workspace's publishable member names, one per line — crates whose
# manifest does not set `publish = false`.
#
# `cargo metadata` reports a member's `publish` field as `null` (unrestricted)
# or a non-empty allow-list for anything publishable, both of which satisfy
# `.publish != []`. Only an explicit `publish = false` becomes the empty array
# `.publish == []` that this filter excludes.
#
# This lives in a script, alongside crate-index-path.sh and
# normalize-version.sh, because draft-release.yaml, publish-release.yaml, and
# CONTRIBUTING.md's recovery snippet all need the identical enumeration: a
# manifest-schema change updating one copy of this filter and missing the
# rest would silently desync what stage 1 checked from what stage 2 actually
# publishes.
set -euo pipefail

members=$(cargo metadata --no-deps --format-version 1 | jq -r '.packages[] | select(.publish != []) | .name')
if [ -z "$members" ]; then
	msg="no publishable workspace members found. Expected at least one."
	if [ "${GITHUB_ACTIONS:-}" = "true" ]; then
		echo "::error::${msg}"
	else
		echo "${0##*/}: ${msg}" >&2
	fi
	exit 1
fi

printf '%s\n' "$members"
