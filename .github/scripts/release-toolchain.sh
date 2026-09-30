#!/usr/bin/env bash
# Read the release toolchain pin and print it, or fail.
#
# The pin is one exact `X.Y.Z` in `.github/release-toolchain` (a trailing
# newline is allowed). Both release stages install exactly that toolchain,
# because different cargo versions can package one tree into different bytes,
# and stage 2 holds its re-derived archives to the build job's hashes.
#
# This lives in a script, like normalize-version.sh, because stage 1
# (draft-release.yaml) and stage 2 (publish-release.yaml) must accept exactly
# the same spellings: the value ends up in a toolchain installer's command
# line, so anything looser would be an injection sink in the credentialed job.
#
# A malformed file is reported with its content shell-quoted, so a file
# containing `::error::`-style workflow commands cannot forge annotations.
set -euo pipefail
# Digits are ASCII digits, whatever the caller's locale.
export LC_ALL=C

if [ "$#" -ne 1 ]; then
	echo "usage: ${0##*/} <file>" >&2
	exit 2
fi

file="$1"

fail() {
	if [ "${GITHUB_ACTIONS:-}" = "true" ]; then
		echo "::error::$1" >&2
	else
		echo "${0##*/}: $1" >&2
	fi
	exit 1
}

if [ ! -f "$file" ]; then
	fail "$(printf '%q' "$file") does not exist. The commit being released must contain the toolchain pin; see CONTRIBUTING.md."
fi

pin=$(cat "$file")
if ! [[ "$pin" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
	fail "$(printf '%q' "$file") must be one exact X.Y.Z line, got $(printf '%q' "$pin")."
fi

printf '%s\n' "$pin"
