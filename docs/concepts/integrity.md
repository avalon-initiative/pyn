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

**Revision v3** is the revision of a deletion: exactly the v2 fields in the same order under the tag `pyn-revision-v3`, where
`file_mode` and `blob_id` are those of the version the deletion removed and `previous` is the hash of the path's last revision.
The tag alone tells it apart from v2, so no field is added and the v2 encoding is untouched.

A v2 hash covers the commit id, and the commit id covers the tree and the parent, so the revision, its commit and the
repository state at that commit cannot be changed independently. Which encoding a revision uses is stored with it as `hash_version` (`1`, `2` or `3`;
`revisions.hash_version` in PostgreSQL, `hash_version` in the API), never inferred. Rows that existed before v2 are version 1 and keep their hash, never
rewritten; new revisions are version 2, or version 3 when they delete the path. A version must agree with the stored fields: versions 2 and 3 need the structured
author (`revisions.author_name`, `author_email` and `author_offset` in PostgreSQL, `author_identity` in the API),
version 1 must not have one, and version 3 is exactly the revisions with `deleted` set (`revisions.deleted`, `deleted` in the API). A disagreement, or a version that names no known encoding, is corrupt history: it is never re-encoded to
make it fit.

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
| v3 revision (a deletion) with the fields of the first v2 row | `081218b2d39da72996468114f21b3281bc73fdaddba7118b06d189d7e8c4ad8d` |

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
events leaves a valid chain. A client that remembers a head hash can detect that; a signed commit proves authorship (see [signing](git-compatibility.md#signing)).

## Where hashes are checked

- Every revision the server returns is checked against its own hash first. The full history of a path is also checked for
  its links. A mismatch, or a `hash_version` that disagrees with the stored fields, is a `500 history_corrupt`, never silently served. The
  response carries a generic message; the path and revision are in the server log.
- A commit chains from the stored head hash only after the head has been checked, so a tampered head cannot be extended.
- Every commit the server returns is checked against its own id: its stored raw bytes must hash to the id and say what
  its tree, parents, author, committer, time and message fields say; a mismatch is a `500 history_corrupt`. Verification also checks each commit's parent links and that each
  branch head's tree is the tree of the snapshot at that head, and reports `hash_mismatch`, `broken_link` and `tree_mismatch`
  problems with the `commit` id. A revision whose `hash_version` disagrees with its stored fields is reported as
  `version_mismatch`. A revision whose `base` is not an older revision of the same path, or, for an exclusive revision on any
  branch, is not the path's pointer at the first parent of its commit ([branches](branches.md)), is `base_mismatch`. The base is
  stored beside the revision and is not covered by the hash, so existing hashes stay valid. A commit whose pointer cache lacks a
  revision it made, repeats a path, or holds a revision that is neither its own nor, for a merge, a newer one from the other
  parents' history, is `pointer_mismatch`, reported with the `commit` id.
- Audit events are checked when they are read, and the whole chain when verified.
- `GET /v1/repos/{owner}/{name}/verify?path=` (the form without `path` needs a signed-in caller) checks a path, or every path and the repository's audit log, and reports every
  break instead of stopping at the first. It needs `read`; the audit log is checked only for callers with `view_audit`.
- **Steps.** One request examines at most 20,000 records (revisions, commits, audit events, and the files of a branch head whose tree
  it rebuilds). With `paged=true` a repository that needs more answers with the share it checked and a `next_cursor`: add the steps'
  counts and problems together and send the cursor back as `cursor` until `next_cursor` is null. The steps run in order: revisions
  path by path (a path with a long history is itself split), commits, each branch head's tree, then the audit log. Without `paged`, a
  repository that needs more than one step is `400 invalid_request`, so a client that does not know about steps never takes a partial
  report for a complete one. A cursor that does not parse, or names another path than `path`, is `400 invalid_request`. The tree check
  of one branch head cannot be split, because a git tree id covers every file; it is charged against the budget by file count and
  ends the step when the budget is spent.
- `pyn verify [<path>]` follows the steps until the server is done, prints the report and exits 1 when anything is broken. It also selects each revision's encoding from
  `hash_version`, recomputes the revision hashes (v1, v2 and v3) and the commit ids it fetches on the client, so a server that reports "ok" cannot hide a mismatch.

Revisions in the API carry `hash`, `hash_version`, `previous` and `base`; audit events carry `hash` and `previous`.

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
cannot hold. The default [branch](branches.md#existing-data) of each repository is then created at the newest commit.

A database from before hashing is hashed once at startup: revisions in order of number within each path, audit events in
order of id within each log. The hashes cover the history as it stands then, so they prove nothing about what happened before
that moment. The step is idempotent and skips anything already hashed.

## Commits

A commit's id is its git commit id (see [commits](commits.md)). Commits are named by hash, with
`HEAD~N` and `^`. Each commit's `parents` link it to the commit it was made on, so every branch is a hash chain too, and the
id covers the tree, so it covers every path's content and file mode as of that commit. A merge commit lists both parents, and
the revisions it writes (a merged text, a resolution) cover its id like any other. Verification checks every commit of
every branch, that each commit's parents are older commits of the repository, and each branch head's tree against the
snapshot at that head.
