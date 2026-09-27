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
# our own.
#
# `--max-time` bounds one attempt, not the whole call — curl resets that
# timer before every retry — so it does NOT cap the total time the way a
# single top-level deadline would. The actual worst case is roughly
# `--retry-max-time` (how long curl keeps retrying) plus one more
# `--max-time` (the attempt that was in flight when the retry budget ran
# out). `--retry` is set far higher than any attempt count reachable within
# `--retry-max-time` seconds specifically so the count itself can never be
# the thing that cuts retries short — `--retry-max-time` is the real bound,
# not a number of attempts picked by guesswork.
#
# The body goes to a temp file via `-o`, not stdout: curl truncates that file
# before each retry, so only the last attempt's body survives there. Left on
# stdout (as `-w`'s own output also is), a failed attempt's body is never
# cleared between retries, and the next attempt's output would print right
# after it — one `jq` parse over what is now two concatenated response
# bodies, however that happens to fail. Keeping only `%{http_code}` on
# stdout means `code=$(curl ...)` captures exactly the final attempt's
# status and nothing else.
tmp=$(mktemp)
trap 'rm -f "$tmp"' EXIT
# The failure branch is `code=$(...) || { ... }`, not `if ! code=$(...); then
# ...`: inside an `if !`'s then-branch, `$?` reflects the negated `!` list,
# which is 0 whenever curl actually failed — reporting "curl exit 0" on every
# real failure. `||`'s right-hand side runs with `$?` still holding curl's own
# exit code, captured here before anything else can overwrite it. `-S` un-silences
# curl's own error line (`-s` alone swallows it) so that line still reaches the
# log even though stderr isn't captured by this script.
code=$(curl -sSL -A "$ua" -o "$tmp" -w '%{http_code}' --retry 1000 --retry-max-time 30 --max-time 45 "$url") || {
	rc=$?
	# curl writes literal `000` for %{http_code} when no response arrived at
	# all (e.g. DNS failure, connection refused) — that is the absence of a
	# code, not a code, so it is excluded here rather than reported as one.
	if [[ "$code" =~ ^[0-9]{3}$ ]] && [ "$code" != 000 ]; then
		fail "curl exit $rc contacting crates.io for ${crate} (last HTTP status seen: $code) — refusing to guess its state"
	else
		fail "curl exit $rc contacting crates.io for ${crate} — refusing to guess its state"
	fi
}
body=$(cat "$tmp")

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

# semver build metadata (the `+...` suffix) is excluded from equality by
# spec, but crates.io still records it in `.num` and still occupies the same
# X.Y.Z slot — `1.2.3+a` and `1.2.3+b` cannot both be published, and neither
# can be republished as bare `1.2.3`. Matching on the version core (`.num`
# with `+...` stripped) is what lets `1.2.3+meta` be found at all when the
# caller asks about `1.2.3`; the exact-match check below is what stops that
# from being silently treated as the same version.
matches=$(printf '%s' "$body" | jq -c --arg v "$version" '[.versions[] | select((.num | split("+")[0]) == $v)]') || fail "unreadable crates.io response for ${crate} — refusing to guess its state"
match_count=$(printf '%s' "$matches" | jq -er 'length') || fail "unreadable crates.io response for ${crate} — refusing to guess its state"
if [ "$match_count" -eq 0 ]; then
	echo absent
	exit 0
fi
if [ "$match_count" -gt 1 ]; then
	fail "crates.io has ${match_count} versions of ${crate} whose version core matches ${version} (differing only in build metadata) — refusing to guess which one, if any, is ${version} exactly."
fi

entry=$(printf '%s' "$matches" | jq -c '.[0]') || fail "unreadable crates.io response for ${crate} — refusing to guess its state"
num=$(printf '%s' "$entry" | jq -er '.num') || fail "unreadable version entry for ${crate} ${version} — refusing to guess its state"
if [ "$num" != "$version" ]; then
	fail "crates.io has ${crate} ${num}, whose version core matches ${version} but carries build metadata — refusing to treat that as ${version} exactly."
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
