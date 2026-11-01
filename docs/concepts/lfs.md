# Git LFS API

A pyn server speaks the Git LFS Locks API and the Batch API with the `basic` transfer adapter (decision #775). Stock
`git lfs lock`, `unlock` and `locks` work against it, and the locks taken that way are the same locks the pyn CLI and web app
see. Stock `git lfs push` and `fetch` store and retrieve large files through it. The `tus` adapter is not served.

The routes are not in the generated OpenAPI document: LFS clients send and expect `application/vnd.git-lfs+json`, which the
generated client contract does not model. This page is their contract.

## Endpoint

The routes live under the repository's LFS base:

```
/v1/repos/{owner}/{name}/info/lfs
```

A repository name may also be written `name.git`, which is what git-lfs derives from a remote URL: with a remote of
`https://pyn.example/v1/repos/acme/game`, git-lfs looks for `https://pyn.example/v1/repos/acme/game.git/info/lfs`, and
that works. To be explicit, set the URL in the repository:

```bash
git config lfs.url https://pyn.example/v1/repos/acme/game/info/lfs
```

## Authentication

The same credentials as the rest of the API: a pyn token as `Authorization: Bearer <token>`, or HTTP Basic with the token as
the password (what git-lfs sends; the user name is ignored). A request with none, or a bad one, gets `401` with
`WWW-Authenticate: Basic realm="pyn"` so git prompts or asks its credential helper. Roles apply as for pyn locks: creating and
releasing a lock needs the `lock` permission, listing and `verify` need `read`. Transferring large files needs `read` to download and
`checkin`, the permission that lets a pyn client upload content, to upload.

These routes require authentication even for a public repository, where pyn's other read routes do not. A private repository the
caller has no part in is `404`.

## Locks: mapping to pyn locks

- An LFS lock is the pyn lock on the path. A lock taken with `git lfs lock` shows in `pyn locks` and the web app, and the
  reverse.
- A path that is `exclusive` in `.pyn/pyn.toml` is enforced: only the holder can check in. It is locked at its current head
  as `pyn lock` does.
- Any other path gets an advisory lock: the same record, but a check-in of a shared path never consults it. `pyn.toml` is the
  authority, so `lockable` in `.gitattributes` does not weaken an exclusive path.
- Leases are a pyn extension that LFS clients do not see. An LFS lock carries the repository's lease (8 hours by default) and
  lapses with it; `pyn lock` renews it.
- The lock `id` is a stable opaque string derived from the path: the same path always has the same id.
- `ref` in requests is accepted and ignored (pyn has a single locking line per repository).
- Every create and unlock is audited and queues the lock events exactly as `pyn lock` and `pyn unlock` do. A force unlock needs
  the `force_unlock` permission, as a pyn force unlock does, and its audit entry reads "forced through the LFS API".

## Lock routes

Bodies and responses are `application/vnd.git-lfs+json`. A write (`POST`) must declare `application/vnd.git-lfs+json` or `application/json`, otherwise it is refused with `415`, so a cross-site form post cannot act on a cached Basic credential.

### `POST .../locks`

Request `{"path": "Content/a.umap", "ref": {"name": "refs/heads/main"}}`.

- `201` `{"lock": {"id", "path", "locked_at", "owner": {"name"}}}`
- `409` `{"lock": {...}, "message"}`: the path is already locked (by anyone, the caller included); `lock` is the holder's.
  Also returned without `lock` when the user's lock limit is reached.
- `403` the caller lacks the `lock` permission; `400` the path is invalid.

### `GET .../locks?path=&id=&cursor=&limit=&refspec=`

`200` `{"locks": [...], "next_cursor"}`. Locks are ordered by path. `path` and `id` filter; `limit` defaults to 100 and is capped
at 1000; `next_cursor` is absent on the last page and is passed back as `cursor`. A malformed cursor is `400`.

### `POST .../locks/verify`

Request `{"ref": {...}, "cursor": "", "limit": 100}`, all optional. `200` `{"ours": [...], "theirs": [...], "next_cursor"}`:
the locks the caller holds and the locks others hold, paged together in path order.

### `POST .../locks/{id}/unlock`

Request `{"force": false, "ref": {...}}`. `200` `{"lock": {...}}` with the removed lock.

- `403` the lock is someone else's and `force` is not set, or `force` is set and the caller lacks `force_unlock`.
- `404` no lock has that id.

## Lock errors

`{"message": "...", "request_id"?, "documentation_url"?}` with the status above; `request_id` and `documentation_url` are
not sent. A body that is not valid JSON is `422`. An unknown repository, or a private one the caller has no part in, is `404`.

## Large files: the Batch API

Large files are stored once per repository, by the sha256 `oid` and length of their content, and moved with the `basic`
transfer adapter. Only objects uploaded through this API are served through it. A file checked in the pyn way is not an LFS
object and is not found by `oid`, so the server never hashes native content on demand, and a native repository gains no pointer
files: pointers exist only at this boundary (decision #775).

Set the endpoint in the git repository the way it is shown under [Endpoint](#endpoint); git-lfs also derives it from a remote
named `https://pyn.example/v1/repos/acme/game`.

### `POST .../objects/batch`

Request `{"operation": "download" | "upload", "transfers": ["basic"], "ref": {...}, "objects": [{"oid", "size"}], "hash_algo": "sha256"}`.
`transfers` and `hash_algo` may be left out; `ref` is accepted and ignored. A batch names at most 1000 objects.

`200` `{"transfer": "basic", "objects": [...], "hash_algo": "sha256"}`, one entry per requested object in order. An entry that has
something to do carries `"authenticated": true` and `actions`, each `{"href"}` with an absolute URL; the client sends the same
credentials to it, so no `header` is returned.

| Operation | The server has the object | The server does not |
| --- | --- | --- |
| `download` | `actions.download` | `error` `{"code": 404}` |
| `upload` | no `actions`: nothing to do | `actions.upload` and `actions.verify` |

A problem with one object is reported in that object's `error` `{"code", "message"}` and does not fail the batch:

- `422`: the `oid` is not 64 lowercase hex digits, the `size` is negative, or the `oid` is known with another size.
- `404`: a download of an object this repository does not have.
- `413`: an upload of more than 16 GiB, or one that would take the owner or the repository past its [storage cap](limits.md#per-repository-storage-cap), counting the repository's LFS objects.

Refusals of the whole request: `422` for an unknown `operation`, a body that is not valid JSON, or `transfers` without `basic`;
`409` for a `hash_algo` other than `sha256`; `403` for an upload by a caller without `checkin`, or while the owner is
[frozen](limits.md); `415` for a content type that is neither `application/vnd.git-lfs+json` nor `application/json`.

The `href` URLs use the scheme and host of the request. When the server runs behind a reverse proxy it trusts
(`PYN_TRUST_FORWARDED_FOR`), `X-Forwarded-Host` and `X-Forwarded-Proto` take precedence, so a proxy must pass them. Without a `Host`
header the configured public URL is used.

### `PUT .../objects/{oid}`

Uploads the content as the request body. A `Content-Length` is required (`400` without) and is the declared size. The body
streams to the object store, chunked and deduplicated as other content is (see [storage](storage.md)), while its sha256 is
computed. The server stores the object only if the digest equals `oid` and the body is exactly `Content-Length` bytes;
otherwise it answers `422` (`digest_mismatch`, `length_mismatch`) and keeps nothing. `200` with no body on success.

- Uploading an object the repository already has is a `200` that changes nothing, as long as the length agrees (`422` otherwise).
- `403` without `checkin` or while the owner is frozen (`owner_frozen`); `413` past the 16 GiB object limit
  (`object_too_large`), the storage caps (`storage_limit_reached`, `repo_storage_limit_reached`) or the
  [unreferenced-upload ceiling](storage.md#per-repository-visibility); `429` when too many uploads run at once.
- Uploads count against the same per-user upload concurrency as `PUT .../objects`. They are recorded in the repository's usage
  accounting like other uploads; no audit entry is written, as for `PUT .../objects`.

### `GET .../objects/{oid}`

Streams the content with `Content-Length`, `Accept-Ranges: bytes` and `Content-Type: application/octet-stream`. A single
`Range: bytes=` header is honoured (`206` with `Content-Range`, `416` outside the content). `404` for an `oid` this repository
has no LFS object for (an object another repository holds is not visible), `422` for a malformed `oid`. Needs `read`.

### `POST .../objects/verify`

Request `{"oid", "size"}`; `200` when the repository has that object, `404` when it has not, `422` when the size differs. Needs
`checkin`, like the upload it follows; the batch response names this action after every upload.

### Limits and accounting

- An LFS upload is held to the same bounds as any upload: the owner must not be frozen, and the unreferenced-upload ceiling
  (32 GiB by default per repository, `PYN_MAX_UNREFERENCED_UPLOAD_BYTES`) applies because no revision names the object.
  Raise the ceiling for repositories that keep more large files than that.
- The storage caps are enforced at upload time against the bytes the owner or repository already stores in revisions plus the
  repository's LFS objects. The check is not atomic with a concurrent upload or check-in, and the usage reports
  (`pyn usage`) count revisions only, so LFS bytes do not appear in them yet.
- Objects are not removed when a repository's other state changes. Deleting the repository removes its LFS records. The bytes
  stay in the shared object store until [garbage collection](storage.md#planned-provisional) (#207), which must treat the
  object each LFS record names as referenced.
- A failed upload leaves no record. Chunks of a large body that were already stored stay until garbage collection, and are
  reused if the upload is retried.
