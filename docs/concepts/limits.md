# Limits and usage

Limits are optional. A server enforces nothing until its operator sets a limit: with none configured, no owner is
limited, no default exists, no extra configuration is needed, and every command and route behaves as it did before limits
existed. They are meant mainly for public and hosted servers that need to keep one owner from starving the rest. The
server holds no concept of plans or payment; an operator or an external service only sets generic numbers.

## What can be limited

An **owner** is a user or an organization: the namespace in `owner/name`.

| Limit | Counts | Applies to |
| --- | --- | --- |
| `repositories` | repositories the owner has | users and organizations |
| `members` | members of the organization (owners included) | organizations only; a user has no member limit |
| `storage_bytes` | bytes of distinct content across the owner's repositories | users and organizations |

Unset means unlimited. `0` is a valid limit and blocks growth entirely. Bandwidth is not limited: nothing measures it yet.

Storage counts each distinct content once per repository (the same bytes checked in at two paths, or restored, add
nothing) and each repository separately, so deleting a repository frees its bytes. Revisions made before limits existed
carry no recorded size and count as 0 bytes.

Uploads no revision names yet are not part of `storage_bytes`; see [unreferenced uploads](#unreferenced-uploads).

## Unreferenced uploads

A repository may hold a fixed number of bytes in uploads that no revision names yet. The server setting
`PYN_MAX_UNREFERENCED_UPLOAD_BYTES` (a whole number of bytes, applied to every repository) sets it; unset, it is
32 GiB (34359738368). An upload that would pass it is refused with `storage_limit_reached` until some are checked in
(see [content storage](storage.md#per-repository-visibility)). Like the other limits, `0` blocks growth entirely: every
non-empty upload is refused. A value that is not a whole number stops the server at start, and the effective value is
written to the startup log. It is read at start; there is no way to change it while the server runs.

The budget is reserved before an upload's body is read: the declared length is held against the repository's
unreferenced total, together with the uploads still in flight, in one atomic step shared by every server copy using the
database. Parallel uploads cannot pass the ceiling together. The hold is released when the upload ends; one left by a
server that stopped expires after six hours. An upload whose body does not match its declared length, or that cannot
be recorded, leaves no stored bytes behind, apart from bytes that were already stored.

## Concurrent uploads

`PYN_MAX_UPLOADS_PER_USER` (default 8) and `PYN_MAX_UPLOADS` (default 64) bound how many uploads run at once for one
user and for the server process. An upload past either is refused with `429 uploads_busy` and a `Retry-After` of 5
seconds before its body is read. Both must be at least 1, and an invalid value stops the server at start. The counts
are per process.

## Where a limit comes from

For each field the server uses the owner's own value if one is set, else the **server default**, else no limit. The
default is itself unset unless the operator provides it:

| Variable | Sets the default for |
| --- | --- |
| `PYN_DEFAULT_MAX_REPOSITORIES` | repositories |
| `PYN_DEFAULT_MAX_ORG_MEMBERS` | members of an organization |
| `PYN_DEFAULT_MAX_STORAGE_BYTES` | stored bytes |

There is no way to exempt one owner from a default except a number large enough not to matter.

## Enforcement

The server checks at the moment something grows, in the same atomic step as the write, so concurrent requests cannot
overshoot. Each limit has its own error code and answers `409`:

| Code | When |
| --- | --- |
| `repo_limit_reached` | creating a repository when the owner already has the limit |
| `member_limit_reached` | adding a member to an organization at its limit |
| `storage_limit_reached` | a check-in or restore that would add content past the limit, or an upload past the repository's unreferenced-upload ceiling |

`uploads_busy` is `429`, not `409`: it is not a limit on what is stored but on uploads at the same time.

## Request size bounds

Fixed bounds apply to what a request may carry, and a request over one is `400 invalid_request` (`invalid_path` for a path):

| Field | Bound |
| --- | --- |
| token name, SSH key title | 1 to 256 bytes, no control characters |
| team name, team description | 100 and 500 characters; control characters refused (a description may hold newline and tab) |
| commit and revision message | 64 KiB; control characters refused except newline, tab and carriage return |
| `.pyn/pyn.toml` | 1 MiB; a larger policy file is refused with `invalid_rules` without being read |
| message in an audit entry | 256 bytes; longer messages are cut with `...` |
| repositories listed in a token's audit entry | 8, then a count of the rest |
| paths in one changeset | 10000 |
| commits in one push | 10000 |
| file changes (added, changed or removed paths) in one pushed commit | 10000 |
| file changes across all the commits of one push | 50000 |
| entries in one tree a push brings or a check-in builds (one folder) | 100000; a push over it is refused with `push_invalid`, a check-in with `invalid_request` |
| invitation lifetime | 1 to 8760 hours |
| path | 4096 bytes, 255 per segment, see [the path rules](pyn-toml.md#path-rules-for-new-writes) |

A push over either file-change bound is refused whole with `413 push_too_large`: the diff stops at the first change past
the bound, and nothing is stored. The client pushes the commits in smaller batches.

Lowering a limit below current use never deletes anything: existing repositories, members and content stay, and only
new growth is refused until use falls under the limit. The [lock limit](pyn-toml.md#lock-limit) is separate and
unchanged.

## Per-request bounds

Each request is bounded in the work it may do, so one request cannot hold the server for long or crowd out the others.

| Bound | Default | Setting |
| --- | --- | --- |
| time for a read route to answer (including fetch and the previews) | 60 s, `0` is no limit | `PYN_REQUEST_TIMEOUT_SECS` |
| time an upload body may send nothing (object, chunk, assemble, push) | 30 s, `0` is no limit | `PYN_BODY_IDLE_TIMEOUT_SECS` |
| fetch, history, verify, snapshot, summary and merge-preview requests at once | four per CPU | `PYN_MAX_CONCURRENT_EXPENSIVE` |
| commits one fetch request examines | 1,000,000 | fixed |
| revisions one history request examines | 20,000 | fixed |
| revisions of one path in a path history page | 50, at most 200 | `limit` |
| records one verify step examines (revisions, commits, audit events, tree entries) | 20,000 | fixed |
| files one folder listing reads | 1,000,000 | fixed |
| files a summary re-counts against the live rules (larger repositories report the stored count) | 100,000 | fixed |

A late answer is `503 request_timeout` and a request that cannot get a slot in time is `503 server_busy`; both carry
`Retry-After`. Writes and the streamed object routes have no overall time limit. A value that is not a whole number, or a
concurrency of 0, stops the server at start. They are read at start.

A fetch examines commits from the cursor onward and refuses a walk past its bound with `400 invalid_request`. A repository
history request that examines its bound without filling a page returns what it found with a `next_cursor`, which may arrive
with fewer revisions than `limit` (or none); the client continues with `before`. The whole-repository form of
`GET .../verify` needs a signed-in caller. A path's history is paged the same way (`limit`, `before`), and a verify
request does one bounded step and returns a `next_cursor` to continue with ([integrity](integrity.md)). The summary reads
counts the store keeps per commit, not the files ([browsing](browsing.md#summary)).

## Setting limits

Server administrators, and service credentials with the `manage_limits` scope, set an owner's own limits. A field left
out of a change stays as it is; a number sets it and `null` returns the field to the server default.

| Route | Notes |
| --- | --- |
| `PATCH /v1/admin/owners/{owner}/limits` | `{repositories?, members?, storage_bytes?}`; `400 invalid_request` for a value out of range or a member limit on a user; `404 user_not_found`; `403 server_admin_required` or `service_scope_required` |
| `GET /v1/admin/limits` | the server default and every owner with limits of its own |

Each change is recorded as `owner_limits_changed` in the server audit log, and in the organization's log for an
organization.

```bash
pyn admin limits set alice --repos 20 --storage 5G
pyn admin limits set acme --members 50 --repos default    # `default` drops the owner's own value
pyn admin limits show alice
pyn admin limits list
```

## Seeing limits and usage

The owner (for an organization, an owner of it), a server administrator and a service credential with `manage_limits`
may look at an owner's limits and usage. Anyone who can read a repository may see its own usage.

The caller is checked before the owner is looked up: anyone else gets `403 not_namespace_owner` whether the name is
missing, a user or an organization, so the routes do not reveal which accounts exist. A member of the organization who is
not an owner gets `403 not_org_owner`. Only an administrator or a `manage_limits` credential gets `404 user_not_found`.

| Route | Returns |
| --- | --- |
| `GET /v1/owners/{owner}/limits` | `{kind, effective, own}`: the limits in force, and the owner's own; a null field is unlimited |
| `GET /v1/owners/{owner}/usage` | `{owner, limits, usage: {repositories, members, stored_bytes}, repositories: [...]}` with each repository's usage |
| `GET /v1/repos/{owner}/{name}/usage` | `{repository, stored_bytes, files, revisions}` |

```bash
pyn usage              # you
pyn usage acme         # an organization you own
pyn repo usage alice/game
```
