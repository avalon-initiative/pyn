# Events

pyn can tell other systems when something happens on the server: an account is approved, a repository is created, an
owner reaches a limit. This page is the schema of those events, the JSON body of a webhook delivery. Signing, delivery,
subscriptions and storage are separate (decision #452); nothing raises these events yet, so this page describes the
contract the server will follow. The schema lives in `pyn-proto` and is part of the OpenAPI document
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

## Open types and versioning

Event types are an open set. A receiver must ignore a type it does not know (acknowledge it and do nothing) instead of
failing, so new types, such as an owner freeze, can be added without breaking anyone. In the same way, new optional
fields may appear in `data`; receivers ignore fields they do not know.

`version` changes only for a breaking change to the envelope or to an existing payload (a field removed, renamed or
changed in meaning). Adding a type or a field does not change it. An event carries the version it was written with.

Payloads name accounts, organizations and repositories by the names the API already shows, and never carry secrets,
password data, tokens or email addresses.

## Events

### `account.created`

A new account exists: sign-up, first sign-in with an external provider, or an administrator creating it.

`data`: `user`; `pending_approval` is true when an administrator must approve it before it can sign in. No email or credential is ever included.

```json
{
  "id": "evt_01J9ZX3K5V8Q2M7R4T6N0B1C2D",
  "type": "account.created",
  "created_at": "2026-03-04T05:06:07Z",
  "server": "https://pyn.example.com",
  "version": 1,
  "data": {
    "user": "alice",
    "pending_approval": true
  }
}
```

### `account.approved`

An administrator approved an account that was waiting.

`data`: `user`; `actor`, the administrator or service credential.

```json
{
  "id": "evt_01J9ZX3K5V8Q2M7R4T6N0B1C2D",
  "type": "account.approved",
  "created_at": "2026-03-04T05:06:07Z",
  "server": "https://pyn.example.com",
  "version": 1,
  "data": {
    "user": "alice",
    "actor": "root"
  }
}
```

### `account.disabled`

An administrator disabled an account.

`data`: `user`, `actor`, and `reason` (null when none was given).

```json
{
  "id": "evt_01J9ZX3K5V8Q2M7R4T6N0B1C2D",
  "type": "account.disabled",
  "created_at": "2026-03-04T05:06:07Z",
  "server": "https://pyn.example.com",
  "version": 1,
  "data": {
    "user": "alice",
    "actor": "root",
    "reason": "spam"
  }
}
```

### `account.enabled`

An administrator lifted a disable.

`data`: `user`, `actor`.

```json
{
  "id": "evt_01J9ZX3K5V8Q2M7R4T6N0B1C2D",
  "type": "account.enabled",
  "created_at": "2026-03-04T05:06:07Z",
  "server": "https://pyn.example.com",
  "version": 1,
  "data": {
    "user": "alice",
    "actor": "root"
  }
}
```

### `organization.created`

An organization was created, by a user or on behalf of one by an administrator or service credential.

`data`: `name`; `owner`, its first owner; `actor`, who made the request.

```json
{
  "id": "evt_01J9ZX3K5V8Q2M7R4T6N0B1C2D",
  "type": "organization.created",
  "created_at": "2026-03-04T05:06:07Z",
  "server": "https://pyn.example.com",
  "version": 1,
  "data": {
    "name": "studio",
    "owner": "alice",
    "actor": "root"
  }
}
```

### `organization.deleted`

An empty organization was deleted.

`data`: `name`, `actor`.

```json
{
  "id": "evt_01J9ZX3K5V8Q2M7R4T6N0B1C2D",
  "type": "organization.deleted",
  "created_at": "2026-03-04T05:06:07Z",
  "server": "https://pyn.example.com",
  "version": 1,
  "data": {
    "name": "studio",
    "actor": "alice"
  }
}
```

### `repository.created`

A repository was created.

`data`: `owner`, `name`, `visibility` (`public` or `private`), `actor`.

```json
{
  "id": "evt_01J9ZX3K5V8Q2M7R4T6N0B1C2D",
  "type": "repository.created",
  "created_at": "2026-03-04T05:06:07Z",
  "server": "https://pyn.example.com",
  "version": 1,
  "data": {
    "owner": "studio",
    "name": "game",
    "visibility": "private",
    "actor": "alice"
  }
}
```

### `repository.renamed`

A repository's name changed (the owner never does). Raised only for a name change, not for other setting changes.

`data`: `owner`, the new `name`, `previous_name`, `actor`.

```json
{
  "id": "evt_01J9ZX3K5V8Q2M7R4T6N0B1C2D",
  "type": "repository.renamed",
  "created_at": "2026-03-04T05:06:07Z",
  "server": "https://pyn.example.com",
  "version": 1,
  "data": {
    "owner": "studio",
    "name": "game-2",
    "previous_name": "game",
    "actor": "alice"
  }
}
```

### `repository.deleted`

A repository was deleted.

`data`: `owner`, `name`, `actor`.

```json
{
  "id": "evt_01J9ZX3K5V8Q2M7R4T6N0B1C2D",
  "type": "repository.deleted",
  "created_at": "2026-03-04T05:06:07Z",
  "server": "https://pyn.example.com",
  "version": 1,
  "data": {
    "owner": "studio",
    "name": "game-2",
    "actor": "alice"
  }
}
```

### `owner.limits_changed`

An administrator or service credential set an owner's own [limits](limits.md).

`data`: `owner`, `kind` (`user` or `org`), `limits` (the owner's own values after the change; null means the server default applies), `actor`.

```json
{
  "id": "evt_01J9ZX3K5V8Q2M7R4T6N0B1C2D",
  "type": "owner.limits_changed",
  "created_at": "2026-03-04T05:06:07Z",
  "server": "https://pyn.example.com",
  "version": 1,
  "data": {
    "owner": "studio",
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

### `owner.limit_reached`

An operation was refused because the owner is at one of its limits. Raised after the refusal and throttled to once per owner and limit per hour.

`data`: `owner`, `limit` (`repositories`, `members` or `storage_bytes`), `value` (the limit in force). It names no actor: the refused caller is not part of the event.

```json
{
  "id": "evt_01J9ZX3K5V8Q2M7R4T6N0B1C2D",
  "type": "owner.limit_reached",
  "created_at": "2026-03-04T05:06:07Z",
  "server": "https://pyn.example.com",
  "version": 1,
  "data": {
    "owner": "studio",
    "limit": "repositories",
    "value": 10
  }
}
```

See also [limits](limits.md) and [access](access.md).
