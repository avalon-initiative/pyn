# Git LFS Locks API

A pyn server speaks the Git LFS Locks API, so stock `git lfs lock`, `unlock` and `locks` work against it and the locks taken
that way are the same locks the pyn CLI and web app see (decision #775). The Batch API (large file transfer) is not served yet.

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
releasing a lock needs the `lock` permission, listing and `verify` need `read`.

## Mapping to pyn locks

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

## Routes

Bodies and responses are `application/vnd.git-lfs+json`. A request body may use that type or any other; it is read as JSON.

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

## Errors

`{"message": "...", "request_id"?, "documentation_url"?}` with the status above; `request_id` and `documentation_url` are
not sent. A body that is not valid JSON is `422`. An unknown repository, or a private one the caller has no part in, is `404`.
