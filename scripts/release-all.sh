#!/usr/bin/env bash
# Releases pyn and the sibling pyn-web checkout in one go; each has its own version and either may be skipped.
# Inputs: VER (pyn) and WEB_VER (pyn-web), at least one; TITLE and WEB_TITLE (defaults to TITLE), YES=1, PYN_WEB_DIR (default ../pyn-web),
# PYN_COMMON_UI_DIR (default ../pyn-common-ui), RELEASE_WAIT_MAX and RELEASE_WAIT_STEP (seconds). SKIP_CHECKS and FORCE_CHECKS pass through.
set -Eeuo pipefail

die() { echo "release-all: $*" >&2; exit 1; }

root=$(git rev-parse --show-toplevel)
ui="${PYN_COMMON_UI_DIR:-$root/../pyn-common-ui}"
ver="${VER:-}"
web_ver="${WEB_VER:-}"
title="${TITLE-}"
web_title="${WEB_TITLE-$title}"
wait_max="${RELEASE_WAIT_MAX:-5400}"
wait_step="${RELEASE_WAIT_STEP:-30}"
web=""

[ -n "$ver" ] || [ -n "$web_ver" ] || die "VER (pyn) or WEB_VER (pyn-web) is required (X.Y.Z)"
for v in "$ver" "$web_ver"; do
  [ -z "$v" ] || [[ $v =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "invalid version '$v' (expected X.Y.Z)"
done

if [ -n "$web_ver" ]; then
  web=$(cd "$root" && cd "${PYN_WEB_DIR:-../pyn-web}" 2>/dev/null && pwd) || die "no pyn-web checkout at ${PYN_WEB_DIR:-../pyn-web} (set PYN_WEB_DIR)"
fi

pyn_version() { sed -n 's/^version = "\(.*\)"$/\1/p' "$root/Cargo.toml" | head -n 1; }
web_version() { sed -n 's/^  "version": "\(.*\)",$/\1/p' "$web/package.json" | head -n 1; }
greater() { [ "$1" != "$2" ] && [ "$(printf '%s\n%s\n' "$1" "$2" | sort -V | tail -n 1)" = "$2" ]; }

# Each repo being released must be on main with a clean tree; release.sh in each repeats this and also checks origin.
require_clean_main() {
  local dir=$1 name=$2
  [ -d "$dir/.git" ] || [ -f "$dir/.git" ] || die "$name is not a git checkout: $dir"
  [ "$(git -C "$dir" rev-parse --abbrev-ref HEAD)" = main ] || die "$name is not on main"
  [ -z "$(git -C "$dir" status --porcelain)" ] || die "$name has a dirty working tree"
}

[ -z "$ver" ] || require_clean_main "$root" pyn
if [ -n "$web_ver" ]; then
  require_clean_main "$web" pyn-web
  [ -f "$web/scripts/release.sh" ] || die "pyn-web has no scripts/release.sh"
fi

[ -z "$ver" ] || greater "$(pyn_version)" "$ver" || die "pyn version $ver is not greater than the current $(pyn_version)"
[ -z "$web_ver" ] || greater "$(web_version)" "$web_ver" || die "pyn-web version $web_ver is not greater than the current $(web_version)"

# pyn-common-ui is released separately and first; warn when main there is ahead of what pyn-web pins.
if [ -n "$web_ver" ]; then
  if [ -f "$web/package.json" ] && [ -d "$ui/.git" ]; then
    pinned=$(sed -n 's/.*"@avalon-initiative\/pyn-common-ui": *"[^0-9]*\([0-9][^"]*\)".*/\1/p' "$web/package.json" | head -n 1)
    latest=$(git -C "$ui" show origin/main:package.json 2>/dev/null | sed -n 's/^  "version": "\(.*\)",$/\1/p' | head -n 1)
    if [ -n "$pinned" ] && [ -n "$latest" ] && [ "$pinned" != "$latest" ]; then
      echo "release-all: WARNING pyn-common-ui is at $latest on origin/main but pyn-web pins $pinned; release pyn-common-ui first if pyn-web needs the change"
    fi
  else
    echo "release-all: note: pyn-common-ui is not checked; release it first if it changed"
  fi
fi

cat <<TXT
release-all: about to release${ver:+ pyn $ver${title:+ ("$title")}}${web_ver:+ pyn-web $web_ver${web_title:+ ("$web_title")}}

  1. open release PRs: ${ver:+pyn ($root) }${web_ver:+pyn-web ($web)}
  2. wait for CI, up to ${wait_max}s, then squash-merge
  3. PUSH TAGS: ${web_ver:+pyn-web v$web_ver${web_title:+-<slug>}, }${ver:+pyn v$ver${title:+-<slug>}}. Pushing a tag PUBLISHES a release
     (images, binaries, GitHub Release). This is outward-facing and cannot be undone.
TXT
if [ "${YES:-}" != 1 ]; then
  [ -t 0 ] || die "no terminal to ask for confirmation; set YES=1 to answer yes"
  read -r -p "Publish these releases? [y/N] " answer || answer=""
  [[ $answer =~ ^[Yy]([Ee][Ss])?$ ]] || die "aborted; nothing was changed"
fi

# The release scripts run non-interactively here.
export YES=1
if [ -n "$ver" ]; then
  (cd "$root" && VER="$ver" TITLE="$title" scripts/release.sh)
  git -C "$root" switch --quiet main
fi
if [ -n "$web_ver" ]; then
  (cd "$web" && VER="$web_ver" TITLE="$web_title" bash scripts/release.sh release)
  git -C "$web" switch --quiet main
fi

pr_of() { gh -R "$1" pr list --head "release-v$2" --state open --json number --jq '.[0].number'; }
slug_of() { git -C "$1" remote get-url origin | sed -E 's#\.git$##; s#^.*[:/]([^/]+/[^/]+)$#\1#'; }

pyn_pr="" web_pr=""
if [ -n "$ver" ]; then
  pyn_slug=$(slug_of "$root")
  pyn_pr=$(pr_of "$pyn_slug" "$ver"); [ -n "$pyn_pr" ] || die "release PR not found in pyn"
  echo "release-all: PR pyn#$pyn_pr"
fi
if [ -n "$web_ver" ]; then
  web_slug=$(slug_of "$web")
  web_pr=$(pr_of "$web_slug" "$web_ver"); [ -n "$web_pr" ] || die "release PR not found in pyn-web"
  echo "release-all: PR pyn-web#$web_pr"
fi

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
[ -z "$pyn_pr" ] || wait_green "$pyn_slug" "$pyn_pr"
[ -z "$web_pr" ] || wait_green "$web_slug" "$web_pr"

if [ -n "$web_pr" ]; then
  gh -R "$web_slug" pr merge "$web_pr" --squash
  git -C "$web" pull --quiet --ff-only origin main
  [ "$(web_version)" = "$web_ver" ] || die "after the merge pyn-web is $(web_version), not $web_ver; nothing was tagged"
fi
if [ -n "$pyn_pr" ]; then
  gh -R "$pyn_slug" pr merge "$pyn_pr" --squash
  git -C "$root" pull --quiet --ff-only origin main
  [ "$(pyn_version)" = "$ver" ] || die "after the merge pyn is $(pyn_version), not $ver; nothing was tagged"
fi

# pyn-web is tagged first so its image is likely published when the pyn smoke test pulls the latest web release.
[ -z "$web_ver" ] || { echo "release-all: tagging pyn-web v$web_ver"; (cd "$web" && bash scripts/release.sh publish); }
[ -z "$ver" ] || { echo "release-all: tagging pyn v$ver"; (cd "$root" && scripts/release.sh publish); }
echo "release-all: tags pushed; watch the release workflows"
