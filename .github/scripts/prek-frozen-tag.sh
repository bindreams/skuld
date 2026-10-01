#!/usr/bin/env bash
# Print the tag a prek.toml hook is frozen at, or fail.
#
#   prek-frozen-tag.sh <prek.toml> <repo-url> [--tool-version]
#
# Reads the `# frozen: <tag>` comment after the repo's `rev = "<40-hex sha>"`.
# tomllib discards the comment, so this matches the raw text. It accepts what
# the Renovate manager in .github/renovate.json accepts: comment lines may sit
# between `repo` and `rev`, and whitespace before the `#` is free. Change both
# together.
#
# With --tool-version the tag is a shellcheck-py tag, `v<tool version>.<pypi
# patch>`: print the tool version (the tag without its `v` and last segment),
# and fail unless it is X.Y.Z.
set -euo pipefail

if [ "$#" -lt 2 ] || [ "$#" -gt 3 ] || { [ "$#" -eq 3 ] && [ "$3" != --tool-version ]; }; then
	echo "usage: ${0##*/} <prek.toml> <repo-url> [--tool-version]" >&2
	exit 2
fi
file=$1 repo=$2 mode=${3:-}

tag=$(REPO=$repo perl -0777 -ne '
	my $r = quotemeta($ENV{REPO});
	if (/repo = "$r"[ \t]*\n(?:[ \t]*#[^\n]*\n)*[ \t]*rev = "[0-9a-f]{40}"[ \t]*# frozen: (\S+)/) { print $1; }
' "$file")

if [ -z "$tag" ]; then
	echo "::error::No 'rev = \"<sha>\" # frozen: <tag>' for $(printf '%q' "$repo") in $(printf '%q' "$file")." >&2
	exit 1
fi

if [ "$mode" != --tool-version ]; then
	printf '%s\n' "$tag"
	exit 0
fi

version=${tag#v}
version=${version%.*}
if ! [[ $version =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]]; then
	echo "::error::Tag $(printf '%q' "$tag") does not have the form v<X.Y.Z>.<patch>." >&2
	exit 1
fi
printf '%s\n' "$version"
