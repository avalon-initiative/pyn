# Phase 1 scope

Prove one thing: a team can safely collaborate on a repository containing both mergeable source and exclusive assets.

In scope: repository rules, shared and exclusive paths, lock, unlock, checkin, leased locks with expiry, per-path
history (`pyn log`), restore. A single mainline per repository; in-memory storage until the Postgres and token work
landed. Phase 1 was delivered with one repository per server; a server now hosts many, see
[repositories](concepts/repositories.md).

Out of scope until later phases: branches, workspaces, changesets, merge and conflict handling, pull requests,
reviews, CI, organizations, SSO, Git import/export, engine and editor integrations, large-file chunking.
