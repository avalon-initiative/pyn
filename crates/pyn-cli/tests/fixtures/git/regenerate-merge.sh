#!/usr/bin/env bash
# Regenerates the merge fixture with the real git binary, used only as an oracle: the tests never run git.
# Payload paths below are inside the archives listed in archives.txt; regeneration packs its temporary trees.
# merge/worktree is the work tree (clean, on `main`) and merge/<format>/dot-git the `.git` directory, with the
# branches the merge scenarios in scenarios-writes.txt use. Objects and refs are deterministic; the index carries
# stat data from the temporary work tree. Rerun only when the history changes.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
source "$here/archive-fixtures.sh"
export GIT_AUTHOR_NAME="Fixture Author" GIT_AUTHOR_EMAIL="author@example.com"
export GIT_COMMITTER_NAME="Fixture Committer" GIT_COMMITTER_EMAIL="committer@example.com"
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null GIT_CONFIG_NOSYSTEM=1
export GIT_PAGER=cat LC_ALL=C TZ=UTC

tick=1700000000
commit() {
  tick=$((tick + 86400))
  GIT_AUTHOR_DATE="$tick +0200" GIT_COMMITTER_DATE="$tick -0530" git commit -q -m "$1"
}

lines() { for i in $(seq 1 10); do echo "line $i"; done; }

scenario() {
  mkdir -p dir
  lines > a.txt
  printf 'bee\n' > b.txt
  printf 'doomed\n' > del.txt
  printf '#!/bin/sh\necho hi\n' > run.sh && chmod +x run.sh
  ln -s a.txt link
  printf 'x\n' > dir/x.txt
  git add -A && commit "base"
  git branch old

  git checkout -q -b clean
  sed -i 's/^line 9$/line nine clean/' a.txt
  printf 'clean\n' > clean-only.txt
  chmod +x b.txt
  rm link && ln -s b.txt link
  git add -A && commit "clean side"

  git checkout -q -b conflict main
  sed -i 's/^line 2$/line two theirs/' a.txt
  printf 'bee theirs\n' > b.txt
  git add -A && commit "conflicting side"

  git checkout -q -b moddel main
  git rm -q del.txt
  git add -A && commit "delete del.txt"

  git checkout -q -b addadd main
  printf 'theirs\n' > new.txt
  git add -A && commit "add new.txt"

  git checkout -q main
  sed -i 's/^line 2$/line two ours/' a.txt
  printf 'doomed and changed\n' > del.txt
  printf 'ours\n' > new.txt
  printf 'main\n' > main-only.txt
  git add -A && commit "main side"

  git checkout -q -b ff
  printf 'ahead\n' > ff.txt
  git add -A && commit "ahead of main"
  git checkout -q main
}

make() {
  local format=$1
  local root="$fixtures/merge/$format" work
  work="$(mktemp -d "$fixture_tmp/work.XXXXXX")"
  rm -rf "$root"; mkdir -p "$root"
  (
    cd "$work"
    git init -q --object-format="$format" -b main .
    git config core.autocrlf false
    scenario
    if [ "$format" = sha1 ]; then
      rm -rf "$fixtures/merge/worktree"; mkdir -p "$fixtures/merge/worktree"
      cp -a . "$fixtures/merge/worktree/"
      rm -rf "$fixtures/merge/worktree/.git"
    fi
    rm -rf .git/hooks .git/logs .git/description .git/COMMIT_EDITMSG .git/ORIG_HEAD .git/branches
    cp -r .git "$root/dot-git"
  )
  rm -rf "$work"
}

make sha1
make sha256

pack_fixtures
