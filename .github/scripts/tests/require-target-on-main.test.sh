#!/usr/bin/env bash
# Tests for ../require-target-on-main.sh against a stubbed `gh`.
#
# The stub serves the compare endpoint from $STUB_DIR/response (or exits 1
# with $STUB_DIR/response as its stderr when $STUB_FAIL is set) and records
# its arguments in $STUB_DIR/args, so each case also pins how the script asks.
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
echo "$*" > "$STUB_DIR/args"
if [ -n "${STUB_FAIL:-}" ]; then
	cat "$STUB_DIR/response" >&2
	exit 1
fi
cat "$STUB_DIR/response"
STUB
chmod +x "$work/bin/gh"

fail=0
target=0123456789abcdef0123456789abcdef01234567
# run_case <name> <expected-rc> <response> <stub-fails:0|1> <stdout-pattern> [target]
run_case() {
	local name=$1 want_rc=$2 response=$3 stub_fail=$4 pattern=$5 tgt=${6:-$target}
	local dir=$work/$name rc=0 out
	mkdir "$dir"
	printf '%s' "$response" > "$dir/response"
	out=$(env -u BASH_ENV PATH="$work/bin:$PATH" STUB_DIR="$dir" STUB_FAIL="$([ "$stub_fail" = 1 ] && echo 1 || true)" \
		GH_REPO=o/r TARGET="$tgt" "$script" 2>&1) || rc=$?
	if [ "$rc" -ne "$want_rc" ] || ! grep -qi -- "$pattern" <<<"$out"; then
		echo "FAIL $name: rc=$rc (want $want_rc) pattern=/$pattern/"
		echo "$out"
		fail=1
	else
		echo "ok   $name"
	fi
}

# GitHub's compare status is relative to head (`refs/heads/main`): `ahead` means main
# contains the target.
run_case ahead 0 '{"status":"ahead","ahead_by":3}' 0 'is on main'
run_case identical 0 '{"status":"identical"}' 0 'is on main'
run_case behind 1 '{"status":"behind","behind_by":2}' 0 '::error::.*not on main.*behind'
run_case diverged 1 '{"status":"diverged"}' 0 '::error::.*not on main.*diverged'
run_case api_error 1 'gh: Not Found (HTTP 404)' 1 '::error::.*could not compare'
run_case empty_body 1 '' 0 '::error::.*could not read'
run_case not_json 1 'oops' 0 '::error::.*could not read'
run_case no_status 1 '{"message":"x"}' 0 '::error::.*could not read'
run_case null_status 1 '{"status":null}' 0 '::error::.*could not read'
run_case number_status 1 '{"status":5}' 0 '::error::.*could not read'
run_case unknown_status 1 '{"status":"sideways"}' 0 '::error::.*not on main.*sideways'
run_case non_sha_target 1 '{"status":"ahead"}' 0 '::error::.*40-char' main

# The request must be an explicit GET, base target, head `refs/heads/main`: a
# bare `main` could resolve to a tag of that name.
got=$(cat "$work/ahead/args")
want="api -X GET repos/o/r/compare/$target...refs/heads/main?per_page=1"
if [ "$got" != "$want" ]; then
	echo "FAIL request: got [$got] want [$want]"
	fail=1
else
	echo "ok   request"
fi

exit "$fail"
