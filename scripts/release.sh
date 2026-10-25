#!/usr/bin/env bash
# Runs the pre-release checks, prompts for the next version, bumps, commits and tags.
set -euo pipefail

make check

if [ "$(grep -c '^version = ' Cargo.toml)" -ne 1 ]; then
  echo "expected exactly one 'version = ' line in Cargo.toml" >&2
  exit 1
fi
current=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml)
if ! [[ $current =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "cannot parse the workspace version '$current' from Cargo.toml" >&2
  exit 1
fi
echo "current version: $current"

read -r -p "next semantic version: " version || { echo "no version entered" >&2; exit 1; }
if ! [[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "invalid version '$version': expected three dot-separated numbers, e.g. 0.1.0" >&2
  exit 1
fi

if [ "$version" = "$current" ]; then
  echo "version '$version' is already the current version" >&2
  exit 1
fi

read -r -p "optional label, empty for none (e.g. alpha): " label || { echo "no label entered" >&2; exit 1; }
if ! [[ $label =~ ^[0-9A-Za-z.-]*$ ]]; then
  echo "invalid label '$label': use letters, digits, dots or dashes" >&2
  exit 1
fi

tag=$version
if [ -n "$label" ]; then
  tag="$version-$label"
fi

if ! git check-ref-format --allow-onelevel "$tag"; then
  echo "invalid tag name '$tag'" >&2
  exit 1
fi

if git rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
  echo "tag '$tag' already exists" >&2
  exit 1
fi

# The members pin their intra-workspace dependencies at the workspace version,
# so the bump moves every intra-workspace requirement with the root version.
sed -i "s/^version = \"$current\"/version = \"$version\"/" Cargo.toml
sed -i "s/^\\(pyn-[a-z-]* = { version = \"\\)$current/\\1$version/" crates/*/Cargo.toml

# The lock pins the member versions, so metadata re-resolves and rewrites it.
if [ -f Cargo.lock ]; then
  cargo metadata > /dev/null
  git add Cargo.toml Cargo.lock crates/*/Cargo.toml
else
  git add Cargo.toml crates/*/Cargo.toml
fi

git commit -m "Release $tag"
git tag "$tag"
echo "tagged $tag"
