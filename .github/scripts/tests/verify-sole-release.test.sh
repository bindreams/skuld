#!/usr/bin/env bash
# Tests for ../verify-sole-release.sh against a stubbed `gh`.
#
# The stub serves the list endpoint from $STUB_DIR/releases.json, but hides
# the release with id $RELEASE_ID for the first $STUB_LAG list calls (forever
# if STUB_LAG=inf). Its DELETE calls are recorded in $STUB_DIR/deleted.
#
# The whole suite runs twice, the second time with a `jq` shim that adds
# latency to every call, so no case may depend on runner speed. Cases that
# must not hit the wait bound get one they cannot reach; only the
# never-visible cases use a small bound, because expiry is what they test.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
script=$here/../verify-sole-release.sh
work=$(mktemp -d)
trap 'rm -rf "${work:?}"' EXIT

mkdir "$work/bin" "$work/slowbin"
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
real_jq=$(command -v jq)
cat > "$work/slowbin/jq" <<STUB
#!/usr/bin/env bash
sleep 0.3
exec "$real_jq" "\$@"
STUB
chmod +x "$work/bin/gh" "$work/slowbin/jq"

fail=0
suite=0
own_delete='api repos/o/r/releases/100 -X DELETE'
# run_case <name> <expected-rc> <lag> <releases-json> <expected-deleted-line|-> <stdout-pattern> <max-wait>
run_case() {
	local name=$1 want_rc=$2 lag=$3 releases=$4 want_del=$5 pattern=$6 wait=$7
	local dir=$work/$suite-$name rc=0 out path=$work/bin:$PATH
	[ "$suite" -eq 0 ] || path=$work/slowbin:$path
	mkdir "$dir"
	printf '%s' "$releases" > "$dir/releases.json"
	out=$(PATH="$path" STUB_DIR="$dir" STUB_LAG="$lag" \
		GH_REPO=o/r VERSION=1.2.3 RELEASE_ID=100 \
		VERIFY_MAX_WAIT_SECS="$wait" VERIFY_INITIAL_DELAY_SECS=0.05 VERIFY_MAX_DELAY_SECS=0.1 \
		"$script" 2>&1) || rc=$?
	local deleted=-
	[ ! -e "$dir/deleted" ] || deleted=$(cat "$dir/deleted")
	if [ "$rc" -ne "$want_rc" ] || [ "$deleted" != "$want_del" ] || ! grep -q -- "$pattern" <<<"$out"; then
		echo "FAIL [suite $suite] $name: rc=$rc (want $want_rc) deleted=[$deleted] (want [$want_del]) pattern=/$pattern/"
		echo "$out"
		fail=1
	else
		echo "ok   [suite $suite] $name"
	fi
}

own='{"id":100,"tag_name":"v1.2.3"}'
other='{"id":7,"tag_name":"v1.0.0"}'
dup='{"id":9,"tag_name":"v1.2.3"}'
sole='is the only release'

for suite in 0 1; do
	run_case immediate 0 0 "[$other,$own]" - "$sole" 600
	run_case lag_then_visible 0 3 "[$other,$own]" - "$sole" 600
	# Offset pagination can list our own id on two pages: still not a duplicate.
	run_case own_listed_twice 0 0 "[$own,$other,$own]" - "$sole" 600
	run_case never_visible 1 inf "[$other,$own]" - "never showed this run's draft (id 100" 2
	# Expiry proves nothing: another release with our tag must not cause a
	# delete or a "duplicate" verdict when our own draft was never seen.
	run_case never_visible_other_same_tag 1 inf "[$other,$own,$dup]" - "never showed this run's draft (id 100" 2
	run_case own_under_other_tag 1 0 "[$other,{\"id\":100,\"tag_name\":\"v9.9.9\"}]" - "not under tag v1.2.3" 600
	# Only our own id is ever deleted.
	run_case real_duplicate 1 0 "[$other,$own,$dup]" "$own_delete" "already exists" 600
	run_case lag_then_duplicate 1 2 "[$own,$dup]" "$own_delete" "2 releases" 600
	run_case two_duplicates 1 0 "[$dup,$own,{\"id\":11,\"tag_name\":\"v1.2.3\"}]" "$own_delete" "3 releases" 600
done

exit "$fail"
