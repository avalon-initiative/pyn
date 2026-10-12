#!/usr/bin/env bash
# Regenerates <format>/expected/staging/ with the real git binary, used only as an oracle: the tests never run git.
# Each scenario starts from a fresh copy of the committed fixtures (laid out as the tests lay them out), runs git, and
# records `git ls-files -s`, `git status --porcelain` and `git diff --cached`, plus the commit for the scenarios
# that commit. The scenario names and commands are mirrored in tests/git_staging.rs. Rerun only when they change.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
export GIT_AUTHOR_NAME="Staging Author" GIT_AUTHOR_EMAIL="staging@example.com"
export GIT_COMMITTER_NAME="Staging Committer" GIT_COMMITTER_EMAIL="committer@example.com"
export GIT_AUTHOR_DATE="1700000000 +0200" GIT_COMMITTER_DATE="1700000100 -0530"
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null GIT_CONFIG_NOSYSTEM=1
export GIT_PAGER=cat LC_ALL=C TZ=UTC

copy_tree() {
  cp -a "$1/." "$2/"
  find "$2" -name dot-gitignore | while IFS= read -r f; do mv "$f" "$(dirname "$f")/.gitignore"; done
}

setup() {
  local format=$1 dest=$2
  mkdir -p "$dest/main" "$dest/linked"
  copy_tree "$here/worktree" "$dest/main"
  cp -a "$here/$format/dot-git" "$dest/main/.git"
  copy_tree "$here/linked" "$dest/linked"
  echo "gitdir: $dest/main/.git/worktrees/linked" > "$dest/linked/.git"
  echo "$dest/linked/.git" > "$dest/main/.git/worktrees/linked/gitdir"
}

# scenario <name> <worktree: main|linked> <subdir> <git command...>
scenario() {
  local name=$1 tree=$2 sub=$3; shift 3
  local work out
  work="$(mktemp -d)"
  setup "$format" "$work"
  out="$here/$format/expected/staging/$name"
  rm -rf "$out"; mkdir -p "$out"
  (
    cd "$work/$tree/$sub"
    "$@" > /dev/null 2>&1 || echo "$?" > "$out/exit.txt"
    cd "$work/$tree"
    git ls-files -s > "$out/ls-files.txt"
    git status --porcelain=v1 > "$out/status.txt"
    git diff --cached > "$out/diff-cached.txt"
    if [ "$(git rev-parse HEAD)" != "$(cd "$here/$format/dot-git" && git --git-dir=. rev-parse refs/heads/main 2>/dev/null || true)" ]; then
      git rev-parse HEAD > "$out/head.txt"
      git cat-file -p HEAD > "$out/commit.txt"
    fi
  )
  rm -rf "$work"
}

make() {
  format=$1
  scenario add-file main . git add README.md
  scenario add-dot main . git add .
  scenario add-update main . git add -u
  scenario add-subdir main src git add .
  scenario add-partial main . git add src/lib.rs
  scenario add-new main . git add new.txt newlink newdir
  scenario add-deleted main . git add data.bin
  scenario add-mode main . git add feature.txt
  scenario add-ignored main . git add debug.out build/o.bin
  scenario add-force main . git add -f debug.out
  scenario commit-all main . git commit -qam "commit all"
  scenario commit-staged main . git commit -qm "staged only"
  scenario restore-staged main . git restore --staged src/lib.rs staged.txt
  scenario linked-commit linked . sh -c 'git add feature.txt wt-only.txt && git commit -qm "in the worktree"'
}

make sha1
make sha256
