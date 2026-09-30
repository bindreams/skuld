#!/usr/bin/env bash
# Fail unless the toolchain asked for is the one cargo runs.
#
# dtolnay/rust-toolchain's `rustup default` step tolerates failure, which would
# silently leave the runner's preinstalled toolchain in place. For an exact
# X.Y.Z this also exports RUSTUP_TOOLCHAIN, here and for every later step
# (through $GITHUB_ENV): it outranks any `rust-toolchain(.toml)` in whatever
# directory cargo runs from, `path =` toolchains included, so the check holds
# wherever a later step runs cargo.
#
# Validation of the argument's shape is the caller's job
# (.github/actions/install-toolchain), before anything is installed.
set -euo pipefail

if [ "$#" -ne 1 ]; then
	echo "usage: ${0##*/} <stable|X.Y.Z>" >&2
	exit 2
fi

requested="$1"
if [ "$requested" = stable ]; then
	exit 0
fi

export RUSTUP_TOOLCHAIN="$requested"
echo "RUSTUP_TOOLCHAIN=$requested" >> "${GITHUB_ENV:?}"

actual=$(cargo --version | cut -d ' ' -f2)
if [ "$actual" != "$requested" ]; then
	echo "::error::Requested toolchain ${requested} but cargo ${actual} is running."
	exit 1
fi
