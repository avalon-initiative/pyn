#!/usr/bin/env bash
# Tests release.sh, release-all.sh, ci-green-for-tree.sh and verify-release-version.sh in a throwaway repository with a bare origin and a stub gh. No network.
set -euo pipefail
unset YES VER TITLE SKIP_CHECKS FORCE_CHECKS GATE_FAILS GH_AUTH_EXIT GH_API_FAIL GH_REPO GITHUB_REPOSITORY PYN_WEB_DIR PYN_COMMON_UI_DIR

src=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@example.invalid GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@example.invalid
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null
fails=0
pass() { echo "ok   $1"; }
fail() { echo "FAIL $1" >&2; fails=$((fails + 1)); }
expect() { # expect <name> <condition-command...>
  local name=$1; shift
  if "$@" >/dev/null 2>&1; then pass "$name"; else fail "$name"; fi
}

# verify-release-version.sh: tag shape, version match and the prerelease classification.
v="$work/verify"
mkdir -p "$v/scripts"
cp "$src/scripts/verify-release-version.sh" "$v/scripts/"
printf '[workspace.package]\nversion = "1.2.3"\n' > "$v/Cargo.toml"
verify() { GITHUB_OUTPUT="$work/out" "$v/scripts/verify-release-version.sh" "$1" >/dev/null 2>&1; }
prerelease_of() { : > "$work/out"; verify "$1" && sed -n 's/^prerelease=//p' "$work/out"; }
for t in v1.2.3 v1.2.3-first-stable v1.2.3-Big.Title_2 v1.2.3-rc.1; do expect "verify accepts $t" verify "$t"; done
for t in v1.2.4 1.2.3 v1.2 v1.2.3- "v1.2.3-bad title" v1.2.3+x; do expect "verify rejects $t" bash -c '! "$0" "$1" >/dev/null 2>&1' "$v/scripts/verify-release-version.sh" "$t"; done
for t in v1.2.3 v1.2.3-first-stable v1.2.3-alphabet v1.2.3-prefix v1.2.3-rcs v1.2.3-bet v1.2.3-the-rc; do
  expect "$t is not a prerelease" test "$(prerelease_of "$t")" = false
done
for t in v1.2.3-alpha v1.2.3-beta.2 v1.2.3-rc1 v1.2.3-RC.1 v1.2.3-Pre-release v1.2.3-pre1 v1.2.3-alpha-first; do
  expect "$t is a prerelease" test "$(prerelease_of "$t")" = true
done
: > "$work/out"; verify v1.2.3-title
expect "verify writes the plain version" grep -qx 'version=1.2.3' "$work/out"

# Stub gh: `auth status` follows GH_AUTH_EXIT; `api` answers from the JSON files in $GH_FIX through the real --jq filter
# (and fails on a missing file or GH_API_FAIL); everything else is logged to $GH_LOG.
cat > "$work/gh-stub" <<'STUB'
#!/bin/bash
[ "$1 $2" = "auth status" ] && exit "${GH_AUTH_EXIT:-0}"
if [ "$1" = api ]; then
  ep=$2; shift 2; q=.
  while [ $# -gt 0 ]; do [ "$1" = --jq ] && q=$2; shift; done
  [ -z "${GH_API_FAIL:-}" ] || exit 1
  case $ep in
    */pulls) f=$GH_FIX/pulls.json ;;
    */git/commits/*) f=$GH_FIX/commit-${ep##*/}.json ;;
    */actions/runs*) f=$GH_FIX/runs.json ;;
    *) exit 1 ;;
  esac
  [ -f "$f" ] || exit 1
  exec jq -r "$q" "$f"
fi
echo "$@" >> "$GH_LOG"
STUB
export GH_FIX="$work/fix"

