#!/usr/bin/env bash
# Checks a release tag (v<MAJOR>.<MINOR>.<PATCH>[-title-slug]) against the workspace version in Cargo.toml.
# Writes version and prerelease to $GITHUB_OUTPUT when set.
set -euo pipefail

tag="${1:?usage: $0 <tag>}"
if ! [[ $tag =~ ^v([0-9]+\.[0-9]+\.[0-9]+)(-[0-9A-Za-z._-]+)?$ ]]; then
  echo "tag '$tag' is not v<MAJOR>.<MINOR>.<PATCH>[-title-slug]" >&2
  exit 1
fi
want="${BASH_REMATCH[1]}"
suffix="${BASH_REMATCH[2]#-}"
have=$(sed -n 's/^version = "\(.*\)"$/\1/p' "$(dirname "$0")/../Cargo.toml")
if [ "$have" != "$want" ]; then
  echo "tag '$tag' does not match the workspace version $have" >&2
  exit 1
fi

# Only an alpha, beta, rc or pre suffix marks a pre-release; any other suffix is a title.
prerelease=false
if [[ ${suffix,,} =~ ^(alpha|beta|rc|pre)($|[.-]|[0-9]) ]]; then
  prerelease=true
fi
if [ -n "${GITHUB_OUTPUT:-}" ]; then
  printf 'version=%s\nprerelease=%s\n' "$want" "$prerelease" >> "$GITHUB_OUTPUT"
fi
echo "tag $tag matches workspace version $have (prerelease=$prerelease)"
