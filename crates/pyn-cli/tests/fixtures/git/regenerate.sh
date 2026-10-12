#!/usr/bin/env bash
# Regenerates the git home fixtures with the real git binary, used only as an oracle: the tests never run git.
# worktree/ and linked/ are the work trees (every `.gitignore` is stored as `dot-gitignore` so it does not apply to
# this repository); <format>/dot-git is the `.git` directory and <format>/expected/ is what git prints for the
# commands the tests compare. Objects, refs and expected/ are deterministic; the index files carry stat data from
# the temporary work tree, so they differ per run. Rerun only when the scenario changes.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
export GIT_AUTHOR_NAME="Fixture Author" GIT_AUTHOR_EMAIL="author@example.com"
export GIT_COMMITTER_NAME="Fixture Committer" GIT_COMMITTER_EMAIL="committer@example.com"
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null GIT_CONFIG_NOSYSTEM=1
export GIT_PAGER=cat LC_ALL=C TZ=UTC

tick=1700000000
commit() {
  tick=$((tick + 86400))
  GIT_AUTHOR_DATE="$tick +0200" GIT_COMMITTER_DATE="$tick -0530" git commit -q -m "$1"
}

scenario() {
  mkdir -p src docs
  printf 'hello\n' > README.md
  printf 'fn main() {}\n' > src/main.rs
  printf 'pub fn f() {}\n' > src/lib.rs
  printf 'guide\n' > docs/guide.md
  printf '#!/bin/sh\necho hi\n' > run.sh && chmod +x run.sh
  ln -s README.md link
  printf '*.out\nbuild/\n!keep.out\n/root-only.txt\n' > .gitignore
  printf '*.tmp\n!important.tmp\n' > src/.gitignore
  git add -A && commit "initial commit"

  printf 'hello\nworld\n' > README.md
  printf 'fn main() { println!("v2"); }\n' > src/main.rs
  printf 'bin\0ary\n' > data.bin
  git add -A && commit $'second commit\n\nA body paragraph that\nspans two lines.\n\n    indented line'
  git tag light

  git checkout -q -b feature HEAD~1
  printf 'feature\n' > feature.txt
  git add -A && commit "feature work"
  git checkout -q main

  git rm -q docs/guide.md
  chmod +x src/lib.rs
  git add -A && commit "third commit"
  tick=$((tick + 86400))
  GIT_AUTHOR_DATE="$tick +0200" GIT_COMMITTER_DATE="$tick -0530" git merge -q --no-ff -m "Merge branch 'feature'" feature
  GIT_COMMITTER_DATE="$tick +0000" git tag -a -m "release one" v1

  git worktree add -q "$wt" feature
}

# Staged, unstaged, deleted, mode-only, untracked, ignored and negated files in the main work tree.
dirty_main() {
  printf 'staged\n' > staged.txt && git add staged.txt
  printf 'pub fn f() { 1 }\n' > src/lib.rs && git add src/lib.rs
  printf 'pub fn f() { 2 }\n' > src/lib.rs
  printf 'hello\nworld\nmore\n' > README.md
  rm data.bin
  chmod +x feature.txt
  printf 'new\n' > new.txt
  mkdir -p newdir sub build private
  printf 'a\n' > newdir/a.txt && printf 'b\n' > newdir/b.txt
  printf 'x\n' > src/important.tmp && printf 'x\n' > src/cache.tmp
  printf 'x\n' > keep.out && printf 'x\n' > debug.out
  printf 'x\n' > root-only.txt && printf 'x\n' > sub/root-only.txt
  printf 'x\n' > build/o.bin
  printf 'x\n' > private/notes.txt
  ln -s new.txt newlink
}

dirty_linked() {
  printf 'feature edit\n' > feature.txt
  printf 'only here\n' > wt-only.txt
}

capture() {
  local out=$1
  mkdir -p "$out"
  git status --porcelain=v1 > "$out/status.txt"
  git log > "$out/log.txt"
  git log -n 2 > "$out/log-n2.txt"
  git log -- README.md > "$out/log-readme.txt"
  git log feature > "$out/log-feature.txt"
  git log --format='%H' > "$out/commits.txt"
  git show HEAD~1 > "$out/show-third.txt"
  git show HEAD~3 > "$out/show-initial.txt"
  git show HEAD^2 > "$out/show-feature.txt"
  git show HEAD > "$out/show-head.txt"
  git show v1~2 > "$out/show-tag-step.txt"
  git diff > "$out/diff.txt"
  git diff --stat > "$out/diff-stat.txt"
  git diff --name-status > "$out/diff-name-status.txt"
  git diff HEAD > "$out/diff-head.txt"
  git diff HEAD~3 HEAD~2 > "$out/diff-commits.txt"
  git diff light v1 --name-status > "$out/diff-commits-name-status.txt"
  git diff HEAD~3..HEAD~2 -- src > "$out/diff-range-path.txt"
  git diff -U1 HEAD~3 HEAD~2 > "$out/diff-u1.txt"
  git diff --stat HEAD~2 HEAD > "$out/diff-stat-commits.txt"
  git show --stat HEAD~1 > "$out/show-stat-third.txt"
  git diff --name-only HEAD > "$out/diff-name-only-head.txt"
}

make() {
  local format=$1
  local root="$here/$format" work wt
  work="$(mktemp -d)"; wt="$work-wt/linked"; mkdir -p "$work-wt"
  rm -rf "$root"; mkdir -p "$root"
  (
    cd "$work"
    git init -q --object-format="$format" -b main .
    git config core.autocrlf false
    scenario
    printf 'private/\n' > .git/info/exclude
    (cd "$wt" && dirty_linked)
    dirty_main
    mkdir -p "$root/expected/linked"
    (cd "$wt" && git status --porcelain=v1 > "$root/expected/linked/status.txt" && git diff > "$root/expected/linked/diff.txt")
    capture "$root/expected"
    if [ "$format" = sha1 ]; then
      rm -rf "$here/worktree" "$here/linked"
      mkdir -p "$here/worktree" "$here/linked"
      cp -a . "$here/worktree/"
      rm -rf "$here/worktree/.git"
      cp -a "$wt/." "$here/linked/"
      rm -f "$here/linked/.git"
      while IFS= read -r f; do mv "$f" "$(dirname "$f")/dot-gitignore"; done < <(find "$here/worktree" "$here/linked" -name .gitignore)
    fi
    echo /fixture/linked/.git > .git/worktrees/linked/gitdir
    rm -rf .git/hooks .git/logs .git/description .git/COMMIT_EDITMSG .git/ORIG_HEAD .git/branches .git/worktrees/*/logs
    cp -r .git "$root/dot-git"
  )
  rm -rf "$work" "$work-wt"
}

make sha1
make sha256
