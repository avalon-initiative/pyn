# Events

pyn can tell other systems when something happens on the server: an account is approved, a repository is created, an
owner reaches a limit. This page is the schema of those events, the JSON body of a webhook delivery. Signing and delivery are separate (decision #452), and
[subscriptions and their secrets](webhooks.md) are administered through the API. The schema lives in `pyn-proto` and is part of the OpenAPI document
(`docs/generated/openapi.json`, schemas `Event` and `*Data`).

## Envelope

Every event is one JSON object:

| Field | Meaning |
| --- | --- |
| `id` | Unique per event; a redelivery keeps it, so receivers can deduplicate on it. |
| `type` | A dotted name such as `repository.created`. |
| `created_at` | When the event happened, RFC 3339 in UTC. |
| `server` | The server's configured public base URL (`PYN_PUBLIC_URL`). |
| `version` | The envelope version, `1` today. |
| `data` | The payload for `type`, described below. |

## When events are written

Account, organization, repository, lock, `owner.limits_changed`, `owner.frozen` and `owner.unfrozen` events go into the webhook outbox in the same database
transaction as the change, so a crash never loses one and a refused or rolled-back change leaves none. Each is queued
once per subscription whose filter matches. `owner.limit_reached` is the exception: it is recorded after the refusal and
throttled (see below). Events are queued only when the change really happens: a duplicate name, a missing target or a
refused request raises nothing.

An account created by open sign-up with email verification raises `account.created` when the address is verified, not
when the unverified name is reserved, so abandoned sign-ups never reach receivers. A repository whose creation is undone
before it finishes (for example, the creator is no longer an organization member) raises `repository.deleted` after its
`repository.created`. Ensuring that a user record exists for a development identity or a token is not an account creation.

## Receiving events

A receiver does four things with every request, in this order. [Delivery](webhooks.md#delivery) lists the headers.

1. **Verify the signature** on the raw body, before parsing it. Read `t` and every `v1` from `Pyn-Signature`, compute the
   HMAC-SHA256 of `<t>.<raw body>` keyed with your secret text (`whsec_...`) and compare it with each `v1` in constant
   time; one match is enough. During a [rotation](webhooks.md#secrets) there are two `v1` values, so a receiver that
   knows either secret verifies. Answer `401` and do not process the event when nothing matches.
2. **Apply the replay window.** Reject a request whose `t` is more than 5 minutes from your clock, in either direction,
   so a captured request cannot be replayed later. Every retry is signed again with a fresh `t`, so a late retry still
   passes. Keep your clock synchronised (NTP).
3. **Deduplicate on the delivery id.** Delivery is at least once: a retry after a timeout, a lost answer, or a manual
   [redeliver](webhooks.md#deliveries-and-redelivery) sends the same event again with the same `Pyn-Delivery` value.
   Store the ids you have processed and acknowledge a repeat without acting on it. The delivery id is per subscription;
   the envelope `id` is per event and identical across subscriptions.
4. **Acknowledge quickly.** Answer any `2xx` within a few seconds and do the work afterwards; the sender gives up on a
   request after 10 seconds and counts it as a failed attempt. Acknowledge types you do not know (see below).

Order is not guaranteed: a retried event can arrive after a newer one. Use `created_at` to order what matters.

```python
import hashlib, hmac, time

def verify(header: str, body: bytes, secrets: list[str], window: int = 300) -> bool:
    parts = dict(p.split("=", 1) for p in header.split(",") if "=" in p and not p.startswith("v1="))
    tags = [p[3:] for p in header.split(",") if p.startswith("v1=")]
    if abs(time.time() - int(parts["t"])) > window:
        return False
    for secret in secrets:
        expected = hmac.new(secret.encode(), f"{parts['t']}.".encode() + body, hashlib.sha256).hexdigest()
        if any(hmac.compare_digest(expected, tag) for tag in tags):
            return True
    return False
```

## Open types and versioning

Event types are an open set. A receiver must ignore a type it does not know (acknowledge it and do nothing) instead of
failing, so new types can be added without breaking anyone. In the same way, new optional
fields may appear in `data`; receivers ignore fields they do not know.

`version` changes only for a breaking change to the envelope or to an existing payload (a field removed, renamed or
changed in meaning). Adding a type or a field does not change it. An event carries the version it was written with.

Payloads name accounts, organizations and repositories by the names the API already shows, and never carry secrets,
password data, tokens or email addresses.

## Stable ids

Names are addresses: a repository can be renamed, and a deleted organization's name can be taken again by someone else.
A receiver that keeps its own data about a tenant must therefore key it by id, never by name. Every payload that names a
user, organization or repository also carries its id, in a field named after the name field with `_id` added
(`user`/`user_id`, `owner`/`owner_id`, `org_id`, `repo_id`).

- A repository id is `r` and 24 lowercase hex digits (96 random bits); a user id is `u` and an organization id is `o`, each
  followed by 24 hex digits. The id of a repository created before ids were exposed is the one it already had, so one
  legacy repository may carry the id `default`. Treat ids as opaque strings.
- The server generates an id when the thing is created and stores it. It is never derived from the name, never changes (a
  rename keeps it), and is never issued again after a delete, so an organization or repository recreated under a deleted name has a new
  id. `repository.renamed` carries the unchanged `repo_id` so a consumer re-keys by name only; `repository.deleted` and
  `organization.deleted` carry the id of what is gone, so a consumer can drop its data.
- The same ids appear in the API: `id` on `RepoInfo`, `OrgInfo`, `AccountInfo` and `GET /v1/me`, `owner_id` on `RepoInfo`,
  the limits routes and the usage routes, `repo_id` on repository usage, and `user_id` on the repository `me` route.
- Actors (`actor`) and lock holders (`holder`) are still named only by name: an actor can be a service credential, which
  has no account.

## Events

### `account.created`

A new account exists: sign-up, first sign-in with an external provider, or an administrator creating it.

`data`: `user`, `user_id`; `pending_approval` is true when an administrator must approve it before it can sign in. No email or credential is ever included.

```json
{
  "id": "evt_01J9ZX3K5V8Q2M7R4T6N0B1C2D",
  "type": "account.created",
  "created_at": "2026-03-04T05:06:07Z",
  "server": "https://pyn.example.com",
  "version": 1,
  "data": {
    "user": "alice",
    "user_id": "u3f9c1a7e5b2d4086a1c7e902",
    "pending_approval": true
  }
}
```

### `account.approved`

An administrator approved an account that was waiting.

`data`: `user`, `user_id`; `actor`, the administrator or service credential.

```json
{
  "id": "evt_01J9ZX3K5V8Q2M7R4T6N0B1C2D",
  "type": "account.approved",
  "created_at": "2026-03-04T05:06:07Z",
  "server": "https://pyn.example.com",
  "version": 1,
  "data": {
    "user": "alice",
    "user_id": "u3f9c1a7e5b2d4086a1c7e902",
    "actor": "root"
  }
}
```

### `account.disabled`

An administrator disabled an account.

`data`: `user`, `user_id`, `actor`, and `reason` (null when none was given).

```json
{
  "id": "evt_01J9ZX3K5V8Q2M7R4T6N0B1C2D",
  "type": "account.disabled",
  "created_at": "2026-03-04T05:06:07Z",
  "server": "https://pyn.example.com",
  "version": 1,
  "data": {
    "user": "alice",
    "user_id": "u3f9c1a7e5b2d4086a1c7e902",
    "actor": "root",
    "reason": "spam"
  }
}
```

### `account.enabled`

An administrator lifted a disable.

`data`: `user`, `user_id`, `actor`.

```json
{
  "id": "evt_01J9ZX3K5V8Q2M7R4T6N0B1C2D",
  "type": "account.enabled",
  "created_at": "2026-03-04T05:06:07Z",
  "server": "https://pyn.example.com",
  "version": 1,
  "data": {
    "user": "alice",
    "user_id": "u3f9c1a7e5b2d4086a1c7e902",
    "actor": "root"
  }
}
```

### `organization.created`

An organization was created, by a user or on behalf of one by an administrator or service credential.

`data`: `name`, `org_id`; `owner`, its first owner, and `owner_id`; `actor`, who made the request.

```json
{
  "id": "evt_01J9ZX3K5V8Q2M7R4T6N0B1C2D",
  "type": "organization.created",
  "created_at": "2026-03-04T05:06:07Z",
  "server": "https://pyn.example.com",
  "version": 1,
  "data": {
    "name": "studio",
    "org_id": "o7d2e41b90c3a58f6e1d4b7a3",
    "owner": "alice",
    "owner_id": "u3f9c1a7e5b2d4086a1c7e902",
    "actor": "root"
  }
}
```

### `organization.deleted`

An empty organization was deleted.

`data`: `name`, `org_id` (never issued again), `actor`.

```json
{
  "id": "evt_01J9ZX3K5V8Q2M7R4T6N0B1C2D",
  "type": "organization.deleted",
  "created_at": "2026-03-04T05:06:07Z",
  "server": "https://pyn.example.com",
  "version": 1,
  "data": {
    "name": "studio",
    "org_id": "o7d2e41b90c3a58f6e1d4b7a3",
    "actor": "alice"
  }
}
```

### `repository.created`

A repository was created.

`data`: `repo_id`, `owner`, `owner_id`, `name`, `visibility` (`public` or `private`), `actor`.

```json
{
  "id": "evt_01J9ZX3K5V8Q2M7R4T6N0B1C2D",
  "type": "repository.created",
  "created_at": "2026-03-04T05:06:07Z",
  "server": "https://pyn.example.com",
  "version": 1,
  "data": {
    "repo_id": "r5e8a0c3f1b7d92460a4c8e15",
    "owner": "studio",
    "owner_id": "o7d2e41b90c3a58f6e1d4b7a3",
    "name": "game",
    "visibility": "private",
    "actor": "alice"
  }
}
```

### `repository.renamed`

A repository's name changed (the owner never does). Raised only for a name change, not for other setting changes.

`data`: `repo_id` (unchanged), `owner`, `owner_id`, the new `name`, `previous_name`, `actor`.

```json
{
  "id": "evt_01J9ZX3K5V8Q2M7R4T6N0B1C2D",
  "type": "repository.renamed",
  "created_at": "2026-03-04T05:06:07Z",
  "server": "https://pyn.example.com",
  "version": 1,
  "data": {
    "repo_id": "r5e8a0c3f1b7d92460a4c8e15",
    "owner": "studio",
    "owner_id": "o7d2e41b90c3a58f6e1d4b7a3",
    "name": "game-2",
    "previous_name": "game",
    "actor": "alice"
  }
}
```

### `repository.deleted`

A repository was deleted.

`data`: `repo_id` (never issued again), `owner`, `owner_id`, `name`, `actor`.

```json
{
  "id": "evt_01J9ZX3K5V8Q2M7R4T6N0B1C2D",
  "type": "repository.deleted",
  "created_at": "2026-03-04T05:06:07Z",
  "server": "https://pyn.example.com",
  "version": 1,
  "data": {
    "repo_id": "r5e8a0c3f1b7d92460a4c8e15",
    "owner": "studio",
    "owner_id": "o7d2e41b90c3a58f6e1d4b7a3",
    "name": "game-2",
    "actor": "alice"
  }
}
```

### `owner.limits_changed`

An administrator or service credential set an owner's own [limits](limits.md).

`data`: `owner`, `owner_id`, `kind` (`user` or `org`), `limits` (the owner's own values after the change; null means the server default applies), `actor`.

```json
{
  "id": "evt_01J9ZX3K5V8Q2M7R4T6N0B1C2D",
  "type": "owner.limits_changed",
  "created_at": "2026-03-04T05:06:07Z",
  "server": "https://pyn.example.com",
  "version": 1,
  "data": {
    "owner": "studio",
    "owner_id": "o7d2e41b90c3a58f6e1d4b7a3",
    "kind": "org",
    "limits": {
      "repositories": 10,
      "members": null,
      "storage_bytes": 1073741824
    },
    "actor": "root"
  }
}
```

### `owner.frozen`

An administrator or service credential froze an owner: every write under it is refused until it is unfrozen
([freezing an owner](limits.md#freezing-an-owner)). Freezing an owner that is already frozen raises it again with the new reason.

`data`: `owner`, `owner_id`, `kind` (`user` or `org`), `reason`, `actor`.

```json
{
  "id": "evt_01J9ZX3K5V8Q2M7R4T6N0B1C2D",
  "type": "owner.frozen",
  "created_at": "2026-03-04T05:06:07Z",
  "server": "https://pyn.example.com",
  "version": 1,
  "data": {
    "owner": "studio",
    "owner_id": "o7d2e41b90c3a58f6e1d4b7a3",
    "kind": "org",
    "reason": "plan lapsed",
    "actor": "billing"
  }
}
```

### `owner.unfrozen`

A freeze was lifted. Unfreezing an owner that is not frozen raises nothing.

`data`: `owner`, `owner_id`, `kind`, `actor`.

```json
{
  "id": "evt_01J9ZX3K5V8Q2M7R4T6N0B1C2D",
  "type": "owner.unfrozen",
  "created_at": "2026-03-04T05:06:07Z",
  "server": "https://pyn.example.com",
  "version": 1,
  "data": {
    "owner": "studio",
    "owner_id": "o7d2e41b90c3a58f6e1d4b7a3",
    "kind": "org",
    "actor": "billing"
  }
}
```

### `owner.limit_reached`

An operation was refused because the owner is at one of its limits. Recorded in the webhook outbox on repository, member, storage and lock limit refusals; a storage refusal is a
check-in, push, merge or changeset that would add content past the owner's storage limit, while an upload past a repository's unreferenced-upload ceiling raises no event. Each owner and limit
is admitted at most once per hour, measured from its last recorded event using the store's clock. Admission and
outbox fan-out are atomic, including across service instances sharing Postgres. The refusal error is unchanged
if event recording fails. Repository-scoped limits name the repository owner, including when a different user hits a lock limit. For locks the `value` is the
repository's lock cap, not an owner limit, and the admission hour is shared across the owner's repositories.

When the store uses the caller's clock, a replica running behind is suppressed until it catches up to the admission window;
a replica running ahead can push `last_event_at` into the future. Postgres uses the database clock by default.

`data`: `owner`, `owner_id`, `limit` (`repositories`, `members`, `storage_bytes` or `locks`), `value` (the limit in force; for locks, the repository's lock cap). It names no actor: the refused caller is not part of the event.

```json
{
  "id": "evt_01J9ZX3K5V8Q2M7R4T6N0B1C2D",
  "type": "owner.limit_reached",
  "created_at": "2026-03-04T05:06:07Z",
  "server": "https://pyn.example.com",
  "version": 1,
  "data": {
    "owner": "studio",
    "owner_id": "o7d2e41b90c3a58f6e1d4b7a3",
    "limit": "repositories",
    "value": 10
  }
}
```

### Lock events

Raised in the same transaction as the lock change, so a refused change (a held path, the lock limit, a release by a non-holder) raises nothing. Every lock event names the repository by `owner` and `repo` (its name), with `owner_id` and `repo_id`, and the locked `path`. A renewal (checking out again while holding a live lock) raises no event.

Expiry is observed lazily: pyn has no background reaper, and a lease that runs out just stops counting as a lock. `lock.expired` is raised when the lapsed lease is replaced, by the next checkout of that path (by anyone, the former holder included), immediately before that checkout's `lock.acquired`. A lease nobody takes again never raises it. A check-in or push that consumes the holder's lock (the lock is released with the path's revision, in the same transaction) raises `lock.released` with `reason` `checkin`. A policy change that makes a path shared releases the live locks on it as `lock.forced`, not `lock.released`. Deleting a repository raises no lock events: `repository.deleted` covers the locks it removes.

### `lock.acquired`

A path was locked, including by taking over a lapsed lease.

`data`: `owner`, `owner_id`, `repo`, `repo_id`, `path`, `holder`, `expires_at` (when the lease ends).

```json
{
  "id": "evt_01J9ZX3K5V8Q2M7R4T6N0B1C2D",
  "type": "lock.acquired",
  "created_at": "2026-03-04T05:06:07Z",
  "server": "https://pyn.example.com",
  "version": 1,
  "data": {
    "owner": "studio",
    "owner_id": "o7d2e41b90c3a58f6e1d4b7a3",
    "repo": "game",
    "repo_id": "r5e8a0c3f1b7d92460a4c8e15",
    "path": "Content/Maps/Main.umap",
    "holder": "alice",
    "expires_at": "2026-03-05T05:06:07Z"
  }
}
```

### `lock.released`

The holder released the lock, or a check-in or push consumed it.

`data`: `owner`, `owner_id`, `repo`, `repo_id`, `path`, `holder`, `reason` (`checkin` when a revision of the path consumed the lock; null for an explicit release).

### `lock.expired`

A lapsed lease was replaced (see above).

`data`: `owner`, `owner_id`, `repo`, `repo_id`, `path`, `holder` (who held the lapsed lease), `expires_at` (when it ran out).

### `lock.forced`

A live lock was removed without its holder: a forced unlock, or a policy change that made the path shared.

`data`: `owner`, `owner_id`, `repo`, `repo_id`, `path`, `holder` (whose lock was removed), `actor` (who removed it), `reason` (the unlock reason; `now shared by policy` for a policy change).

```json
{
  "id": "evt_01J9ZX3K5V8Q2M7R4T6N0B1C2D",
  "type": "lock.forced",
  "created_at": "2026-03-04T05:06:07Z",
  "server": "https://pyn.example.com",
  "version": 1,
  "data": {
    "owner": "studio",
    "owner_id": "o7d2e41b90c3a58f6e1d4b7a3",
    "repo": "game",
    "repo_id": "r5e8a0c3f1b7d92460a4c8e15",
    "path": "Content/Maps/Main.umap",
    "holder": "alice",
    "actor": "bob",
    "reason": "alice is out sick"
  }
}
```

See also [limits](limits.md) and [access](access.md).
