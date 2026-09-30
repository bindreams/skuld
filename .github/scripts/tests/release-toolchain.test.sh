#!/usr/bin/env bash
# Tests for ../release-toolchain.sh: what it accepts and prints, and that a
# malformed file cannot forge a workflow command in its error message.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
script=$here/../release-toolchain.sh
work=$(mktemp -d)
trap 'rm -rf "${work:?}"' EXIT

fail=0

# Run <name> <expected-rc> <expected-stdout> <stderr-pattern|-> <path>
# Under Actions, stderr may hold at most one workflow command: the script's own
# `::error::`. Anything else was smuggled in by the file.
run() {
	local name=$1 want_rc=$2 want_out=$3 pattern=$4 path=$5 rc=0 out n other
	out=$(GITHUB_ACTIONS=true "$script" "$path" 2>"$work/err") || rc=$?
	if [ "$rc" -ne "$want_rc" ] || [ "$out" != "$want_out" ]; then
		echo "FAIL $name: rc=$rc (want $want_rc), stdout='$out' (want '$want_out')"
		fail=1
	fi
	if [ "$pattern" != "-" ] && ! grep -q -- "$pattern" "$work/err"; then
		echo "FAIL $name: stderr lacks '$pattern':"
		cat "$work/err"
		fail=1
	fi
	grep '^::' "$work/err" > "$work/cmds" || true
	n=$(grep -c . "$work/cmds" || true)
	other=$(grep -vc '^::error::' "$work/cmds" || true)
	if [ "$n" -gt 1 ] || [ "$other" -gt 0 ]; then
		echo "FAIL $name: stderr carries a forged workflow command"
		fail=1
	fi
}

# check <name> <expected-rc> <expected-stdout> <stderr-pattern|-> <content>
# The content is written with printf %b, so escapes in it are real bytes.
check() {
	printf '%b' "$5" > "$work/pin"
	run "$1" "$2" "$3" "$4" "$work/pin"
}

# The suite runs under a UTF-8 locale.
export LC_ALL=C.UTF-8

check valid 0 1.98.1 - '1.98.1'
check zero-components 0 0.0.0 - '0.0.0'
check multi-digit 0 10.100.1000 - '10.100.1000'
check trailing-newline 0 1.98.1 - '1.98.1\n'
check several-trailing-newlines 0 1.98.1 - '1.98.1\n\n\n'
check empty 1 '' 'X.Y.Z' ''
check embedded-newline-after 1 '' 'X.Y.Z' '1.98.1\nfoo'
check embedded-newline-before 1 '' 'X.Y.Z' 'foo\n1.98.1'
check two-versions 1 '' 'X.Y.Z' '1.98.1\n1.98.2\n'
check trailing-space 1 '' 'X.Y.Z' '1.98.1 '
check leading-space 1 '' 'X.Y.Z' ' 1.98.1'
check carriage-return 1 '' 'X.Y.Z' '1.98.1\r\n'
check arabic-indic-digits 1 '' 'X.Y.Z' '\xd9\xa1.\xd9\xa9\xd9\xa8.\xd9\xa1'
check superscript-digit 1 '' 'X.Y.Z' '1.98.\xc2\xb2'
check fullwidth-digits 1 '' 'X.Y.Z' '\xef\xbc\x91.98.1'
check prerelease 1 '' 'X.Y.Z' '1.98.1-beta'
check channel 1 '' 'X.Y.Z' 'stable'
check two-components 1 '' 'X.Y.Z' '1.98'
check shell-metachar 1 '' 'X.Y.Z' '1.98.1;id'
check forged-command 1 '' 'X.Y.Z' '1\n::warning::forged'
check leading-zero-major 1 '' 'X.Y.Z' '01.98.1'
check leading-zero-minor 1 '' 'X.Y.Z' '1.098.1'
check leading-zero-patch 1 '' 'X.Y.Z' '1.98.01'
check nul-in-middle 1 '' 'NUL byte' '1.98\x00.1'
check nul-at-end 1 '' 'NUL byte' '1.98.1\x00'
check nul-only-after-newline 1 '' 'NUL byte' '1.98.1\n\x00'

# Unreadable inputs get the same annotation, not cat's raw error.
run missing-file 1 '' 'could not be read' "$work/absent"
mkdir "$work/dir"
run directory 1 '' 'could not be read' "$work/dir"
ln -s loop "$work/loop"
run symlink-loop 1 '' 'could not be read' "$work/loop"
ln -s absent "$work/dangling"
run dangling-symlink 1 '' 'could not be read' "$work/dangling"
# A symlink to an endless device must not be read to the end.
ln -s /dev/zero "$work/zero"
run symlink-to-dev-zero 1 '' 'or longer' "$work/zero"

# 63 bytes is the longest accepted file, 64 the shortest refused.
newlines=''
for _ in $(seq 57); do newlines+='\n'; done
check longest-accepted 0 1.98.1 - "1.98.1${newlines}"
check shortest-refused 1 '' 'or longer' "1.98.1${newlines}\\n"

# Needs a non-root user: root reads mode-000 files. Fail rather than skip.
uid=$(id -u)
if [ "$uid" -eq 0 ]; then
	echo "FAIL mode-000: this test must run as a non-root user"
	fail=1
else
	printf '1.98.1\n' > "$work/private"
	chmod 000 "$work/private"
	run mode-000 1 '' 'could not be read' "$work/private"
	chmod 600 "$work/private"
fi

# No FIFO test: git cannot store a FIFO, so a pin cannot be one; the only route
# is a symlink to a FIFO already on the runner, which a commit cannot create.

printf '1.98.1\n' > "$work/real"
ln -s real "$work/link"
run symlink-to-valid 0 1.98.1 - "$work/link"

rc=0
"$script" >/dev/null 2>&1 || rc=$?
[ "$rc" -eq 2 ] || { echo "FAIL no-args: rc=$rc"; fail=1; }

# Not an assertion about the script: the tree's own pin must be one it accepts.
"$here/../release-toolchain.sh" "$here/../../release-toolchain" > /dev/null || { echo "FAIL: .github/release-toolchain is not a valid pin"; fail=1; }

[ "$fail" -eq 0 ] && echo "release-toolchain tests passed"
exit "$fail"
