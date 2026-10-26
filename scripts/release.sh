#!/usr/bin/env bash
# `release.sh` bumps the version on a release branch and opens a pull request; `release.sh publish`
# tags the merged commit on main. Inputs: VER, TITLE, SKIP_CHECKS=1, YES=1 (skip the questions).
set -Eeuo pipefail

die() { echo "release: $*" >&2; exit 1; }

# Asks a question on the terminal; YES=1 answers yes, and a non-terminal stdin fails instead of hanging.
confirm() {
  [ "${YES:-}" = 1 ] && return 0
  [ -t 0 ] || die "no terminal to ask '$1'; set YES=1 to answer yes"
  local answer
  read -r -p "$1 [y/N] " answer || answer=""
  [[ $answer =~ ^[Yy]([Ee][Ss])?$ ]]
}

# Prompts for a value, failing on a non-terminal stdin.
ask() {
  [ -t 0 ] || die "no terminal to ask '$1'; pass $2 on the command line"
  local value
  read -r -p "$1: " value || die "no input"
  printf '%s' "$value"
}

preconditions() {
  [ "$(git rev-parse --abbrev-ref HEAD)" = main ] || die "not on main"
  [ -z "$(git status --porcelain)" ] || die "the working tree is not clean; commit or stash first"
  git fetch --quiet origin main || die "cannot fetch origin"
  [ "$(git rev-parse HEAD)" = "$(git rev-parse origin/main)" ] || die "main is not up to date with origin/main"
  gh auth status >/dev/null 2>&1 || die "gh is not authenticated (gh auth login)"
}

tag_exists() {
  git rev-parse -q --verify "refs/tags/$1" >/dev/null && return 0
  [ -n "$(git ls-remote --tags origin "refs/tags/$1")" ]
}

workspace_version() { sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml; }

release() {
  preconditions
  if [ "${SKIP_CHECKS:-}" = 1 ]; then
    echo "release: skipping the checks (SKIP_CHECKS=1)"
  else
    make check || die "the checks failed; nothing was changed"
  fi

  [ "$(grep -c '^version = ' Cargo.toml)" -eq 1 ] || die "expected exactly one 'version = ' line in Cargo.toml"
  local current ver title slug tag branch
  current=$(workspace_version)
  [[ $current =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "cannot parse the workspace version '$current' from Cargo.toml"

  ver="${VER:-}"
  [ -n "$ver" ] || ver=$(ask "Release version (current: $current)" VER)
  [[ $ver =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "invalid version '$ver' (expected X.Y.Z)"
  [ "$ver" != "$current" ] && [ "$(printf '%s\n%s\n' "$current" "$ver" | sort -V | tail -n 1)" = "$ver" ] ||
    die "version $ver is not greater than the current $current"

  if [ -n "${TITLE+set}" ]; then title="$TITLE"; else title=$(ask "Release title (optional)" TITLE); fi
  slug=$(printf '%s' "$title" | sed -E 's/[[:space:]]+/-/g; s/[^A-Za-z0-9._-]//g; s/^-+//; s/-+$//')
  tag="v$ver"
  [ -z "$slug" ] || tag="$tag-$slug"
  git check-ref-format --allow-onelevel "$tag" || die "invalid tag name '$tag'"
  tag_exists "$tag" && die "tag '$tag' already exists"
  branch="release-v$ver"
  git rev-parse -q --verify "refs/heads/$branch" >/dev/null && die "branch '$branch' already exists"
  [ -z "$(git ls-remote --heads origin "refs/heads/$branch")" ] || die "branch '$branch' already exists on origin"

  trap 'echo "release: failed after creating the branch; clean up with: git switch main && git branch -D $branch" >&2' ERR
  git switch --quiet -c "$branch"

  # The members pin their intra-workspace dependencies at the workspace version.
  sed -i "s/^version = \"$current\"/version = \"$ver\"/" Cargo.toml
  sed -i "s/^\\(pyn-[a-z-]* = { version = \"\\)$current/\\1$ver/" crates/*/Cargo.toml
  # The lock pins the member versions, so metadata re-resolves and rewrites it.
  cargo metadata --format-version 1 >/dev/null
  # The published OpenAPI document embeds the workspace version.
  make openapi
  git add Cargo.toml Cargo.lock crates/*/Cargo.toml docs/generated/openapi.json
  local subject="[noissue] - Release $tag"
  git commit --quiet -m "$subject"

  echo "release: tag $tag, version $ver, branch $branch"
  git show --stat --format= HEAD
  if confirm "Push the branch and open the pull request?"; then
    git push -u origin "$branch"
    gh pr create --base main --head "$branch" --title "$subject" \
      --body "Releases $tag. The tag is created on the merged commit with \`make release-publish\`, which starts the release pipeline."
  else
    echo "release: to continue later:"
    echo "  git push -u origin $branch"
    echo "  gh pr create --base main --head $branch --title '$subject'"
  fi
  trap - ERR
}

publish() {
  preconditions
  local subject tag ver bad
  subject=$(git log -1 --format=%s)
  [[ $subject =~ ^\[noissue\]\ -\ Release\ (v([0-9]+\.[0-9]+\.[0-9]+)(-[0-9A-Za-z._-]+)?)\ \(#[0-9]+\)$ ]] ||
    die "HEAD is not a merged release commit ('[noissue] - Release v<version>[-title] (#N)'): $subject"
  tag="${BASH_REMATCH[1]}"
  ver="${BASH_REMATCH[2]}"
  scripts/verify-release-version.sh "$tag" >/dev/null || die "the workspace version at HEAD is not $ver"
  bad=$(grep -Hn "^pyn-[a-z-]* = { version = " crates/*/Cargo.toml | grep -v "version = \"$ver\"" || true)
  [ -z "$bad" ] || die "intra-workspace pins are not $ver:"$'\n'"$bad"
  grep -q "^    \"version\": \"$ver\"" docs/generated/openapi.json || die "docs/generated/openapi.json is not at $ver"
  tag_exists "$tag" && die "tag '$tag' already exists"
  confirm "Create tag $tag on $(git rev-parse --short HEAD) and push it? This starts the release pipeline." || die "aborted"
  git tag -a "$tag" -m "$tag"
  git push origin "$tag"
}

cd "$(git rev-parse --show-toplevel)"
case "${1:-}" in
  "") release ;;
  publish) publish ;;
  *) die "usage: $0 [publish]" ;;
esac