# Fixture: a two-crate workspace, a bare origin and a clone on main with a stub gh.
new_repo() {
  local d=$1
  rm -rf "$d" "$d.git" "$GH_FIX"; mkdir -p "$GH_FIX"
  git init -q --bare -b main "$d.git"
  git clone -q "$d.git" "$d" 2>/dev/null
  mkdir -p "$d/scripts" "$d/crates/pyn-a/src" "$d/crates/pyn-b/src" "$d/docs/generated" "$d/bin"
  cp "$src/scripts/release.sh" "$src/scripts/verify-release-version.sh" "$src/scripts/ci-green-for-tree.sh" "$src/scripts/release-all.sh" "$d/scripts/"
  printf '[workspace]\nmembers = ["crates/*"]\nresolver = "2"\n\n[workspace.package]\nversion = "0.0.1"\n' > "$d/Cargo.toml"
  printf '[package]\nname = "pyn-a"\nversion.workspace = true\nedition = "2021"\n\n[dependencies]\npyn-b = { version = "0.0.1", path = "../pyn-b" }\n' > "$d/crates/pyn-a/Cargo.toml"
  printf '[package]\nname = "pyn-b"\nversion.workspace = true\nedition = "2021"\n' > "$d/crates/pyn-b/Cargo.toml"
  : > "$d/crates/pyn-a/src/lib.rs"; : > "$d/crates/pyn-b/src/lib.rs"
  printf '{\n  "info": {\n    "version": "0.0.1"\n  }\n}\n' > "$d/docs/generated/openapi.json"
  printf 'check:\n\t@test -z "$$GATE_FAILS"\nopenapi:\n\t@sed -i "s/^    \\"version\\": .*/    \\"version\\": \\"$$(sed -n "s/^version = \\"\\(.*\\)\\"$$/\\1/p" Cargo.toml)\\"/" docs/generated/openapi.json\n' > "$d/Makefile"
  cp "$work/gh-stub" "$d/bin/gh"
  chmod +x "$d/bin/gh"
  printf 'bin\n' > "$d/.gitignore"
  (cd "$d" && cargo metadata --format-version 1 >/dev/null 2>&1 && git add -A && git commit -qm init && git push -q origin main)
}
r="$work/repo"
export GH_LOG="$work/gh.log"
run() { (cd "$r" && PATH="$r/bin:$PATH" "$@" </dev/null 2>"$work/err"); }
release() { run env "$@" scripts/release.sh; }
publish() { run env "$@" scripts/release.sh publish; }
expect_refused() { # expect_refused <name> <message-substring> <command...>
  local name=$1 msg=$2; shift 2
  if ! "$@" >/dev/null && grep -q -- "$msg" "$work/err"; then pass "$name"; else fail "$name"; fi
}
unchanged() { [ -z "$(git -C "$r" status --porcelain)" ] && [ "$(git -C "$r" rev-parse --abbrev-ref HEAD)" = main ]; }

new_repo "$r"
expect_refused "refuses a bad version" "invalid version" release VER=1.0 TITLE= YES=1
expect_refused "refuses an equal version" "not greater" release VER=0.0.1 TITLE= YES=1
expect_refused "refuses a lower version" "not greater" release VER=0.0.0 TITLE= YES=1
expect_refused "refuses a missing version without a terminal" "no terminal" release TITLE= YES=1
expect_refused "refuses a missing title without a terminal" "no terminal" release VER=0.1.0 YES=1
expect_refused "refuses an unauthenticated gh" "not authenticated" release GH_AUTH_EXIT=1 VER=0.1.0 TITLE= YES=1
echo dirty > "$r/dirty"
expect_refused "refuses a dirty tree" "not clean" release VER=0.1.0 TITLE= YES=1
rm "$r/dirty"
git -C "$r" tag -a v0.1.0 -m x
expect_refused "refuses an existing local tag" "already exists" release VER=0.1.0 TITLE= YES=1
git -C "$r" tag -d v0.1.0 >/dev/null
git -C "$r" tag -a v0.1.0-other -m x
expect_refused "refuses another title for a tagged version" "already exists" release VER=0.1.0 TITLE=new YES=1
git -C "$r" tag -d v0.1.0-other >/dev/null
git -C "$r" push -q origin HEAD:refs/tags/v0.2.0
expect_refused "refuses an existing remote tag" "already exists" release VER=0.2.0 TITLE= YES=1
expect_refused "refuses another title for a version tagged on origin" "already exists" release VER=0.2.0 TITLE=new YES=1
git -C "$r" push -q origin :refs/tags/v0.2.0
expect "refusals change nothing" unchanged
echo more >> "$r/docs/generated/openapi.json"; git -C "$r" commit -qam local-only
expect_refused "refuses a main that differs from origin" "not up to date" release VER=0.1.0 TITLE= YES=1
git -C "$r" reset -q --hard origin/main

