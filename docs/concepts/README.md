# Concepts

pyn separates three things that most version control systems fuse together. Each exists per [repository](repositories.md): a
server hosts many, addressed as `owner/name`.

1. **File content history.** Every path has immutable, numbered revisions (1, 2, 3, ...). A restore appends a new
   revision with old content; nothing is ever rewritten.
2. **Collaboration policy.** Each path is `shared` (git-style: concurrent edits are allowed and merged later) or
   `exclusive` (one editor at a time, enforced by the server). Policy is decided by [`pyn.toml`](pyn-toml.md) and is
   not a file-type classification: a text file can be exclusive and a binary can be shared.
3. **Branch state.** A branch records which revision of each path it references. An exclusive file is still part of
   branch history; the lock only controls who may modify it *right now*.

## Exclusive files and locks

Exclusive files are materialized **read-only** on disk. To edit one, a user runs `pyn lock <path>`; the server
grants a **leased lock** (8 hours by default) if nobody else holds it and the user's copy is at the current head.
`pyn checkin` uploads the new content, creates the next revision and releases the lock; `pyn unlock <path>` gives the lock up without a checkin.

The server is the only enforcer. A checkin of an exclusive path is rejected unless the caller holds a live lock *and*
the base revision equals the current head, so bypassing the CLI (uploading directly) cannot skip the lock. The
read-only bit on disk is a convenience that stops editors from silently making unshippable edits.

Locks expire so abandoned work cannot block a file forever. The same holder locking again renews the lease. One user may hold only a limited number of locks in a repository (5
by default); see [the lock limit](pyn-toml.md#lock-limit).

## Force unlock and the audit log

Locks expire, but sometimes one has to be removed at once. `pyn unlock --force --reason "..." <path>` removes someone else's live
lock. It needs the `force_unlock` permission, and the reason is mandatory because it is recorded.

The audit log is an append-only record of lock, revision and access events: checkout, release, checkin, restore, force unlock,
member added, role changed, role permissions changed, token created, token revoked, and repository created, updated and deleted, each with who, when, the path and a short detail (for a force unlock, whose lock was removed and why). `pyn audit` shows it
newest first and can filter by path, user and action; it needs the `view_audit` permission. Events are never changed or
removed.

Access events have no path: the detail names the target and what changed (a user and their old and new role, the
permissions a role gained or lost, a token's id, name, owner, permissions and expiry). Token secrets are never recorded.
Sign-in sessions are not logged. The log is kept per repository, and a token's events appear in the log of each repository it is limited to. See [access](access.md).

## Looking at and restoring older versions

Any revision of a file can be fetched read-only, without a lock: `pyn get <path> --rev N`.

Making an older revision the working version again is a **restore**, and it is guarded because it changes the head for
everyone. `pyn restore <path> <revision>` needs the live lock on the path, the `restore` permission, and a typed
confirmation of the head being replaced. The server enforces all three; the confirmation is sent as `<path>@r<head>`, so a
script cannot restore by accident.

A restore appends a new revision on the same line. If the history is `a, b, c` and `b` is restored, the new head `d` has
`b`'s content (the same stored object, not a copy) and records `restored_from = b`. Nothing is removed: `c` stays in
history, a mistaken restore can itself be undone by restoring `c`, and further edits build on `d`. `pyn log` shows
restores.

## Exclusive files and branches

A lock covers a path across every branch. An exclusive path does not branch: it has one linear history shared by all
branches. A branch (and each commit) records a pointer to a revision of each exclusive path, which is how a past state of
the whole project is shown. Rolling a branch back moves its pointers only; the file's own history and head do not change.
Merging branches takes the later pointer, and since edits are serialized by the lock there is no content conflict.

## Shared files, in Phase 1

Shared files follow git's workflow once branches and merge exist (Phase 2), built in house with no dependency on git.
Until then a shared checkin must also be based on the current head, so a stale edit is rejected rather than silently
overwriting someone else's work.

## Terminology

- **Path**: repo-relative, `/`-separated, no `.`/`..` segments.
- **Revision**: one immutable version of one path, numbered per path.
- **Lease**: how long a lock lasts before it expires on its own.
- **Base revision**: the revision a client's copy was taken from; sent with lock and checkin.
