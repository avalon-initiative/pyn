# Running several server copies

Audit of in-process state, for a shared multi-tenant server (decision #140). The question for each item is
whether it stays correct when several server processes share one PostgreSQL database. Findings are tracked as issues; this
page records the current state, as of 2026-10-30.

## Safe with several copies on Postgres

| State | Where it lives | Why it is safe |
| --- | --- | --- |
| Locks and leases | `locks` table | Acquire is one atomic upsert; the per-user count is serialised by an advisory transaction lock; expired locks are filtered on read, so no sweeper exists. |
| Rate limits | `rate_limits` table | One atomic upsert per hit. |
| Sessions and CSRF | `sessions` table | The CSRF token is stored in the session row. |
| OpenID Connect flows | `oidc_flows` table | Single-use rows, so the callback may land on any copy. Discovery and key caches are per process and refetch on expiry. |
| Audit events | `audit_events` table | There is no live push or fan-out. |
| Cleanup of expired sessions, flows, pending accounts | Lazy, inside requests | Idempotent deletes; no background task does this cleanup. With no traffic nothing is swept, which costs storage only. |
| Organization deleting mark | Database | Atomic compare-and-set. |
| Migrations | Startup | sqlx takes an advisory lock, so concurrent starts serialise. |
| Repository policy cache | Per process | Revalidated against the head of `.pyn/pyn.toml` on each lookup (#181). |
| Setup settings | Per process | Re-read from the database every minute (#182). |
| Lease and rate-limit clock | Database `now()` | With Postgres, expiry is judged by the database clock, so skewed copies agree; the in-memory stores use the process clock. Other expiries (sessions, invites, tokens) still use the process clock, so keep the copies in sync (NTP). |
| Webhook delivery | `webhook_events` and `webhook_deliveries` tables | Events are queued in the transaction of the change. Each copy's delivery loop claims due rows with row locks and a two-minute lease, so no delivery is sent by two copies at once; a copy that dies mid-send loses its lease and another picks the row up. Due times, leases, retention and the throttle for `owner.limit_reached` use the database `now()`. A crash between a send and its recording can send twice, which is why receivers deduplicate on `Pyn-Delivery` ([events](events.md#receiving-events)). `PYN_WEBHOOK_WORKER=false` keeps a copy out of delivery (for example web-only copies); at least one copy must run it, or events queue and nothing is sent. The loop and its outbound-request cap (`PYN_WEBHOOKS_CONCURRENCY`) are per process, so total concurrency is the cap times the copies delivering. |
| Account deletion task | `account_deletions`, `name_reservations` tables, disabled times on `users` | Every copy runs the hourly pass. It reads due schedules from the database and each purge is one transaction, so two copies purging the same account never both succeed: the second finds it gone and logs a refusal. The timers use the copy's clock, so keep clocks in sync ([deleting an account](access.md#deleting-an-account)). |
| Rolling restarts and deploys | `SIGTERM` handling, `GET /readyz` | A copy that gets a stop signal turns `/readyz` to `503 draining`, stops accepting connections, lets running requests finish for `PYN_SHUTDOWN_GRACE_SECS` (default 30), ends lock streams so clients reconnect to another copy, stops its background loops and exits 0 (1 if requests were cut off). `/readyz` also fails while the metadata store does not answer or the object store failed its probe, so a balancer can keep traffic off a copy that cannot serve it; `/healthz` stays a pure liveness check. Restart copies one at a time so the others keep serving. See [deployment](../deployment.md#stopping-and-restarting). |
| Password hashing limit | Per process | Holds no correctness state. |
| Upload and expensive-request limits | Per process, on purpose | None of them is a rate. Each bounds how many requests run at once so one process keeps its memory and CPU in check: `PYN_MAX_CONCURRENT_EXPENSIVE` (fetch, history, verify, snapshot, summary, compare, merge preview), `PYN_MAX_UPLOADS` and `PYN_MAX_UPLOADS_PER_USER` (uploads running at once), `PYN_MAX_CONCURRENT_MERGES` and the password hashing limit. A copy's load is its own to bound, so sharing the count would add a database round trip per request and a leaked slot whenever a copy dies, for no gain in safety: N copies carry N times the cap by design, as N times the hardware does. The limits that must hold across copies are shared already: the open resumable uploads per owner (counted in `tus_sessions` under an advisory lock), the unreferenced-upload ceiling per repository (an atomic database hold, see [limits](limits.md#unreferenced-uploads)) and the rate windows above. No upload or expensive route has a hits-per-window limit. |

## Proven by a test with two copies

`crates/pyn-server/tests/two_copies.rs` and `crates/pyn-cli/tests/two_copies.rs` start two real `pyn-server` processes on one PostgreSQL schema and one S3 bucket (`PYN_INSTANCES=2`, the same `PYN_SECRET_KEY` and `PYN_SETUP_TOKEN`) and check from the outside. They need `PYN_DATABASE_URL`, so `make test-live` runs them.

- Setup done on one copy is seen by the other at once (setup status, root sign-in, admin settings), as are accounts registered on either.
- A token or a cookie session made on one copy is accepted by the other.
- A lock taken on one copy is refused to another user on the other copy (`lock_held`) and listed there; a release on one copy frees the path on the other.
- A lock stream request held open on one copy wakes for a lock taken on the other (events are read from the database, polled twice a second).
- A resumable upload started on one copy continues and finishes on the other, and the object downloads from both.
- A push through one copy is cloned through the other, and the objects land in the bucket.
- `SIGTERM` to one copy exits 0 while the other keeps answering `/readyz` and API calls.
- A copy whose bucket stops answering turns `/readyz` to 503 (within the 30 second probe interval) while `/healthz` stays 200.

## Not safe yet

| State | Problem | Issue |
| --- | --- | --- |
| Setup token | One generated token per process unless `PYN_SETUP_TOKEN` is shared; any one completes setup. Documented in [access](access.md). | #182 |
| Filesystem and in-memory object stores | Single-process or not durable. `pyn-server migrate-objects` moves an existing filesystem store into a bucket and back (see [deployment](../deployment.md#moving-the-object-store)). Setting `PYN_INSTANCES` above 1 makes the server refuse to start without `PYN_DATABASE_URL` and shared content storage: `PYN_S3_BUCKET` (every copy shares the bucket, see [deployment](../deployment.md#several-copies-on-a-bucket)), or `PYN_DATA_DIR`, which must then be shared storage. | #183 |
