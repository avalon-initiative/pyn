#!/usr/bin/env bash
# Checks that a release tag (v<MAJOR>.<MINOR>.<PATCH>[-label]) matches the workspace version in Cargo.toml.
set -euo pipefail

tag="${1:?usage: $0 <tag>}"
if ! [[ $tag =~ ^v([0-9]+\.[0-9]+\.[0-9]+)(-[0-9A-Za-z.-]+)?$ ]]; then
  echo "tag '$tag' is not v<MAJOR>.<MINOR>.<PATCH>[-label]" >&2
  exit 1
fi
want="${BASH_REMATCH[1]}"
have=$(sed -n 's/^version = "\(.*\)"$/\1/p' "$(dirname "$0")/../Cargo.toml")
if [ "$have" != "$want" ]; then
  echo "tag '$tag' does not match the workspace version $have" >&2
  exit 1
fi
echo "tag $tag matches workspace version $have"
