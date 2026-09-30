#!/usr/bin/env bash
# Refuse unless commit $TARGET is on `main` (an ancestor of, or equal to, its
# tip).
#
# Environment (both required): GH_REPO, TARGET (a 40-char commit SHA); `gh` on
# PATH, authenticated.
#
# The Deploy environment's branch policy restricts which ref the workflow is
# dispatched from, not which commit it publishes; a draft release can target
# any commit a writer can push. This is what ties the published tree to
# `main`.
#
# GitHub's compare endpoint reports status relative to the head, here `main`:
# `ahead` means main contains TARGET and `identical` means main is TARGET.
# `behind` and `diverged` mean TARGET has commits main lacks. Every other
# outcome, including an API failure or a body without a status, refuses: the
# check fails closed.
set -euo pipefail

: "${GH_REPO:?}" "${TARGET:?}"

if ! [[ "$TARGET" =~ ^[0-9a-f]{40}$ ]]; then
	echo "::error::Target '$TARGET' is not a 40-char commit SHA, so it cannot be checked against main."
	exit 1
fi

if ! body=$(gh api -X GET "repos/${GH_REPO}/compare/${TARGET}...main"); then
	echo "::error::Could not compare ${TARGET} against main, so it is not known to be on main. Refusing to publish."
	exit 1
fi

if ! status=$(printf '%s' "$body" | jq -er '.status | strings'); then
	echo "::error::Could not read a status from GitHub's comparison of ${TARGET} against main. Refusing to publish."
	exit 1
fi

case "$status" in
	ahead | identical)
		echo "Commit ${TARGET} is on main (${status})."
		;;
	*)
		echo "::error::Commit ${TARGET} is not on main (comparison status: ${status}). A draft release must target a commit reachable from main; recreate the draft from main via the Draft Release workflow."
		exit 1
		;;
esac
