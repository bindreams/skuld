#!/usr/bin/env bash
# Refuse unless commit $TARGET is on `main` and not older than FLOOR: it must
# descend from (or be) FLOOR and be an ancestor of (or be) the tip of
# `refs/heads/main`.
#
# Environment (both required): GH_REPO, TARGET; `gh` on PATH, authenticated.
#
# The trust this gives, and what it rests on, is stated once, in the header of
# publish-release.yaml.
#
# FLOOR is the first commit after which every commit on `main` entered through
# the default-branch ruleset (linear history, squash-only merges, required CI).
# Earlier history holds direct pushes and a merge commit. Raise it only to a
# later commit on `main`, never lower it.
#
# Compare status is relative to the head, so `ahead` means the head contains
# the base. Only `ahead` and `identical` pass. The head of the main comparison
# is the full ref, since a bare `main` may resolve to a tag of that name, which
# any writer can push. A failed `gh` refuses whatever it printed, and `gh`
# itself logs the HTTP status, which is how to tell an outage (5xx, 403/429)
# from a target that is not a comparable commit here (404/422).
set -euo pipefail

: "${GH_REPO:?}" "${TARGET:?}"

FLOOR=0060c68d928654ef029ed0680ec5d062094805ec

if ! [[ "$TARGET" =~ ^[0-9a-f]{40}$ ]]; then
	echo "::error::Target '$TARGET' is not a 40-char commit SHA, so it cannot be checked against main."
	exit 1
fi

# compare_status <label> <base> <head>: set `status` to the comparison's status,
# or refuse. Not run in a subshell, so the refusal's message reaches the log.
compare_status() {
	local label=$1 base=$2 head=$3 body
	if ! body=$(gh api -X GET "repos/${GH_REPO}/compare/${base}...${head}?per_page=1"); then
		echo "::error::Could not compare ${label} (${base}...${head}), so ${TARGET} is not known to be on main. Refusing to publish."
		exit 1
	fi
	if ! status=$(printf '%s' "$body" | jq -er '.status | strings'); then
		echo "::error::Could not read a status from GitHub's comparison of ${label} (${base}...${head}). Refusing to publish."
		exit 1
	fi
}

compare_status "the floor" "$FLOOR" "$TARGET"
case "$status" in
	ahead | identical) ;;
	*)
		echo "::error::Commit ${TARGET} is not after the floor ${FLOOR} (comparison status: ${status}). A draft release must target a commit on main from the floor onward; recreate the draft from main via the Draft Release workflow."
		exit 1
		;;
esac

compare_status "main" "$TARGET" "refs/heads/main"
case "$status" in
	ahead | identical)
		echo "Commit ${TARGET} is on main (${status})."
		;;
	*)
		echo "::error::Commit ${TARGET} is not on main (comparison status: ${status}). A draft release must target a commit reachable from main; recreate the draft from main via the Draft Release workflow."
		exit 1
		;;
esac
