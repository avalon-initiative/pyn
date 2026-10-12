# Tamper-evident history

Decision #65. Every revision carries a hash that covers its own fields and the hash of the previous revision of the same
path, and every audit event carries a hash that covers its fields and the hash of the event before it. Altering or removing
anything in the past changes every later hash, so the break can be found. These hashes are pyn's own, not git's; a
[commit](commits.md) is identified by git's own commit id, which revision v2 covers.

## Encoding (frozen)

Both hashes are SHA-256 over a byte string built from these primitives and written as 64 lowercase hex digits. Only the
primitives below are used, so the encoding is unambiguous and needs no escaping.

| Primitive | Bytes |
| --- | --- |
| `str(s)` | the length of `s` in UTF-8 bytes as an unsigned 64-bit big-endian integer, then the UTF-8 bytes |
| `opt_str(s)` | `00` when absent, else `01` followed by `str(s)` |
| `opt_u64(n)` | `00` when absent, else `01` followed by `n` as 8 big-endian bytes |
| `time(t)` | microseconds since the Unix epoch, as a signed 64-bit big-endian integer (sub-microsecond digits are dropped) |

**Revision**, in this order:

```text
str("pyn-revision-v1")
str(path)               repository-relative path, as stored
str(content)            the content string: the git blob id hex for revisions made since decision #245,
                        the SHA-256 hex of the bare content for revisions made before (see below)
opt_str(previous)       hash of the path's previous revision; absent for the first
str(author)
time(created_at)
str(message)
opt_u64(restored_from)  the number of the revision restored from; absent unless the revision is a restore
```

