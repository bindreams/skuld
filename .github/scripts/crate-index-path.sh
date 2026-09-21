#!/usr/bin/env bash
# Print the sparse-index path for a crate name, without the host.
#
#   skuld        -> sk/ul/skuld
#   log          -> 3/l/log
#   os           -> 2/os
#   a            -> 1/a
#
# Index paths encode the name's length and are lowercased. A fixed 2+2 slice
# mis-encodes any name under 4 characters (e.g. `log` -> `log/log`), which 404s
# and reads as "never published" — silently, and permanently for that member.
#
# This lives in a script rather than inline in the workflow because the release
# recovery instructions in CONTRIBUTING.md need the identical computation: the
# two copies drifting is what this file exists to prevent.
set -euo pipefail

if [ "$#" -ne 1 ]; then
	echo "usage: ${0##*/} <crate-name>" >&2
	exit 2
fi

lc=$(printf '%s' "$1" | tr '[:upper:]' '[:lower:]')

case ${#lc} in
	0)
		echo "${0##*/}: empty crate name" >&2
		exit 2
		;;
	1) printf '1/%s\n' "$lc" ;;
	2) printf '2/%s\n' "$lc" ;;
	3) printf '3/%s/%s\n' "${lc:0:1}" "$lc" ;;
	*) printf '%s/%s/%s\n' "${lc:0:2}" "${lc:2:2}" "$lc" ;;
esac
