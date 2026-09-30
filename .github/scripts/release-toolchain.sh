#!/usr/bin/env bash
# Read the release toolchain pin and print it, or fail.
#
# Both release stages install exactly this toolchain, because different cargo
# versions can package one tree into different bytes, and stage 2 holds its
# re-derived archives to the build job's hashes.
#
# This lives in a script, like normalize-version.sh, because both stages must
# accept exactly the same spellings: the value ends up in a toolchain
# installer's command line. The grammar is also written in
# .github/actions/install-toolchain/action.yaml and .github/renovate.json;
# change all three together.
#
# The file is read once, bounded, into a temporary copy, so the NUL check and the
# grammar check see the same bytes. Errors quote the file's content with
# `printf %q`, so it cannot forge workflow commands.
set -euo pipefail
# Not known to matter for the grammar below (no locale was found where it
# changes an outcome); kept as defence and untested.
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

copy=$(mktemp)
trap 'rm -f -- "${copy:?}"' EXIT

# The read is bounded: git stores symlinks, so the pin may point at an endless
# device such as /dev/zero.
max_bytes=64
head -c "$max_bytes" -- "$file" > "$copy" 2>/dev/null ||
	fail "$(printf '%q' "$file") could not be read (missing, not a regular file, or unreadable). The commit being released must contain the toolchain pin; see CONTRIBUTING.md."

size=$(wc -c < "$copy")
if [ "$size" -ge "$max_bytes" ]; then
	fail "$(printf '%q' "$file") is ${max_bytes} bytes or longer; a pin is one short X.Y.Z line."
fi

# Command substitution drops NUL bytes, which would let `1.98\0.1` read as 1.98.1.
nuls=$(tr -cd '\0' < "$copy" | wc -c)
if [ "$nuls" -ne 0 ]; then
	fail "$(printf '%q' "$file") contains a NUL byte."
fi

pin=$(cat -- "$copy")
if ! [[ "$pin" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]]; then
	fail "$(printf '%q' "$file") must be one exact X.Y.Z line, got $(printf '%q' "$pin")."
fi

printf '%s\n' "$pin"
