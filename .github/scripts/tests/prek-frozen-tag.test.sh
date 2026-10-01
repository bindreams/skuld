#!/usr/bin/env bash
# Tests for ../prek-frozen-tag.sh: the frozen-tag lookup, with the layouts the
# Renovate manager also accepts, and the shellcheck version derived from it.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
script=$here/../prek-frozen-tag.sh
work=$(mktemp -d)
trap 'rm -rf "${work:?}"' EXIT

fail=0
sha=745eface02aef23e168a8afb6b5737818efbea95
repo=https://github.com/shellcheck-py/shellcheck-py

# check <name> <expected-rc> <expected-stdout> <toml-content> [flag]
check() {
	local name=$1 want_rc=$2 want_out=$3 content=$4 flag=${5-} rc=0 out
	printf '%s' "$content" > "$work/prek.toml"
	out=$("$script" "$work/prek.toml" "$repo" ${flag:+"$flag"} 2>/dev/null) || rc=$?
	if [ "$rc" -ne "$want_rc" ] || [ "$out" != "$want_out" ]; then
		echo "FAIL $name: rc=$rc (want $want_rc), stdout='$out' (want '$want_out')"
		fail=1
	fi
}

plain="[[repos]]
repo = \"$repo\"
rev = \"$sha\" # frozen: v0.11.0.1
hooks = [{ id = \"shellcheck\" }]
"
check normal 0 v0.11.0.1 "$plain"
check normal-tool-version 0 0.11.0 "$plain" --tool-version

check comment-between 0 v0.11.0.1 "[[repos]]
repo = \"$repo\"
# a note about this hook
  # and another
rev = \"$sha\" # frozen: v0.11.0.1
"

check wide-spacing 0 v0.11.0.1 "[[repos]]
repo = \"$repo\"
rev = \"$sha\"     # frozen: v0.11.0.1
"

# The wanted repo is the one matched, not the first block.
check second-block 0 v0.11.0.1 "[[repos]]
repo = \"https://github.com/rhysd/actionlint\"
rev = \"914e7df21a07ef503a81201c76d2b11c789d3fca\" # frozen: v1.7.12

$plain"

check missing-frozen-comment 1 '' "[[repos]]
repo = \"$repo\"
rev = \"$sha\"
"
check tag-rev-not-sha 1 '' "[[repos]]
repo = \"$repo\"
rev = \"v0.11.0.1\"
"
check other-repo-only 1 '' "[[repos]]
repo = \"https://github.com/rhysd/actionlint\"
rev = \"914e7df21a07ef503a81201c76d2b11c789d3fca\" # frozen: v1.7.12
"

# Malformed tags: the lookup prints them, the tool version refuses them.
check short-tag-lookup 0 v1 "[[repos]]
repo = \"$repo\"
rev = \"$sha\" # frozen: v1
"
check short-tag-version 1 '' "[[repos]]
repo = \"$repo\"
rev = \"$sha\" # frozen: v1
" --tool-version
check three-segment-tag-version 1 '' "[[repos]]
repo = \"$repo\"
rev = \"$sha\" # frozen: v0.11.0
" --tool-version
check non-numeric-tag-version 1 '' "[[repos]]
repo = \"$repo\"
rev = \"$sha\" # frozen: v0.11.x.1
" --tool-version
check leading-zero-tag-version 1 '' "[[repos]]
repo = \"$repo\"
rev = \"$sha\" # frozen: v0.011.0.1
" --tool-version

rc=0
"$script" >/dev/null 2>&1 || rc=$?
[ "$rc" -eq 2 ] || { echo "FAIL no-args: rc=$rc"; fail=1; }

# Not an assertion about the script: the tree's own prek.toml must be one it reads.
"$script" "$here/../../../prek.toml" "$repo" --tool-version > /dev/null || { echo "FAIL: prek.toml has no readable shellcheck-py pin"; fail=1; }

[ "$fail" -eq 0 ] && echo "prek-frozen-tag tests passed"
exit "$fail"
