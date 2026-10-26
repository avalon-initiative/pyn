# Webhooks: subscriptions and secrets

A webhook subscription tells the server where to send [lifecycle events](events.md) and which types to send. This page
covers administering subscriptions and their signing secrets, and how events are signed and delivered (decision #452).

## Who may manage them

A server administrator, or a [service credential](access.md) with the `manage_webhooks` scope, uses `/v1/admin/webhooks`
and `pyn admin webhook`. Anyone else gets `403 server_admin_required`, and a service credential without the scope gets
`403 service_scope_required`.

## A subscription

| Field | Meaning |
| --- | --- |
| `id` | Assigned by the server. |
| `name` | A label, 1 to 256 bytes. |
| `url` | Where deliveries are sent, see [URL rules](#url-rules). |
| `event_types` | Types to deliver, such as `repository.created`; empty means every type. Types are an open set, so only the form is checked (lowercase letters, digits, `.` and `_`, at most 64 types). A type nothing raises simply never matches. |
| `enabled` | A disabled subscription receives nothing. Default true. |
| `secrets` | The active secrets: `id`, `created_at` and `encrypted`, never the value. |
| `created_at`, `updated_at`, `created_by` | When and by whom. |

| Route | Does |
| --- | --- |
| `POST /v1/admin/webhooks` | `{name, url, event_types?, enabled?}`; `201` with `{webhook, secret}`. The secret is shown here once |
| `GET /v1/admin/webhooks` | every subscription, oldest first |
| `GET /v1/admin/webhooks/{id}` | one subscription; `404 webhook_not_found` |
| `PATCH /v1/admin/webhooks/{id}` | any of `name`, `url`, `event_types`, `enabled`; fields left out stay; an empty body is `400 invalid_request` |
| `DELETE /v1/admin/webhooks/{id}` | `204`; its secrets and deliveries go with it |
| `POST /v1/admin/webhooks/{id}/secrets` | rotate: adds a second active secret; `201` with `{secret_id, created_at, secret}`, the value shown once. `409 webhook_secret_limit` when two are already active |
| `DELETE /v1/admin/webhooks/{id}/secrets/{secret_id}` | retire one active secret; `204`. `404 webhook_secret_not_found` when it is not an active secret of that subscription, `409 webhook_last_secret` for the only one |

On the command line:

| Command | Does |
| --- | --- |
| `pyn admin webhook create <name> --url U [--event T,T] [--disabled]` | prints the secret alone on stdout, with a warning on stderr that it is shown once |
| `pyn admin webhook list`, `show <id>` | `list` columns `ID STATE SECRETS EVENTS NAME URL` |
| `pyn admin webhook update <id> [--name N] [--url U] [--event T,T \| --all-events] [--enable \| --disable]` | changes the fields given |
| `pyn admin webhook delete <id>` | removes it |
| `pyn admin webhook rotate <id>` | prints the new secret once |
| `pyn admin webhook retire <id> <secret-id>` | stops signing with that secret |

## URL rules

A URL is checked when a subscription is created or changed:

- `https` only. `http` is accepted only when the operator sets `PYN_WEBHOOKS_ALLOW_HTTP=true`, for development.
- No user name or password, no fragment, ASCII only, at most 2048 bytes, a valid host and port.

These checks look at the text only. Where the name resolves, and refusing loopback, private, link-local and metadata
addresses, is done by the sender at connect time.

## Secrets

The server generates a secret for each subscription: `whsec_` followed by 64 hex characters (32 random bytes). The
whole text is the HMAC key used to sign deliveries. The create or rotate response is the only time the value is ever
returned: it is not in any listing, never logged, and never in an audit entry.

A subscription holds at most two active secrets, so a receiver can switch without a gap:

1. `rotate` adds a new secret. Deliveries carry a signature for each active secret, so a receiver that verifies with
   either keeps working.
2. Switch the receiver to the new secret.
3. `retire` the old one. The last active secret cannot be retired; rotate in a new one first.

A third rotate while two are active is `409 webhook_secret_limit`; retire one first.

### Encryption at rest

By default secrets are stored as plaintext and the server logs a warning at start. Setting `PYN_SECRET_KEY` to 32 random
bytes written as 64 hex characters (for example `openssl rand -hex 32`) stores secrets encrypted with AES-256-GCM, each
with its own random nonce; the stored row records `encrypted` and a `key_id` (the first 4 bytes of the key's SHA-256, in
hex) that names the key without revealing it. An invalid value stops the server at start. Keep the key outside the
database it protects.

Changing the key does not re-encrypt existing secrets; one stored under a different key cannot be read, so rotate and
retire secrets after changing it. Secrets created while no key was set stay plaintext.

## Delivery

Each server runs a delivery loop that claims due deliveries from the outbox (row locks and a two-minute lease, so several
copies never send the same one), signs them and POSTs the event JSON. Set `PYN_WEBHOOK_WORKER=false` on a copy that
should not deliver. In-memory mode delivers from the same process.

Every request carries `Content-Type: application/json` and:

| Header | Value |
| --- | --- |
| `Pyn-Signature` | `t=<unix seconds>,v1=<hex>`, one `v1` per active secret. Each is the HMAC-SHA256 of `<t>.<body>` keyed with the secret text (`whsec_...`). |
| `Pyn-Delivery` | The delivery id; the same on every retry. |
| `Pyn-Event` | The event type. |
| `Pyn-Server` | The server's public URL. |

Verify by recomputing the HMAC over the raw body for any `v1`, comparing in constant time, and rejecting a `t` more than
5 minutes from your clock. A retry is signed again with a fresh `t`.

A 2xx answer settles the delivery. Anything else (another status, a timeout, a refused connection, a redirect) counts as a
failed attempt and is retried with jittered exponential backoff: 8 attempts over about 22 hours, then the delivery is marked
`failed`. A manual redeliver starts again. Rows are kept 30 days.

### Deliveries and redelivery

Each event queued for a subscription is one delivery, identified by the `Pyn-Delivery` id. An administrator (or a
service credential with `manage_webhooks`) can see how they are going and send one again:

| Route | Does |
| --- | --- |
| `GET /v1/admin/webhooks/{id}/deliveries` | Lists the subscription's deliveries, newest event first: `status` (`pending`, `delivered`, `failed`), `attempts`, `next_attempt_at`, `last_status_code`, `last_error`. Filter with `?status=`, page with `?limit=` (default 50, at most 200) and `?before=` (the `next_before` of the previous page, null on the last). |
| `POST /v1/admin/webhooks/{id}/deliveries/{delivery_id}/redeliver` | Puts the delivery back to `pending` with no attempts, due now. It keeps its delivery id, so a receiver that deduplicates sees a repeat, and it is `404 webhook_delivery_not_found` for an id that is not a delivery of that subscription. |

A redeliver works on a delivery in any status, including `delivered` (to replay it) and `failed` (after the receiver is
fixed), and is audited as `webhook_redelivered`. `last_error` is a short text such as `the receiver answered 503` or a
connection error; it never contains the receiver's response body. Deliveries are kept 30 days.

From the CLI: `pyn admin webhook deliveries <id> [--status failed] [--limit N] [--before EVENT]` and
`pyn admin webhook redeliver <id> <delivery id>`.

### What the sender refuses

- Only `https` URLs (`http` when `PYN_WEBHOOKS_ALLOW_HTTP=true`).
- Hosts that resolve only to loopback, private, link-local (including the cloud metadata address), shared (100.64/10),
  reserved or multicast addresses. The address is checked when connecting and the connection is pinned to it, so a name
  that changes its answer between the check and the connection gains nothing.
- Redirects are never followed; no proxy is used.
- Connecting takes at most 5 seconds and the whole request 10; at most 16 KiB of the response is read.
- `PYN_WEBHOOKS_ALLOWED_HOSTS` (comma separated, `example.com` or `*.example.com`), when set, limits deliveries to those
  hosts. It narrows the targets; it never lifts the address rules above.
- Development only: `PYN_WEBHOOKS_ALLOW_LOOPBACK=true` lets the sender reach loopback receivers (`127.0.0.0/8`, `::1`) so a
  local receiver can be tried against a real server. Every other refused range stays refused, the server will not start
  without `PYN_WEBHOOKS_ALLOW_HTTP=true` as well, and it logs a warning at start. Never set it on a server others use.
- `PYN_WEBHOOKS_CONCURRENCY` (default 16) caps sends in flight in one process.

## Audit

Every change is recorded in the server audit log (`GET /v1/admin/audit`) with the actor: `webhook_created`,
`webhook_updated` (naming the fields changed, not their values), `webhook_deleted`, `webhook_secret_rotated`,
`webhook_secret_retired` and `webhook_redelivered` (naming the delivery id). An entry names the subscription's id and name and, for create and delete, the URL's scheme and
host only, because a path or query often carries a token.
