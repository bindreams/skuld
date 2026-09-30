#!/usr/bin/env bash
# Confirm this run's own draft release is the only release (draft or
# published) carrying its tag; delete the draft and fail if it is not.
#
# Environment (all required): GH_REPO, VERSION, RELEASE_ID; `gh` on PATH,
# authenticated. Optional, for tests: VERIFY_MAX_WAIT_SECS (default 240),
# VERIFY_INITIAL_DELAY_SECS (default 1), VERIFY_MAX_DELAY_SECS (default 16).
#
# GitHub's list-releases endpoint is eventually consistent after a create:
# seconds after the POST returned this run's draft, the listing may still
# not include it (observed: run 36648825456 counted 0 and deleted its own
# perfectly good draft as a "duplicate"). So a count taken before the
# listing shows RELEASE_ID says nothing about duplicates. This script
# re-lists, with backoff, until the listing contains RELEASE_ID — a
# deterministic condition that a lagging listing will eventually satisfy —
# and only then counts.
#
# The wait bound is a failure limit for a remote API response, not proof of
# anything: on expiry the draft is left in place (it is not known to be a
# duplicate) and the operator is told to check it by hand.
set -euo pipefail

: "${GH_REPO:?}" "${VERSION:?}" "${RELEASE_ID:?}"
max_wait=${VERIFY_MAX_WAIT_SECS:-240}
delay=${VERIFY_INITIAL_DELAY_SECS:-1}
max_delay=${VERIFY_MAX_DELAY_SECS:-16}
TAG="v${VERSION}"

started=$SECONDS
while :; do
	# `--paginate` alone does NOT merge pages into one array: each page is
	# its own top-level JSON array on its own line, and a filter run once
	# over the whole output would only see the last one. `--slurp` wraps
	# every page into a single outer array instead, so `.[][]` below
	# iterates every release across every page as one flat sequence.
	releases_json=$(gh api "repos/${GH_REPO}/releases" --paginate --slurp) || {
		echo "::error::Draft release $TAG (id $RELEASE_ID) was created, but listing releases afterward to confirm it's the only one with this tag failed. Do not assume it's fine: check https://github.com/${GH_REPO}/releases by hand, and delete this run's own draft (gh api repos/${GH_REPO}/releases/${RELEASE_ID} -X DELETE) if a duplicate turns out to exist."
		exit 1
	}

	visible=$(printf '%s' "$releases_json" | jq -er --argjson id "$RELEASE_ID" '[.[][] | select(.id == $id)] | length') || {
		echo "::error::Draft release $TAG (id $RELEASE_ID) was created, but GitHub's releases list could not be parsed to confirm it's the only one with this tag. Check https://github.com/${GH_REPO}/releases by hand."
		exit 1
	}
	[ "$visible" -eq 0 ] || break

	if [ $((SECONDS - started)) -ge "$max_wait" ]; then
		echo "::error::The releases list never showed this run's draft (id $RELEASE_ID, tag $TAG) within ${max_wait}s, so duplicates could not be counted. The draft is NOT known to be a duplicate and has been left in place. Check https://github.com/${GH_REPO}/releases by hand: if it is the only release with this tag, it is fine; if another release shares the tag, delete this run's draft (gh api repos/${GH_REPO}/releases/${RELEASE_ID} -X DELETE)."
		exit 1
	fi
	echo "Releases list does not include draft id $RELEASE_ID yet (list lagging behind the create); retrying in ${delay}s."
	sleep "$delay"
	delay=$(awk -v d="$delay" -v m="$max_delay" 'BEGIN { d *= 2; print (d > m ? m : d) }')
done

# unique_by(.id): offset pagination can list one release on two pages if the
# list shifts mid-walk, and our own id counted twice is not a duplicate.
count=$(printf '%s' "$releases_json" | jq -er --arg t "$TAG" '[.[][] | select(.tag_name == $t)] | unique_by(.id) | length') || {
	echo "::error::Draft release $TAG (id $RELEASE_ID) was created, but GitHub's releases list could not be parsed to confirm it's the only one with this tag. Check https://github.com/${GH_REPO}/releases by hand."
	exit 1
}

if [ "$count" -eq 0 ]; then
	# Own id is listed but under a different tag: nothing here is a duplicate.
	echo "::error::The releases list shows this run's draft (id $RELEASE_ID) but not under tag $TAG, so duplicates could not be counted. The draft has been left in place. Check https://github.com/${GH_REPO}/releases by hand."
	exit 1
fi

if [ "$count" -gt 1 ]; then
	echo "::error::$count releases (draft or published) now share the tag $TAG, including this run's own (id $RELEASE_ID) — GitHub does not reject a duplicate tag_name on a draft, so this can happen even though nothing here was wrong at the time it ran. Deleting this run's own draft and refusing."
	gh api "repos/${GH_REPO}/releases/${RELEASE_ID}" -X DELETE || echo "::error::Could not delete this run's own draft (id $RELEASE_ID) — delete it by hand: gh api repos/${GH_REPO}/releases/${RELEASE_ID} -X DELETE"
	echo "::error::Release $TAG already exists (draft or published). Delete it or pick a new version."
	exit 1
fi

echo "Release $TAG (id $RELEASE_ID) is the only release with this tag."
