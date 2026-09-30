#!/usr/bin/env bash
# Tests for ../require-target-on-main.sh against a stubbed `gh`.
#
# The script makes two compare calls: FLOOR...TARGET and TARGET...refs/heads/main.
# The stub answers each from its own file ($STUB_DIR/floor, $STUB_DIR/main),
# appends every call's arguments to $STUB_DIR/args, and, for a call named in
# $STUB_FAIL_ON (floor, main or both), prints the response on stdout and exits
# 1, as real `gh api` does on an HTTP error.
#
# `env -u BASH_ENV` keeps a login profile from reordering PATH ahead of the stub.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
script=$here/../require-target-on-main.sh
work=$(mktemp -d)
trap 'rm -rf "${work:?}"' EXIT

mkdir "$work/bin"
cat > "$work/bin/gh" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
echo "$*" >> "$STUB_DIR/args"
if [[ "$*" == *"...refs/heads/main"* ]]; then which=main; else which=floor; fi
cat "$STUB_DIR/$which"
if [[ " ${STUB_FAIL_ON:-} " == *" $which "* ]]; then
	exit 1
fi
STUB
chmod +x "$work/bin/gh"

fail=0
floor=0060c68d928654ef029ed0680ec5d062094805ec
target=0123456789abcdef0123456789abcdef01234567
ok_body='{"status":"ahead"}'
same='{"status":"identical"}'

# run_case <name> <expected-rc> <floor-response> <main-response> <fail-on> <pattern> [target]
# `fail-on` is a space-separated subset of "floor main", or "-" for none.
run_case() {
	local name=$1 want_rc=$2 floor_resp=$3 main_resp=$4 fail_on=$5 pattern=$6 tgt=${7-$target}
	local dir=$work/$name rc=0 out
	mkdir "$dir"
	printf '%s' "$floor_resp" > "$dir/floor"
	printf '%s' "$main_resp" > "$dir/main"
	[ "$fail_on" != - ] || fail_on=
	out=$(env -u BASH_ENV PATH="$work/bin:$PATH" STUB_DIR="$dir" STUB_FAIL_ON="$fail_on" \
		GH_REPO=o/r TARGET="$tgt" "$script" 2>&1) || rc=$?
	if [ "$rc" -ne "$want_rc" ] || ! grep -qi -- "$pattern" <<<"$out"; then
		echo "FAIL $name: rc=$rc (want $want_rc) pattern=/$pattern/"
		echo "$out"
		fail=1
	else
		echo "ok   $name"
	fi
}

# assert <name> <condition-description> <command...>: records a failure if the command fails.
assert() {
	local name=$1 what=$2
	shift 2
	if "$@"; then
		echo "ok   $name"
	else
		echo "FAIL $name: $what"
		fail=1
	fi
}
# shellcheck disable=SC2329 # run indirectly, by `assert`
stub_never_called() { [ ! -e "$work/$1/args" ]; }

# Accepted: ahead of the floor (or the floor itself), and on main.
run_case ahead 0 "$ok_body" "$ok_body" - 'is on main'
run_case floor_itself 0 "$same" "$ok_body" - 'is on main'
run_case main_tip 0 "$ok_body" "$same" - 'is on main'

# Refused: not descended from the floor.
run_case pre_floor 1 '{"status":"behind"}' "$ok_body" - 'not after the floor.*behind'
run_case floor_diverged 1 '{"status":"diverged"}' "$ok_body" - 'not after the floor.*diverged'

# Refused: not on main.
run_case behind 1 "$ok_body" '{"status":"behind"}' - 'not on main.*behind'
run_case diverged 1 "$ok_body" '{"status":"diverged"}' - 'not on main.*diverged'
run_case unknown_status 1 "$ok_body" '{"status":"sideways"}' - 'not on main.*sideways'

# Refused: an API failure, even one whose body says `ahead`, on either call.
run_case api_error_main 1 "$ok_body" 'gh: Not Found' main 'could not compare main'
run_case api_error_floor 1 'gh: Not Found' "$ok_body" floor 'could not compare the floor'
run_case fails_with_ahead_main 1 "$ok_body" "$ok_body" main 'could not compare'
run_case fails_with_ahead_floor 1 "$ok_body" "$ok_body" floor 'could not compare'
run_case fails_with_ahead_both 1 "$ok_body" "$ok_body" 'floor main' 'could not compare'

# Refused: an unreadable body, on either call.
for which in floor main; do
	for spec in 'empty|' 'not_json|oops' 'no_status|{"message":"x"}' 'null_status|{"status":null}' 'number_status|{"status":5}'; do
		bad=${spec#*|}
		if [ "$which" = floor ]; then
			run_case "${which}_${spec%%|*}" 1 "$bad" "$ok_body" - 'could not read'
		else
			run_case "${which}_${spec%%|*}" 1 "$ok_body" "$bad" - 'could not read'
		fi
	done
done

# Refused before any API call: anything but 40 lowercase hex characters.
for spec in 'upper|0123456789ABCDEF0123456789ABCDEF01234567' 'short|0123456789abcdef0123456789abcdef0123456' \
	'long|0123456789abcdef0123456789abcdef012345678' 'ref_name|main' 'ref_path|refs/heads/x' \
	'newline|0123456789abcdef0123456789abcdef01234567'$'\n'; do
	n=sha_${spec%%|*}
	run_case "$n" 1 "$ok_body" "$ok_body" - '40-char' "${spec#*|}"
	assert "$n-no-call" 'gh was called' stub_never_called "$n"
done
run_case sha_empty 1 "$ok_body" "$ok_body" - 'TARGET' ''
assert sha_empty-no-call 'gh was called' stub_never_called sha_empty

# The requests. Both are explicit GETs. The floor call has the floor as base
# and TARGET as head. The main call has TARGET as base and exactly
# `refs/heads/main` as head, since a bare `main` could resolve to a tag.
main_call="^api -X GET repos/o/r/compare/$target\.\.\.refs/heads/main\?per_page=1$"
floor_call="^api -X GET repos/o/r/compare/$floor\.\.\.$target\?per_page=1$"
# shellcheck disable=SC2329 # run indirectly, by `assert`
args_have() { grep -Eq -- "$2" "$work/$1/args"; }
# shellcheck disable=SC2329
args_lack() { ! args_have "$@"; }
# shellcheck disable=SC2329
count_calls() {
	local n
	n=$(grep -c "" "$work/$1/args")
	[ "$n" = "$2" ]
}
for c in ahead floor_itself main_tip; do
	assert "$c-requests" 'want one floor call and one main call' count_calls "$c" 2
	assert "$c-floor-request" "want /$floor_call/" args_have "$c" "$floor_call"
	assert "$c-main-request" "want /$main_call/" args_have "$c" "$main_call"
	assert "$c-no-bare-main" 'a call ends in a bare ...main' args_lack "$c" '\.\.\.main($|\?)'
done
# A failing floor call refuses without asking about main.
assert pre_floor-no-main-call 'main was compared after a floor refusal' args_lack pre_floor 'refs/heads/main'

exit "$fail"