expect_refused "a failing gate changes nothing" "checks failed" release GATE_FAILS=1 VER=0.1.0 TITLE= YES=1
expect "a failing gate leaves main untouched" unchanged
expect "SKIP_CHECKS skips the gate" bash -c 'cd "$0" && PATH="$0/bin:$PATH" GATE_FAILS=1 SKIP_CHECKS=1 VER=0.1.0 TITLE="First stable!" YES=1 scripts/release.sh </dev/null >/dev/null 2>&1' "$r"

# The branch from the SKIP_CHECKS run: a title is slugged, every version file is bumped, the PR is opened.
tag=v0.1.0-First-stable
expect "release makes the branch" git -C "$r" rev-parse --verify -q release-v0.1.0
expect "commit subject names the tag" test "$(git -C "$r" log -1 --format=%s release-v0.1.0)" = "[noissue] - Release $tag"
changed=$(git -C "$r" diff --name-only main release-v0.1.0 | tr '\n' ' ')
expect "release changes exactly the version files" test "$changed" = "Cargo.lock Cargo.toml crates/pyn-a/Cargo.toml docs/generated/openapi.json "
expect "release bumps the workspace version" git -C "$r" grep -q '^version = "0.1.0"' release-v0.1.0 -- Cargo.toml
expect "release bumps the intra-workspace pin" git -C "$r" grep -q 'pyn-b = { version = "0.1.0"' release-v0.1.0 -- crates/pyn-a/Cargo.toml
expect "release bumps the lock" bash -c 'git -C "$0" show release-v0.1.0:Cargo.lock | grep -q "version = \"0.1.0\""' "$r"
expect "release bumps the openapi document" git -C "$r" grep -q '"version": "0.1.0"' release-v0.1.0 -- docs/generated/openapi.json
expect "release pushes the branch" git -C "$r" ls-remote --exit-code --heads origin release-v0.1.0 >/dev/null
expect "release pushes no tag" test -z "$(git -C "$r" ls-remote --tags origin)"
expect "release does not move origin main" test "$(git -C "$r" rev-parse origin/main)" = "$(git -C "$r" rev-parse main)"
expect "release opens the pull request" grep -q -- "pr create --base main --head release-v0.1.0 --title \[noissue\] - Release $tag" "$GH_LOG"
expect "release leaves no tag" test -z "$(git -C "$r" tag)"
expect "release stays on its branch" test "$(git -C "$r" rev-parse --abbrev-ref HEAD)" = release-v0.1.0

# Declining the question pushes nothing.
new_repo "$r"
: > "$GH_LOG"
declined=$(cd "$r" && PATH="$r/bin:$PATH" VER=0.3.0 TITLE= bash -c 'scripts/release.sh <<<n' 2>&1 || true)
expect "no terminal and no YES refuses the question" grep -q "no terminal" <<<"$declined"
expect "declining pushes no branch" test -z "$(git -C "$r" ls-remote --heads origin 'release-*')"
expect "declining opens no pull request" test ! -s "$GH_LOG"

# publish on a simulated squash merge.
new_repo "$r"
release VER=0.1.0 TITLE=first-stable YES=1 >/dev/null
merge() { # merge <subject>: squash-merge the release branch into main and publish it to origin
  git -C "$r" switch -q main
  git -C "$r" merge -q --squash release-v0.1.0 >/dev/null
  git -C "$r" commit -qm "$1"
  git -C "$r" push -q origin main
}
expect_refused "publish refuses a branch other than main" "not on main" publish YES=1
git -C "$r" switch -q main
expect_refused "publish refuses a non-release HEAD" "not a merged release commit" publish YES=1
merge "[noissue] - Release v0.1.0-first-stable (#7)"
expect_refused "publish needs a terminal or YES" "no terminal" publish
expect_refused "publish refuses an unauthenticated gh" "not authenticated" publish GH_AUTH_EXIT=1 YES=1
git -C "$r" tag -a v0.1.0-first-stable -m x
expect_refused "publish refuses an existing tag" "already exists" publish YES=1
git -C "$r" tag -d v0.1.0-first-stable >/dev/null
publish YES=1 >/dev/null
expect "publish tags the merged commit" test "$(git -C "$r" rev-parse 'v0.1.0-first-stable^{commit}')" = "$(git -C "$r" rev-parse HEAD)"
expect "publish makes an annotated tag" test "$(git -C "$r" cat-file -t v0.1.0-first-stable)" = tag
expect "publish pushes only the tag" test "$(git -C "$r" ls-remote --tags origin | grep -c 'v0.1.0-first-stable$')" = 1

