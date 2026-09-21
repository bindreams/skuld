#!/usr/bin/env bash
# Report a crate's state on crates.io. With a version: absent | yanked |
# published. Without: never-published | exists.
#
# Uses the JSON API rather than the sparse index for two reasons: the index is
# CDN-cached for 600s and these checks run seconds after a publish, and the
# index path encodes the name length (1/, 2/, 3/n[0]/, n[0:2]/n[2:4]/), which
# is easy to get wrong for short names. The API needs a User-Agent — without
# one it answers 403.
#
# Any answer other than 200 or 404 is a refusal, not a guess: a dropped
# connection must never read as "absent", because the caller's recovery path
# for that is `cargo yank`, which permanently consumes the version slot.
set -euo pipefail

crate=${1:?usage: crate-state.sh <crate> [version]}
version=${2-}
ua="skuld-release-pipeline (https://github.com/bindreams/skuld)"
url="https://crates.io/api/v1/crates/${crate}${version:+/$version}"

resp=$(curl -sL -A "$ua" -w $'\n%{http_code}' --retry 3 --retry-all-errors --max-time 30 "$url") \
  || resp=$'\n000'
code=${resp##*$'\n'}
body=${resp%$'\n'*}

case "$code" in
  404) [ -n "$version" ] && echo absent || echo never-published ;;
  200)
    if [ -z "$version" ]; then
      echo exists
    elif [ "$(printf '%s' "$body" | jq -r '.version.yanked')" = "true" ]; then
      # 200 does not mean usable: crates.io serves yanked versions too, and a
      # yanked version's slot is spent forever.
      echo yanked
    else
      echo published
    fi
    ;;
  *)
    echo "::error::Unexpected HTTP $code from crates.io for ${crate} ${version:-}" >&2
    echo "::error::Cannot determine its state; refusing to continue." >&2
    exit 1
    ;;
esac
