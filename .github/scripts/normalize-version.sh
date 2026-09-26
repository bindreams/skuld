#!/usr/bin/env bash
# Validate a release version string and print it back unchanged, or fail.
#
# Accepts three dot-separated non-negative integers with no leading zeros, no
# leading `v`, and no pre-release or build suffix (e.g. `1.2.3`).
#
# A leading `v` is rejected rather than stripped: draft-release.yaml and
# publish-release.yaml both key their concurrency group on the raw input, so
# if `1.2.3` and `v1.2.3` normalized to the same version they would land in
# *different* concurrency groups while naming the same release — two runs of
# the same publish, in parallel, on an operation that cannot be undone. One
# accepted spelling keeps key and meaning in step.
#
# Leading zeros are rejected too: `01.2.3` is not valid semver, so
# `semver::Version::parse` fails on it and xtask's tag map silently skips such
# a tag — the one check tying input to manifest would no-op for exactly those
# spellings.
#
# This lives in a script, alongside crate-index-path.sh, because both release
# workflows validate the same operator input the same way: a fix to one copy
# of this regex and not the other would silently reopen the double-publish
# race described above.
set -euo pipefail

if [ "$#" -ne 1 ]; then
	echo "usage: ${0##*/} <version>" >&2
	exit 2
fi

v="$1"
if ! [[ "$v" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]]; then
	msg="invalid version '${v}'. Expected three dot-separated numbers with no leading zeros, no leading 'v', and no pre-release or build suffix (e.g. 1.2.3)."
	if [ "${GITHUB_ACTIONS:-}" = "true" ]; then
		echo "::error::${msg}"
	else
		echo "${0##*/}: ${msg}" >&2
	fi
	exit 1
fi

printf '%s\n' "$v"
