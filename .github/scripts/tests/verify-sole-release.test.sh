#!/usr/bin/env bash
# Tests for ../verify-sole-release.sh against a stubbed `gh`.
#
# The stub serves the list endpoint from $STUB_DIR/releases.json, but hides
# the release with id $RELEASE_ID for the first $STUB_LAG list calls (and
# forever if STUB_LAG=inf). DELETE calls are recorded in $STUB_DIR/deleted.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
script=$here/../verify-sole-release.sh
work=$(mktemp -d)
trap 'rm -rf "${work:?}"' EXIT

mkdir "$work/bin"
cat > "$work/bin/gh" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
if [[ " $* " == *" DELETE "* ]]; then
	echo "$*" >> "$STUB_DIR/deleted"
	exit 0
fi
calls=$(($(cat "$STUB_DIR/calls" 2>/dev/null || echo 0) + 1))
echo "$calls" > "$STUB_DIR/calls"
if [ "$STUB_LAG" = inf ] || [ "$calls" -le "$STUB_LAG" ]; then
	jq -c --argjson id "$RELEASE_ID" '[[.[] | select(.id != $id)]]' "$STUB_DIR/releases.json"
else
	jq -c '[.]' "$STUB_DIR/releases.json"
fi
STUB
chmod +x "$work/bin/gh"

fail=0
# run_case <name> <expected-rc> <lag> <releases-json> <expect-deleted yes|no> <stdout-pattern>
run_case() {
	local name=$1 want_rc=$2 lag=$3 releases=$4 want_del=$5 pattern=$6
	local dir=$work/$name rc=0 out
	mkdir "$dir"
	printf '%s' "$releases" > "$dir/releases.json"
	out=$(PATH="$work/bin:$PATH" STUB_DIR="$dir" STUB_LAG="$lag" \
		GH_REPO=o/r VERSION=1.2.3 RELEASE_ID=100 \
		VERIFY_MAX_WAIT_SECS=2 VERIFY_INITIAL_DELAY_SECS=0.05 VERIFY_MAX_DELAY_SECS=0.1 \
		"$script" 2>&1) || rc=$?
	local deleted=no
	[ ! -s "$dir/deleted" ] || deleted=yes
	if [ "$rc" -ne "$want_rc" ] || [ "$deleted" != "$want_del" ] || ! grep -q -- "$pattern" <<<"$out"; then
		echo "FAIL $name: rc=$rc (want $want_rc) deleted=$deleted (want $want_del) pattern=/$pattern/"
		echo "$out"
		fail=1
	else
		echo "ok   $name"
	fi
}

own='{"id":100,"tag_name":"v1.2.3"}'
other='{"id":7,"tag_name":"v1.0.0"}'

run_case immediate 0 0 "[$other,$own]" no "is the only release"
run_case lag_then_visible 0 3 "[$other,$own]" no "is the only release"
run_case never_visible 1 inf "[$other,$own]" no "never showed this run's draft (id 100"
run_case real_duplicate 1 0 "[$other,$own,{\"id\":9,\"tag_name\":\"v1.2.3\"}]" yes "already exists"
# A lagging list must not be mistaken for a duplicate, nor a duplicate
# announced before our own draft is visible.
run_case lag_then_duplicate 1 2 "[$own,{\"id\":9,\"tag_name\":\"v1.2.3\"}]" yes "2 releases"

exit "$fail"