# publish refuses a merge whose subject disagrees with the files.
new_repo "$r"
release VER=0.1.0 TITLE= YES=1 >/dev/null
merge "[noissue] - Release v0.2.0 (#8)"
expect_refused "publish refuses a version that differs from the files" "workspace version" publish YES=1

# ci-green-for-tree.sh: a passing CI run on a PR head with the same tree, else non-zero with a reason.
head=1111111111111111111111111111111111111111
other=2222222222222222222222222222222222222222
fixtures() { # fixtures <pr-head-tree> <ci-conclusion>
  printf '[{"number":7,"head":{"sha":"%s"}}]\n' "$head" > "$GH_FIX/pulls.json"
  printf '{"tree":{"sha":"%s"}}\n' "$1" > "$GH_FIX/commit-$head.json"
  printf '{"workflow_runs":[{"name":"Other","conclusion":"failure","created_at":"2026-01-02T00:00:00Z","html_url":"x"},{"name":"CI","conclusion":%s,"status":"in_progress","created_at":"2026-01-01T00:00:00Z","html_url":"https://example.invalid/run/1"}]}\n' "$2" > "$GH_FIX/runs.json"
}
tree=$(git -C "$r" rev-parse 'origin/main^{tree}')
green() { run scripts/ci-green-for-tree.sh "$@"; }
new_repo "$r"
tree=$(git -C "$r" rev-parse 'origin/main^{tree}')
fixtures "$tree" '"success"'
expect "tree check passes on a matching tree with a green run" green origin/main
expect "tree check names the PR and run" bash -c 'grep -q "PR #7 (https://example.invalid/run/1)" <<<"$(cd "$0" && PATH="$0/bin:$PATH" scripts/ci-green-for-tree.sh origin/main)"' "$r"
expect "tree check defaults to HEAD" green
fixtures "$other" '"success"'
expect_refused "tree check refuses a different tree" "different tree" green origin/main
fixtures "$tree" '"failure"'
expect_refused "tree check refuses a failing run" "is failure" green origin/main
fixtures "$tree" null
expect_refused "tree check refuses a run still in progress" "is in_progress" green origin/main
printf '{"workflow_runs":[{"name":"Other","conclusion":"success","created_at":"2026-01-01T00:00:00Z","html_url":"x"}]}\n' > "$GH_FIX/runs.json"
expect_refused "tree check refuses a head with no CI run" "is none" green origin/main
printf '[]\n' > "$GH_FIX/pulls.json"
expect_refused "tree check refuses a commit with no pull request" "no pull request" green origin/main
fixtures "$tree" '"success"'
expect "tree check fails on an API error" bash -c '! (cd "$0" && PATH="$0/bin:$PATH" GH_API_FAIL=1 scripts/ci-green-for-tree.sh origin/main)' "$r"
expect_refused "tree check says why on an API error" "cannot decide" run env GH_API_FAIL=1 scripts/ci-green-for-tree.sh origin/main
expect "tree check fails on an unknown commit" bash -c '! (cd "$0" && PATH="$0/bin:$PATH" scripts/ci-green-for-tree.sh nonexistent)' "$r"

