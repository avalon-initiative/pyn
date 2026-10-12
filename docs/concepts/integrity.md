# Tamper-evident history

Decision #65. Every revision carries a hash that covers its own fields and the hash of the previous revision of the same
path, and every audit event carries a hash that covers its fields and the hash of the event before it. Altering or removing
anything in the past changes every later hash, so the break can be found. Hashes are pyn's own, not git's.

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

Test vectors (also asserted in `crates/pyn-hash`), with content string `e3b0c442...b855` (the SHA-256 of the empty file, as stored before blob ids), author `alice`
and time `1700000000123456`:

| Input | Hash |
| --- | --- |
| revision `Content/a.umap`, no previous, message `first` | `9c4caa53e94d5fa8fb261cb765f73725506641df99e225605e83f179ae0d9b52` |
| the same with previous set to the hash above, message `restore`, restored from 1 | `858c075aa1d8e6e9d624aaf0c3cdc032c3e4a165a809ff4a38d6f49ba84e0790` |
| audit scope `alice/game`, no previous, actor `alice`, action `checkout`, path `Content/a.umap`, detail `lease 8h` | `4faf45d2108cab101ce1e890a016c314cfde33485307de1eace154d4e0edef8a` |
| audit scope `alice/game`, previous the hash above, time `1700000001000000`, actor `bob`, action `repo_updated`, no path, empty detail | `ffb456bf011e957ddb3aafa9a4b301b822e22a3029cf8a52ee91c432ddfdc6d9` |

## What is covered, and what is not

Covered: path, content hash, previous hash, author, time, message and restore source (revisions); scope, previous hash,
time, actor, action, path and detail (audit events).

Not covered, on purpose: the per-path revision number (its position in the chain fixes it), the recorded `mode`, the
changeset id and the commit number. A [commit](commits.md) will be identified by its own hash over its parents and its tree,
with the mode recorded per tree entry (decisions #65 and #233), which covers these. Until commit hashes exist, grouping of
revisions into commits and the recorded mode are not tamper-evident. Provisional.

A hash chain shows that what is there is consistent, not that nothing was cut off the end: removing the newest revisions or
events leaves a valid chain. A client that remembers a head hash can detect that; signed revisions (#60) will prove authorship.

## Where hashes are checked

- Every revision the server returns is checked against its own hash first. The full history of a path is also checked for
  its links. A mismatch is a `500 history_corrupt` naming the path and revision, never silently served.
- A commit chains from the stored head hash only after the head has been checked, so a tampered head cannot be extended.
- Audit events are checked when they are read, and the whole chain when verified.
- `GET /v1/repos/{owner}/{name}/verify?path=` checks a path, or every path and the repository's audit log, and reports every
  break instead of stopping at the first. It needs `read`; the audit log is checked only for callers with `view_audit`.
- `pyn verify [<path>]` prints the report and exits 1 when anything is broken. It also recomputes the revision hashes it
  fetches on the client, so a server that reports "ok" cannot hide a mismatch.

Revisions in the API carry `hash` and `previous`; audit events carry `hash` and `previous`.

## Content ids and v1

Decision #245 changed the content id from the SHA-256 of the bytes to the git blob id. The v1 encoding is not touched:
`str(content)` covers whatever string the revision was hashed with. Revisions made after the change cover the blob id hex.
Revisions that existed before it keep a stored copy of the string they were hashed with (`revisions.content_v1` in
PostgreSQL, `legacy_content` in the API), so their hashes still reproduce; `pyn verify` and the server use that string
in place of `content` when it is present. A revision whose `legacy_content` is stripped no longer verifies. Revision v2
(which covers the blob id and algorithm explicitly) is planned with the commit record.

## Existing data

A database from before hashing is hashed once at startup: revisions in order of number within each path, audit events in
order of id within each log. The hashes cover the history as it stands then, so they prove nothing about what happened before
that moment. The step is idempotent and skips anything already hashed.

## Commit numbers

Commits keep their `cN` numbers as per-repository aliases on the mainline (display, `HEAD~N`). When commit hashes land with
branches (#205), a commit is named by its hash and `cN` remains an alias resolved on the default branch only. Provisional.
