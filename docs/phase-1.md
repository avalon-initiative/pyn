# Phase 1 scope

Prove one thing: a team can safely collaborate on a repository containing both mergeable source and exclusive assets.

In scope: repository rules, shared and exclusive paths, checkout, checkin, leased locks with expiry, per-path
history, restore. A single mainline; one repository per server; in-memory storage and a development auth header
until the Postgres and token work lands.

Out of scope until later phases: branches, workspaces, changesets, merge and conflict handling, pull requests,
reviews, CI, organizations, SSO, Git import/export, engine and editor integrations, large-file chunking.