# release.sh: the local gate is skipped on a green tree; FORCE_CHECKS and a non-matching tree run it.
new_repo "$r"
tree=$(git -C "$r" rev-parse 'origin/main^{tree}')
fixtures "$tree" '"success"'
expect_refused "FORCE_CHECKS runs the gate despite a green tree" "checks failed" release FORCE_CHECKS=1 GATE_FAILS=1 VER=0.1.0 TITLE= YES=1
expect_refused "SKIP_CHECKS with FORCE_CHECKS is refused" "contradict" release SKIP_CHECKS=1 FORCE_CHECKS=1 VER=0.1.0 TITLE= YES=1
expect "refusals leave main untouched" unchanged
out=$(release GATE_FAILS=1 VER=0.1.0 TITLE= YES=1 2>&1)
expect "a green tree skips the failing local gate" test -n "$(git -C "$r" rev-parse -q --verify release-v0.1.0)"
expect "the skip says why" grep -q "skipping the local checks; ci-green-for-tree: tree .* passed CI on PR #7" <<<"$out"
new_repo "$r"
tree=$(git -C "$r" rev-parse 'origin/main^{tree}')
fixtures "$other" '"success"'
expect_refused "a different tree still runs the gate" "checks failed" release GATE_FAILS=1 VER=0.1.0 TITLE= YES=1
fixtures "$tree" '"failure"'
expect_refused "a failing CI run still runs the gate" "checks failed" release GATE_FAILS=1 VER=0.1.0 TITLE= YES=1
expect "SKIP_CHECKS still wins without any CI data" bash -c 'cd "$0" && PATH="$0/bin:$PATH" GH_API_FAIL=1 GATE_FAILS=1 SKIP_CHECKS=1 VER=0.1.0 TITLE= YES=1 scripts/release.sh </dev/null >/dev/null 2>&1' "$r"

# release-all.sh refusals, all before anything is changed: a sibling pyn-web fixture beside the pyn one.
new_repo "$r"
w="$work/web"
rm -rf "$w"; mkdir -p "$w/scripts"
git init -q -b main "$w"
printf '{\n  "name": "pyn-web",\n  "version": "0.0.1",\n  "dependencies": {\n    "@avalon-initiative/pyn-common-ui": "^0.1.5"\n  }\n}\n' > "$w/package.json"
printf '#!/bin/sh\nexit 0\n' > "$w/scripts/release.sh"
(cd "$w" && git add -A && git commit -qm init)
: > "$GH_LOG"
all() { run env PYN_WEB_DIR="$w" PYN_COMMON_UI_DIR="$work/none" "$@" scripts/release-all.sh; }
expect_refused "release-all needs a version" "VER is required" all YES=1
expect_refused "release-all refuses a bad version" "invalid version" all VER=1.0 YES=1
expect_refused "release-all refuses a version that is not greater" "not greater" all VER=0.0.1 YES=1
expect_refused "release-all refuses a missing pyn-web checkout" "no pyn-web checkout" all PYN_WEB_DIR="$work/nowhere" VER=0.2.0 YES=1
expect_refused "release-all asks for confirmation" "no terminal" all VER=0.2.0
echo dirty > "$w/dirty"
expect_refused "release-all refuses a dirty pyn-web" "pyn-web has a dirty" all VER=0.2.0 YES=1
rm "$w/dirty"
echo dirty > "$r/dirty"
expect_refused "release-all refuses a dirty pyn" "pyn has a dirty" all VER=0.2.0 YES=1
rm "$r/dirty"
git -C "$w" switch -q -c topic
expect_refused "release-all refuses pyn-web off main" "pyn-web is not on main" all VER=0.2.0 YES=1
git -C "$w" switch -q main
git -C "$r" switch -q -c topic
expect_refused "release-all refuses pyn off main" "pyn is not on main" all VER=0.2.0 YES=1
git -C "$r" switch -q main
sed -i 's/"version": "0.0.1"/"version": "0.0.2"/' "$w/package.json"
git -C "$w" commit -qam bump
expect_refused "release-all refuses differing versions" "versions differ" all VER=0.2.0 YES=1
expect "release-all refusals change nothing" unchanged
expect "release-all refusals open no pull request" test ! -s "$GH_LOG"

if [ "$fails" -ne 0 ]; then echo "$fails release test(s) failed" >&2; exit 1; fi
echo "release tests passed"
