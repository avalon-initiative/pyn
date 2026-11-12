#!/usr/bin/env bash
# Releases pyn and the sibling pyn-web checkout together: both release PRs, merge on green, then tag pyn-web and pyn.
# Inputs: VER (required), TITLE, YES=1, PYN_WEB_DIR (default ../pyn-web), PYN_COMMON_UI_DIR (default ../pyn-common-ui),
# RELEASE_WAIT_MAX and RELEASE_WAIT_STEP (seconds). SKIP_CHECKS and FORCE_CHECKS pass through to both release scripts.
set -Eeuo pipefail

die() { echo "release-all: $*" >&2; exit 1; }

root=$(git rev-parse --show-toplevel)
web=$(cd "$root" && cd "${PYN_WEB_DIR:-../pyn-web}" 2>/dev/null && pwd) || die "no pyn-web checkout at ${PYN_WEB_DIR:-../pyn-web} (set PYN_WEB_DIR)"
ui="${PYN_COMMON_UI_DIR:-$root/../pyn-common-ui}"
ver="${VER:-}"
title="${TITLE-}"
wait_max="${RELEASE_WAIT_MAX:-5400}"
wait_step="${RELEASE_WAIT_STEP:-30}"

[ -n "$ver" ] || die "VER is required (VER=x.y.z)"
[[ $ver =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "invalid version '$ver' (expected X.Y.Z)"

pyn_version() { sed -n 's/^version = "\(.*\)"$/\1/p' "$root/Cargo.toml" | head -n 1; }
web_version() { sed -n 's/^  "version": "\(.*\)",$/\1/p' "$web/package.json" | head -n 1; }

# Both repos must be on main with a clean tree; release.sh in each repeats this and also checks origin.
require_clean_main() {
  local dir=$1 name=$2
  [ -d "$dir/.git" ] || [ -f "$dir/.git" ] || die "$name is not a git checkout: $dir"
  [ "$(git -C "$dir" rev-parse --abbrev-ref HEAD)" = main ] || die "$name is not on main"
  [ -z "$(git -C "$dir" status --porcelain)" ] || die "$name has a dirty working tree"
}

require_clean_main "$root" pyn
require_clean_main "$web" pyn-web
[ -f "$web/scripts/release.sh" ] || die "pyn-web has no scripts/release.sh"

have_pyn=$(pyn_version)
have_web=$(web_version)
[ "$have_pyn" = "$have_web" ] || die "versions differ: pyn is $have_pyn, pyn-web is $have_web; align them before a joint release"
[ "$have_pyn" != "$ver" ] && [ "$(printf '%s\n%s\n' "$have_pyn" "$ver" | sort -V | tail -n 1)" = "$ver" ] ||
  die "version $ver is not greater than the current $have_pyn"

# pyn-common-ui is released separately and first; warn when main there is ahead of what pyn-web pins.
if [ -f "$web/package.json" ] && [ -d "$ui/.git" ]; then
  pinned=$(sed -n 's/.*"@avalon-initiative\/pyn-common-ui": *"[^0-9]*\([0-9][^"]*\)".*/\1/p' "$web/package.json" | head -n 1)
  latest=$(git -C "$ui" show origin/main:package.json 2>/dev/null | sed -n 's/^  "version": "\(.*\)",$/\1/p' | head -n 1)
  if [ -n "$pinned" ] && [ -n "$latest" ] && [ "$pinned" != "$latest" ]; then
    echo "release-all: WARNING pyn-common-ui is at $latest on origin/main but pyn-web pins $pinned; release pyn-common-ui first if pyn-web needs the change"
  fi
else
  echo "release-all: note: pyn-common-ui is not checked; release it first if it changed"
fi

cat <<TXT
release-all: about to release version $ver${title:+ ("$title")}

  1. open release PRs in pyn ($root) and pyn-web ($web)
  2. wait for CI on both, up to ${wait_max}s, then squash-merge both
  3. PUSH TAGS: v$ver${title:+-<slug>} in pyn-web, then in pyn. Pushing a tag PUBLISHES a release
     (images, binaries, GitHub Release). This is outward-facing and cannot be undone.
TXT
if [ "${YES:-}" != 1 ]; then
  [ -t 0 ] || die "no terminal to ask for confirmation; set YES=1 to answer yes"
  read -r -p "Publish a joint release $ver? [y/N] " answer || answer=""
  [[ $answer =~ ^[Yy]([Ee][Ss])?$ ]] || die "aborted; nothing was changed"
fi

# Both release scripts run non-interactively here.
export YES=1 VER="$ver" TITLE="$title"
(cd "$root" && scripts/release.sh)
git -C "$root" switch --quiet main
(cd "$web" && bash scripts/release.sh release)
git -C "$web" switch --quiet main

pr_of() { gh -R "$1" pr list --head "release-v$ver" --state open --json number --jq '.[0].number'; }
slug_of() { git -C "$1" remote get-url origin | sed -E 's#\.git$##; s#^.*[:/]([^/]+/[^/]+)$#\1#'; }

pyn_slug=$(slug_of "$root"); web_slug=$(slug_of "$web")
pyn_pr=$(pr_of "$pyn_slug"); web_pr=$(pr_of "$web_slug")
[ -n "$pyn_pr" ] && [ -n "$web_pr" ] || die "release PRs not found (pyn: '$pyn_pr', pyn-web: '$web_pr')"
echo "release-all: PRs: pyn#$pyn_pr, pyn-web#$web_pr"

# Green means mergeStateStatus CLEAN; a failed check aborts early instead of waiting out the clock.
wait_green() {
  local slug=$1 pr=$2 waited=0 state rc out
  while :; do
    state=$(gh -R "$slug" pr view "$pr" --json mergeStateStatus --jq .mergeStateStatus)
    [ "$state" = CLEAN ] && return 0
    case $state in BEHIND | DIRTY) die "PR $slug#$pr is $state; resolve it and merge by hand" ;; esac
    rc=0; out=$(gh -R "$slug" pr checks "$pr" 2>&1) || rc=$?
    if [ "$rc" -eq 1 ] && ! grep -qi "no checks reported" <<<"$out"; then die "checks failed on $slug#$pr"; fi
    [ "$waited" -lt "$wait_max" ] || die "timed out waiting for CI on $slug#$pr (state $state)"
    sleep "$wait_step"; waited=$((waited + wait_step))
  done
}
wait_green "$pyn_slug" "$pyn_pr"
wait_green "$web_slug" "$web_pr"

gh -R "$web_slug" pr merge "$web_pr" --squash
gh -R "$pyn_slug" pr merge "$pyn_pr" --squash

git -C "$web" pull --quiet --ff-only origin main
git -C "$root" pull --quiet --ff-only origin main
[ "$(pyn_version)" = "$ver" ] && [ "$(web_version)" = "$ver" ] ||
  die "after the merge pyn is $(pyn_version) and pyn-web is $(web_version), not $ver; nothing was tagged"

echo "release-all: tagging pyn-web, then pyn"
(cd "$web" && bash scripts/release.sh publish)
(cd "$root" && scripts/release.sh publish)
echo "release-all: both tags pushed; watch the release workflows"
