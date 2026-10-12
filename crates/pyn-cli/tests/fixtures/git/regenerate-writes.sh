#!/usr/bin/env bash
# Regenerates <format>/expected/writes/ with the real git binary, used only as an oracle: the tests never run git.
# Every line of scenarios-writes.txt starts from a fresh copy of the committed fixtures (laid out as
# tests/git_writes.rs lays them out), runs the commands with git, and records what is left on disk. The same file
# drives the pyn run in the tests. Rerun only when the scenarios change.
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
  local format=$1 fixture=$2 dest=$3
  mkdir -p "$dest/main"
  if [ "$fixture" = merge ]; then
    copy_tree "$here/merge/worktree" "$dest/main"
    cp -a "$here/merge/$format/dot-git" "$dest/main/.git"
    return
  fi
  mkdir -p "$dest/linked"
  copy_tree "$here/worktree" "$dest/main"
  cp -a "$here/$format/dot-git" "$dest/main/.git"
  copy_tree "$here/linked" "$dest/linked"
  echo "gitdir: $dest/main/.git/worktrees/linked" > "$dest/linked/.git"
  echo "$dest/linked/.git" > "$dest/main/.git/worktrees/linked/gitdir"
}

dump() {
  local out=$1 gitdir common
  gitdir="$(git rev-parse --absolute-git-dir)"
  common="$(cd "$(git rev-parse --git-common-dir)" && pwd)"
  mkdir -p "$out"
  { git for-each-ref --format='%(refname) %(objectname)'; echo "HEAD $(cat "$gitdir/HEAD")"; } > "$out/refs.txt"
  git ls-files -s > "$out/ls-files.txt"
  git status --porcelain=v1 > "$out/status.txt"
  find . \( -name .git -prune \) -o \( -type f -o -type l \) -print | sort | while IFS= read -r f; do
    if [ -L "$f" ]; then echo "$f -> $(readlink "$f")"; else echo "$f $(stat -c %a "$f") $(sha256sum < "$f" | cut -d' ' -f1)"; fi
  done > "$out/tree.txt"
  ( cd "$common"; { find logs worktrees/*/logs -type f 2>/dev/null || true; } | sort | while IFS= read -r f; do echo "== $f"; cat "$f"; done ) > "$out/logs.txt"
  { for f in MERGE_HEAD MERGE_MSG MERGE_MODE ORIG_HEAD; do [ -f "$gitdir/$f" ] && { echo "== $f"; cat "$gitdir/$f"; }; done; true; } > "$out/state.txt"
  {
    echo "== HEAD"; git cat-file -p HEAD 2>/dev/null || true
    for r in $(git for-each-ref --format='%(refname)' refs/tags refs/stash); do echo "== $r"; git cat-file -p "$r"; done
  } > "$out/objects.txt"
  cp "$common/config" "$out/config.txt"
}

scenario() {
  local format=$1 name=$2 fixture=$3 tree=$4 sub=$5 cmds=$6
  local work out code=0
  work="$(mktemp -d)"
  setup "$format" "$fixture" "$work"
  out="$here/$format/expected/writes/$name"
  [ "$fixture" = merge ] && out="$here/merge/$format/expected/writes/$name"
  rm -rf "$out"; mkdir -p "$out"
  (
    cd "$work/$tree/$sub"
    : > "$out/stdout.txt"
    IFS=$'\n'
    for c in $(printf '%s' "$cmds" | sed 's/ ;; /\n/g'); do
      unset IFS
      read -r -a argv <<< "$c"
      argv=("${argv[@]//+/ }")
      code=0
      if [ "${argv[0]}" = "@write" ]; then
        printf "${argv[2]}" > "${argv[1]}"
      elif [ "${argv[0]}" = "@resume" ]; then
        git commit -q --no-edit > "$out/stdout.txt" 2>/dev/null || code=$?
      else
        git -c merge.conflictStyle=diff3 -c advice.detachedHead=false "${argv[@]}" > "$out/stdout.txt" 2>/dev/null || code=$?
      fi
      IFS=$'\n'
    done
    unset IFS
    echo "$code" > "$out/exit.txt"
    cd "$work/$tree"
    dump "$out"
  )
  rm -rf "$work"
}

for format in sha1 sha256; do
  while IFS=$'\t' read -r name fixture tree sub cmds; do
    case "$name" in ''|'#'*) continue ;; esac
    scenario "$format" "$name" "$fixture" "$tree" "$sub" "$cmds"
  done < "$here/scenarios-writes.txt"
done
