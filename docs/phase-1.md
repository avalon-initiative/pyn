# Phase 1 scope

Prove one thing: a team can safely collaborate on a repository containing both mergeable source and exclusive assets.

Phase 1 is complete. Scope: repository rules, shared and exclusive paths, lock, unlock, commit, leased locks with
expiry, per-path history (`pyn log`), restore, on a single mainline per repository.

Built since, beyond Phase 1: many repositories per server ([repositories](concepts/repositories.md)), accounts,
organizations and teams, OpenID Connect sign-in, persistent and chunked content storage, commits, diff and show,
tamper-evident history, and the web UI.

Still ahead: branches, merge and conflict handling, remotes, pull requests, reviews, CI, Git import/export, and engine
and editor integrations. The git-style commands not yet built are listed as *planned* in
[git compatibility](concepts/git-compatibility.md).
