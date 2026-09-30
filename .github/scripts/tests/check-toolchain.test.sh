#!/usr/bin/env bash
# Tests for ../check-toolchain.sh against a stub `cargo`.
#
# The stub reports what rustup would: RUSTUP_TOOLCHAIN when set (it outranks
# toolchain files), otherwise 9.9.9, standing for a `rust-toolchain.toml` in
# the directory cargo runs from. Real rustup is not used because it would need
# the toolchains installed, i.e. network.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
script=$here/../check-toolchain.sh
work=$(mktemp -d)
trap 'rm -rf "${work:?}"' EXIT

mkdir "$work/bin" "$work/tree"
cat > "$work/bin/cargo" <<'STUB'
#!/bin/sh
echo "cargo ${RUSTUP_TOOLCHAIN:-9.9.9} (stub)"
STUB
chmod +x "$work/bin/cargo"
printf '[toolchain]\nchannel = "9.9.9"\n' > "$work/tree/rust-toolchain.toml"

fail=0
# check <name> <expected-rc> <expected-GITHUB_ENV-content> <arg>
# Runs from a directory whose toolchain file disagrees with the request.
check() {
	local name=$1 want_rc=$2 want_env=$3 arg=$4 rc=0 got_env
	: > "$work/env"
	(cd "$work/tree" && env -u RUSTUP_TOOLCHAIN PATH="$work/bin:$PATH" GITHUB_ENV="$work/env" "$script" "$arg") > "$work/out" 2>&1 || rc=$?
	got_env=$(cat "$work/env")
	if [ "$rc" -ne "$want_rc" ] || [ "$got_env" != "$want_env" ]; then
		echo "FAIL $name: rc=$rc (want $want_rc), GITHUB_ENV='$got_env' (want '$want_env')"
		cat "$work/out"
		fail=1
	fi
}

check exact-beats-toolchain-file 0 RUSTUP_TOOLCHAIN=1.98.1 1.98.1
check stable-changes-nothing 0 '' stable

# A cargo that ignores the pin (e.g. the install failed) is refused.
cat > "$work/bin/cargo" <<'STUB'
#!/bin/sh
echo "cargo 1.97.0 (stub)"
STUB
check wrong-cargo 1 RUSTUP_TOOLCHAIN=1.98.1 1.98.1
grep -q 'Requested toolchain 1.98.1 but cargo 1.97.0' "$work/out" || { echo "FAIL wrong-cargo: message"; cat "$work/out"; fail=1; }

rc=0
"$script" >/dev/null 2>&1 || rc=$?
[ "$rc" -eq 2 ] || { echo "FAIL no-args: rc=$rc"; fail=1; }

[ "$fail" -eq 0 ] && echo "check-toolchain tests passed"
exit "$fail"
