#!/usr/bin/env bash
# Report a crate's state on crates.io. With a version: absent | yanked |
# published. Without: never-published | exists.
#
# Uses the JSON API rather than the sparse index: the index is CDN-cached for
# 600s and this runs seconds after a publish. The API needs a User-Agent —
# without one it answers 403.
#
# Refuses rather than guesses. A dropped connection, an unexpected status, or
# a 200 whose body cannot be parsed all exit non-zero, because the caller's
# only remedy for a wrong "absent" is `cargo yank`, which spends the version
# slot permanently. Callers must treat a non-zero exit as fatal — `set -e`
# does not see through `$(...)` in a `[` test or an unmatched `case`.
set -euo pipefail

if [ "$#" -lt 1 ] || [ "$#" -gt 2 ]; then
	echo "usage: ${0##*/} <crate> [version]" >&2
	exit 2
fi

crate=$1
version=${2-}
ua="skuld-release-pipeline (https://github.com/bindreams/skuld)"
url="https://crates.io/api/v1/crates/${crate}${version:+/$version}"

# `--retry 3` covers transient 5xx and connection failures. Not
# `--retry-all-errors`: without `--fail`, curl treats any HTTP response as a
# successful transfer, so it would never engage for status codes anyway.
resp=$(curl -sL -A "$ua" -w $'\n%{http_code}' --retry 3 --max-time 30 "$url") || resp=$'\n000'
code=${resp##*$'\n'}
body=${resp%$'\n'*}

if [ "$code" = 404 ]; then
	if [ -n "$version" ]; then echo absent; else echo never-published; fi
	exit 0
fi

if [ "$code" != 200 ]; then
	echo "${0##*/}: unexpected HTTP $code from crates.io for ${crate} ${version} — refusing to guess its state" >&2
	exit 1
fi

if [ -z "$version" ]; then
	echo exists
	exit 0
fi

# A 200 does not mean usable: crates.io serves yanked versions too, and a
# yanked version's slot is spent forever. `-e` so a body that is not the JSON
# we expect — an HTML error page, an errors object — refuses instead of
# defaulting to "published".
if ! yanked=$(printf '%s' "$body" | jq -er '.version.yanked | tostring'); then
	echo "${0##*/}: unreadable crates.io response for ${crate} ${version} — refusing to guess its state" >&2
	exit 1
fi

case "$yanked" in
	true) echo yanked ;;
	false) echo published ;;
	*)
		echo "${0##*/}: unexpected .version.yanked='$yanked' for ${crate} ${version}" >&2
		exit 1
		;;
esac
