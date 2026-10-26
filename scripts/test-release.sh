#!/usr/bin/env bash
# Tests release.sh and verify-release-version.sh in a throwaway repository with a bare origin and a stub gh. No network.
set -euo pipefail

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

# Fixture: a two-crate workspace, a bare origin and a clone on main with a stub gh.
new_repo() {
  local d=$1
  rm -rf "$d" "$d.git"
  git init -q --bare -b main "$d.git"
  git clone -q "$d.git" "$d" 2>/dev/null
  mkdir -p "$d/scripts" "$d/crates/pyn-a/src" "$d/crates/pyn-b/src" "$d/docs/generated" "$d/bin"
  cp "$src/scripts/release.sh" "$src/scripts/verify-release-version.sh" "$d/scripts/"
  printf '[workspace]\nmembers = ["crates/*"]\nresolver = "2"\n\n[workspace.package]\nversion = "0.0.1"\n' > "$d/Cargo.toml"
  printf '[package]\nname = "pyn-a"\nversion.workspace = true\nedition = "2021"\n\n[dependencies]\npyn-b = { version = "0.0.1", path = "../pyn-b" }\n' > "$d/crates/pyn-a/Cargo.toml"
  printf '[package]\nname = "pyn-b"\nversion.workspace = true\nedition = "2021"\n' > "$d/crates/pyn-b/Cargo.toml"
  : > "$d/crates/pyn-a/src/lib.rs"; : > "$d/crates/pyn-b/src/lib.rs"
  printf '{\n  "info": {\n    "version": "0.0.1"\n  }\n}\n' > "$d/docs/generated/openapi.json"
  printf 'check:\n\t@test -z "$$GATE_FAILS"\nopenapi:\n\t@sed -i "s/^    \\"version\\": .*/    \\"version\\": \\"$$(sed -n "s/^version = \\"\\(.*\\)\\"$$/\\1/p" Cargo.toml)\\"/" docs/generated/openapi.json\n' > "$d/Makefile"
  printf '#!/bin/sh\n[ "$1 $2" = "auth status" ] && exit "${GH_AUTH_EXIT:-0}"\necho "$@" >> "$GH_LOG"\n' > "$d/bin/gh"
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
git -C "$r" push -q origin HEAD:refs/tags/v0.2.0
expect_refused "refuses an existing remote tag" "already exists" release VER=0.2.0 TITLE= YES=1
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

if [ "$fails" -ne 0 ]; then echo "$fails release test(s) failed" >&2; exit 1; fi
echo "release tests passed"
