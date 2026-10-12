#!/usr/bin/env bash
# Regenerates the fixture repositories under this directory with the real git binary, used only as an oracle.
# The tests never run git. Objects, refs and expected/ files are deterministic (fixed identity and dates); the
# index files carry stat data from the temporary work tree, so they differ per run. Rerun only when a fixture changes.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
export GIT_AUTHOR_NAME="Fixture Author" GIT_AUTHOR_EMAIL="author@example.com"
export GIT_COMMITTER_NAME="Fixture Committer" GIT_COMMITTER_EMAIL="committer@example.com"
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null GIT_CONFIG_NOSYSTEM=1
export LC_ALL=C

tick=1700000000
commit() {
  tick=$((tick + 86400))
  GIT_AUTHOR_DATE="$tick +0200" GIT_COMMITTER_DATE="$tick -0530" git commit -q -m "$1"
}

# A repository with three commits, files in subdirectories, an executable, a symlink and a growing text file
# (so a repack produces deltas).
populate() {
  mkdir -p src/deep docs
  seq 1 400 | sed 's/^/line /' > docs/log.txt
  printf 'fn main() {}\n' > src/main.rs
  printf '#!/bin/sh\necho hi\n' > run.sh && chmod +x run.sh
  printf 'deep\n' > src/deep/mod.rs
  ln -s src/main.rs link
  git add -A && commit "first"
  seq 1 400 | sed 's/^/line /' > docs/log.txt && echo "line 401 added" >> docs/log.txt
  printf 'fn main() { println!("v2"); }\n' > src/main.rs
  git add -A && commit "second"
  echo "line 402 added" >> docs/log.txt
  git rm -q src/deep/mod.rs
  printf 'new file\n' > docs/new.md
  git add -A && commit "third"
}

# name format pack(0|1|ref) index-version
make() {
  local name=$1 format=$2 mode=$3 iv=$4
  local root="$here/$name" work
  rm -rf "$root"; mkdir -p "$root"
  work="$(mktemp -d)"; mkdir -p "$work-wt"
  (
    cd "$work"
    git init -q --object-format="$format" -b main .
    git config core.autocrlf false
    populate
    git tag light-tag
    GIT_COMMITTER_DATE="1700999999 +0000" git tag -a -m "annotated" v1.0
    git branch side HEAD~1
    git update-index --index-version "$iv"
    case $mode in
      loose) ;;
      pack) git repack -adq -f --depth=50 --window=50 && git pack-refs --all && git prune-packed ;;
      ref) git -c repack.useDeltaBaseOffset=false repack -adq -f --depth=50 --window=50 && git prune-packed ;;
    esac
    if [ "$mode" = ref ]; then git checkout -q --detach; fi
    git update-index --index-version "$iv"
    rm -rf .git/hooks .git/logs .git/info/exclude .git/description .git/COMMIT_EDITMSG .git/ORIG_HEAD
    mkdir -p "$root/expected"
    git cat-file --batch-all-objects --batch-check='%(objectname) %(objecttype) %(objectsize)' | sort > "$root/expected/objects.txt"
    git for-each-ref --format='%(objectname) %(refname) %(*objectname)' | sort > "$root/expected/refs.txt"
    if git symbolic-ref -q HEAD > "$root/expected/head.txt"; then :; else git rev-parse HEAD > "$root/expected/head.txt"; fi
    git ls-files -s | sort -k4 > "$root/expected/index.txt"
    echo "$iv" > "$root/expected/index-version.txt"
    for p in .git/objects/pack/*.pack; do
      [ -e "$p" ] && git verify-pack -v "$p" | grep -E '^[0-9a-f]{40,64} ' > "$root/expected/pack.txt" || true
    done
    if [ "$name" = sha1-loose ]; then
      # A linked worktree; the test rebuilds its `.git` file, so only the metadata under worktrees/ is kept.
      git worktree add -q -b wt-branch "$work-wt/wt1" HEAD~1
      git for-each-ref --format='%(objectname) %(refname) %(*objectname)' | sort > "$root/expected/refs.txt"
      echo /fixture/wt1/.git > .git/worktrees/wt1/gitdir
    fi
    rm -rf .git/logs .git/branches .git/worktrees/*/logs
    cp -r .git "$root/dot-git"
  )
  rm -rf "$work" "$work-wt"
}

make sha1-loose sha1 loose 2
make sha1-packed sha1 pack 4
make sha1-refdelta sha1 ref 2
make sha256-loose sha256 loose 4
make sha256-packed sha256 pack 2