**Revision v2** (decision #245, made since the commit record landed), in this order:

```text
str("pyn-revision-v2")
str(path)
str(file_mode)          git's octal mode: 100644, 100755 or 120000
str(blob_algorithm)     the blob id's hash algorithm: sha256 (sha1 for a git home)
str(blob_id)            the content's git blob id, lowercase hex
opt_str(previous)       hash of the path's previous revision (v1 or v2); absent for the first
str(user)               the author's account name
str(author_name)        the structured author: name and email as written into the commit
str(author_email)
time(created_at)
i32(author_offset)      minutes east of UTC as a signed 32-bit big-endian integer
str(message)
opt_u64(restored_from)
str(commit_algorithm)
str(commit_id)          the git commit id of the commit that made the revision, lowercase hex
```

A v2 hash covers the commit id, and the commit id covers the tree and the parent, so the revision, its commit and the
repository state at that commit cannot be changed independently. A revision is v2 exactly when its structured author is
stored with it (`revisions.author_name`, `author_email` and `author_offset` in PostgreSQL, `author_identity` in the API);
rows without it keep their v1 hash and are never rewritten. New revisions are always v2.

**Audit event**, in this order:

```text
str("pyn-audit-v1")
str(scope)              the log's storage key: `owner/name`, `@org:<name>` or `@server`
opt_str(previous)       hash of the preceding event in the same log; absent for the first
time(at)
str(actor)
str(action)             the action's snake_case name
opt_str(path)
str(detail)
```

The first byte string of each encoding is a domain tag, so a revision hash can never equal an audit hash. The encoding is
part of the contract: a change to any byte needs a new tag (`...-v2`) and a new column, never an edit of these.

Test vectors (also asserted in `crates/pyn-hash`). v2 first, with blob id `473a0f4c...1813` (the empty blob, SHA-256),
user `alice`, author `Alice <alice@example.com>`, time `1700000000123456` and commit id `ab` repeated 32 times:

| Input | Hash |
| --- | --- |
| v2 revision `Content/a.umap`, mode `100644`, no previous, offset 0, message `first` | `5b9cb79dd9a6deb197f72c532eb669b72d1e0820c6a8cb56bbc5d0fc838d52ed` |
| the same with mode `100755`, previous set to the hash above, name `Alice Liddell`, offset -330, message `restore`, restored from 1, commit id `cd` repeated 32 times | `3d2323ad77d4713b6667982fe3f18a4613bdd068a56c023af00a61caa7e5bbe8` |

v1 and audit vectors, with content string `e3b0c442...b855` (the SHA-256 of the empty file, as stored before blob ids), author `alice`
and time `1700000000123456`:

| Input | Hash |
| --- | --- |
| revision `Content/a.umap`, no previous, message `first` | `9c4caa53e94d5fa8fb261cb765f73725506641df99e225605e83f179ae0d9b52` |
| the same with previous set to the hash above, message `restore`, restored from 1 | `858c075aa1d8e6e9d624aaf0c3cdc032c3e4a165a809ff4a38d6f49ba84e0790` |
| audit scope `alice/game`, no previous, actor `alice`, action `checkout`, path `Content/a.umap`, detail `lease 8h` | `4faf45d2108cab101ce1e890a016c314cfde33485307de1eace154d4e0edef8a` |
| audit scope `alice/game`, previous the hash above, time `1700000001000000`, actor `bob`, action `repo_updated`, no path, empty detail | `ffb456bf011e957ddb3aafa9a4b301b822e22a3029cf8a52ee91c432ddfdc6d9` |

## What is covered, and what is not

Covered: path, content hash, previous hash, author, time, message and restore source (revisions, v1); v2 adds the git
file mode, the structured author, the blob id with its algorithm and the commit id; scope, previous hash, time, actor,
action, path and detail (audit events).

Not covered by the revision hash, on purpose: the per-path revision number (its position in the chain fixes it), the
recorded `shared` or `exclusive` mode (`.pyn/pyn.toml` at the commit decides it, and the commit id covers that file) and the
changeset id. A [commit](commits.md) is identified by its git commit id over its parents, its tree (every path's blob id and
file mode) and its author, committer and message. Revisions made before v2 are not tied to their commit by their own hash,
only by the commit record that lists them; the migrated commits themselves are covered from the moment they were created.

A hash chain shows that what is there is consistent, not that nothing was cut off the end: removing the newest revisions or
events leaves a valid chain. A client that remembers a head hash can detect that; signed revisions (#60) will prove authorship.

## Where hashes are checked

- Every revision the server returns is checked against its own hash first. The full history of a path is also checked for
  its links. A mismatch is a `500 history_corrupt` naming the path and revision, never silently served.
- A commit chains from the stored head hash only after the head has been checked, so a tampered head cannot be extended.
- Every commit the server returns is checked against its own id (recomputed from its tree, parents, author, committer
  and message); a mismatch is a `500 history_corrupt`. Verification also checks each commit's parent link and that the
  head commit's tree is the tree of the head snapshot, and reports `hash_mismatch`, `broken_link` and `tree_mismatch`
  problems with the `commit` id.
- Audit events are checked when they are read, and the whole chain when verified.
- `GET /v1/repos/{owner}/{name}/verify?path=` checks a path, or every path and the repository's audit log, and reports every
  break instead of stopping at the first. It needs `read`; the audit log is checked only for callers with `view_audit`.
- `pyn verify [<path>]` prints the report and exits 1 when anything is broken. It also recomputes the revision hashes (v1 and v2) and the commit ids it
  fetches on the client, so a server that reports "ok" cannot hide a mismatch.

Revisions in the API carry `hash` and `previous`; audit events carry `hash` and `previous`.

## Content ids and v1

Decision #245 changed the content id from the SHA-256 of the bytes to the git blob id. The v1 encoding is not touched:
`str(content)` covers whatever string the revision was hashed with. Revisions made after the change cover the blob id hex.
Revisions that existed before it keep a stored copy of the string they were hashed with (`revisions.content_v1` in
PostgreSQL, `legacy_content` in the API), so their hashes still reproduce; `pyn verify` and the server use that string
in place of `content` when it is present. A revision whose `legacy_content` is stripped no longer verifies. Revision v2
covers the blob id and its algorithm explicitly.

## Existing data

History that predates the commit record is migrated once at startup (`migrate_history`, after the content ids are
re-keyed): each old commit number becomes a commit on a single mainline in the same order, with the previous one as its
parent, the author `<user> <<user>@pyn.invalid>` at offset 0, the first revision's message, the whole second as its time,
and the git tree of the repository as of that number (every file as `100644`, since modes were not recorded). The
revisions keep their v1 hashes, which still verify, and are pointed at their commit. The step runs in one transaction,
skips anything already migrated and refuses a repository that has both a file `a` and a path under `a/`, which a git tree
cannot hold.

A database from before hashing is hashed once at startup: revisions in order of number within each path, audit events in
order of id within each log. The hashes cover the history as it stands then, so they prove nothing about what happened before
that moment. The step is idempotent and skips anything already hashed.

## Commits

A commit's id is its git commit id (see [commits](commits.md)). `cN` numbers are gone: commits are named by hash, with
`HEAD~N` and `^`. Each commit's `parents` link it to the one before, so the mainline is a hash chain too, and the id covers
the tree, so it covers every path's content and file mode as of that commit.
