#!/usr/bin/env bash
# Tests for ../release-toolchain.sh: what it accepts and prints, and that a
# malformed file cannot forge a workflow command in its error message.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
script=$here/../release-toolchain.sh
work=$(mktemp -d)
trap 'rm -rf "${work:?}"' EXIT

fail=0
# check <name> <expected-rc> <expected-stdout> <printf-format-of-file|->
# The file is written with printf, so escapes in the format are real bytes.
# `-` means the file does not exist.
check() {
	local name=$1 want_rc=$2 want_out=$3 content=$4
	local f=$work/pin rc=0 out
	rm -f "$f"
	if [ "$content" != "-" ]; then
		printf '%b' "$content" > "$f"
	fi
	out=$(GITHUB_ACTIONS=true "$script" "$f" 2>"$work/err") || rc=$?
	if [ "$rc" -ne "$want_rc" ] || [ "$out" != "$want_out" ]; then
		echo "FAIL $name: rc=$rc (want $want_rc), stdout='$out' (want '$want_out')"
		fail=1
	fi
	# Under Actions, stderr may hold at most one workflow command: the
	# script's own `::error::`. Anything else was smuggled in by the file.
	grep '^::' "$work/err" > "$work/cmds" || true
	local n other
	n=$(grep -c . "$work/cmds" || true)
	other=$(grep -vc '^::error::' "$work/cmds" || true)
	if [ "$n" -gt 1 ] || [ "$other" -gt 0 ]; then
		echo "FAIL $name: stderr carries a forged workflow command"
		fail=1
	fi
}

# Run the suite under a UTF-8 locale: the script must not let it widen [0-9].
export LC_ALL=C.UTF-8

check valid 0 1.98.1 '1.98.1'
check trailing-newline 0 1.98.1 '1.98.1\n'
check several-trailing-newlines 0 1.98.1 '1.98.1\n\n\n'
check missing-file 1 '' '-'
check empty 1 '' ''
check embedded-newline-after 1 '' '1.98.1\nfoo'
check embedded-newline-before 1 '' 'foo\n1.98.1'
check two-versions 1 '' '1.98.1\n1.98.2\n'
check trailing-space 1 '' '1.98.1 '
check leading-space 1 '' ' 1.98.1'
check carriage-return 1 '' '1.98.1\r\n'
check arabic-indic-digits 1 '' '\xd9\xa1.\xd9\xa9\xd9\xa8.\xd9\xa1'
check superscript-digit 1 '' '1.98.\xc2\xb2'
check fullwidth-digits 1 '' '\xef\xbc\x91.98.1'
check prerelease 1 '' '1.98.1-beta'
check channel 1 '' 'stable'
check two-components 1 '' '1.98'
check shell-metachar 1 '' '1.98.1;id'
check forged-command 1 '' '1\n::warning::forged'

# A directory is not a pin.
mkdir "$work/dir"
rc=0
"$script" "$work/dir" >/dev/null 2>&1 || rc=$?
[ "$rc" -eq 1 ] || { echo "FAIL directory: rc=$rc"; fail=1; }

# Usage errors.
rc=0
"$script" >/dev/null 2>&1 || rc=$?
[ "$rc" -eq 2 ] || { echo "FAIL no-args: rc=$rc"; fail=1; }

# The real pin in this tree must itself be accepted.
"$script" "$here/../../release-toolchain" >/dev/null || { echo "FAIL: .github/release-toolchain is not a valid pin"; fail=1; }

[ "$fail" -eq 0 ] && echo "release-toolchain tests passed"
exit "$fail"
