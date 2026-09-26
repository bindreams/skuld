#!/usr/bin/env bash
# Report a crate's state, at one version, on crates.io: never-published |
# absent | yanked | "published <checksum>". The checksum is crates.io's own
# sha256 of the uploaded `.crate` file, so a caller can compare it against a
# local build's own hash to prove — not assume — that "published" is *this*
# build.
#
# Queries the crate as a whole (`/api/v1/crates/<crate>`), not the per-version
# endpoint: its `versions` array carries every version's `yanked` flag and
# checksum, so one request answers both "does this crate exist at all" and
# "what is this version's state" — a caller that needs both (draft-release.yaml
# does, for every member) needs one request per crate instead of two.
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

if [ "$#" -ne 2 ]; then
	echo "usage: ${0##*/} <crate> <version>" >&2
	exit 2
fi

crate=$1
version=$2
ua="skuld-release-pipeline (https://github.com/bindreams/skuld)"
url="https://crates.io/api/v1/crates/${crate}"

fail() {
	local msg=$1
	if [ "${GITHUB_ACTIONS:-}" = "true" ]; then
		echo "::error::${msg}" >&2
	else
		echo "${0##*/}: ${msg}" >&2
	fi
	exit 1
}

# `--retry-max-time` bounds the retry phase by elapsed time instead of a fixed
# attempt count, so a burst of transient failures does not eat all retries
# before a real backoff window has passed. curl's own retry logic already
# honors a `Retry-After` on 429/503 as long as `--retry` is set — no sleep of
# our own. `--max-time` is the hard cap on the whole call, retries included.
if ! resp=$(curl -sL -A "$ua" -w $'\n%{http_code}' --retry 5 --retry-max-time 30 --max-time 45 "$url"); then
	fail "curl exit $? contacting crates.io for ${crate} — refusing to guess its state"
fi
code=${resp##*$'\n'}
body=${resp%$'\n'*}

if [ "$code" = 404 ]; then
	echo never-published
	exit 0
fi

if [ "$code" != 200 ]; then
	fail "unexpected HTTP $code from crates.io for ${crate} — refusing to guess its state"
fi

# `versions` must actually be an array before it is indexed below — an HTML
# error page or an `{"errors": [...]}` body would otherwise read as "no
# matching version" (i.e. absent) instead of refusing.
kind=$(printf '%s' "$body" | jq -er '.versions | type') || fail "unreadable crates.io response for ${crate} — refusing to guess its state"
if [ "$kind" != array ]; then
	fail "unreadable crates.io response for ${crate}: '.versions' is ${kind}, not an array — refusing to guess its state"
fi

entry=$(printf '%s' "$body" | jq -c --arg v "$version" '[.versions[] | select(.num == $v)][0] // empty')
if [ -z "$entry" ]; then
	echo absent
	exit 0
fi

yanked=$(printf '%s' "$entry" | jq -er '.yanked | tostring') || fail "unreadable version entry for ${crate} ${version} — refusing to guess its state"
case "$yanked" in
	true) echo yanked ;;
	false)
		checksum=$(printf '%s' "$entry" | jq -er '.checksum') || fail "unreadable checksum for ${crate} ${version} — refusing to guess its state"
		echo "published $checksum"
		;;
	*) fail "unexpected .yanked='$yanked' for ${crate} ${version}" ;;
esac
