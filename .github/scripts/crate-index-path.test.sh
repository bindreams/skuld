#!/usr/bin/env bash
# Assertions for crate-index-path.sh's case-on-name-length encoding. Runs as a
# CI lint step (see ci.yaml) rather than under `cargo test`: this is workflow
# tooling, not Rust, and the failure mode it guards against (a wrong sparse-
# index path) only ever surfaces during an irreversible publish or a stressed
# manual recovery — see crate-index-path.sh's own header comment.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")"

check() {
	local name="$1" want="$2" got
	got=$(./crate-index-path.sh "$name")
	if [ "$got" != "$want" ]; then
		echo "crate-index-path.sh $name: want '$want', got '$got'" >&2
		exit 1
	fi
}

# Examples from the script's own header comment.
check skuld sk/ul/skuld
check log 3/l/log
check os 2/os
check a 1/a

# 4-char boundary: the shortest name that takes the general (2/2) case.
check ruby ru/by/ruby

echo "crate-index-path.sh: all cases OK"
