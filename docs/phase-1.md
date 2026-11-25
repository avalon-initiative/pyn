# Phase 1 scope and what came after

A history page: what Phase 1 set out to prove and what has landed since. Open work is tracked in the repository's issues, and the
planned git-style commands are the *planned* rows of [git compatibility](concepts/git-compatibility.md).

Prove one thing: a team can safely collaborate on a repository containing both mergeable source and exclusive assets.

Phase 1 is complete. Scope: repository rules, shared and exclusive paths, lock, unlock, commit, leased locks with
expiry, per-path history (`pyn log`), restore, on a single mainline per repository.

Built since, beyond Phase 1: many repositories per server ([repositories](concepts/repositories.md)), accounts,
organizations and teams, OpenID Connect sign-in, persistent and chunked content storage, commits, diff and show,
tamper-evident history, and the web UI.

Branches (create, list, rename, delete, commit on a branch, switch) and merge ([merging](concepts/merging.md): merge base, merge
commits, conflicts and their resolution), per-branch policy, remotes, tags and git import and export have landed
([branches](concepts/branches.md)). Since then: [pull requests](concepts/pull-requests.md) with lock-aware merging,
[diffs](concepts/diff.md) with lock state per path, forks, stars and watching ([repositories](concepts/repositories.md)).
Still ahead: pull request reviews, CI, and engine and editor integrations. The
git-style commands not yet built are listed in [git compatibility](concepts/git-compatibility.md).
