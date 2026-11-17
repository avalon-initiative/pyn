#!/usr/bin/env bash
# Prints the `gh release create` flag for a tag: --latest when its X.Y.Z is the highest among the existing release tags, else
# --latest=false (a backport). The title slug is ignored. Needs GITHUB_REPOSITORY and gh authentication.
set -euo pipefail

tag="${1:?usage: $0 <tag>}"
ver_of() { sed -nE 's/^v([0-9]+\.[0-9]+\.[0-9]+)(-.*)?$/\1/p'; }
this=$(ver_of <<<"$tag")
[ -n "$this" ] || { echo "release-latest: '$tag' is not v<MAJOR>.<MINOR>.<PATCH>[-title]" >&2; exit 1; }
tags=$(gh api "repos/${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is not set}/releases" --paginate --jq '.[].tag_name')
top=$({ ver_of <<<"$tags"; echo "$this"; } | sort -V | tail -n 1)
if [ "$top" = "$this" ]; then echo --latest; else echo --latest=false; fi
