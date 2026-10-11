# Running several server copies

Audit of in-process state, for the planned shared multi-tenant server (decision #140). The question for each item is
whether it stays correct when several server processes share one PostgreSQL database. Findings are tracked as issues; this
page records the current state. Provisional: it reflects the code at the time of the audit.

## Safe with several copies on Postgres

| State | Where it lives | Why it is safe |
| --- | --- | --- |
| Locks and leases | `locks` table | Acquire is one atomic upsert; the per-user count is serialised by an advisory transaction lock; expired locks are filtered on read, so no sweeper exists. |
| Rate limits | `rate_limits` table | One atomic upsert per hit. |
| Sessions and CSRF | `sessions` table | The CSRF token is stored in the session row. |
| OpenID Connect flows | `oidc_flows` table | Single-use rows, so the callback may land on any copy. Discovery and key caches are per process and refetch on expiry. |
| Audit events | `audit_events` table | There is no live push or fan-out. |
| Cleanup of expired sessions, flows, pending accounts | Lazy, inside requests | Idempotent deletes; there are no background tasks. With no traffic nothing is swept, which costs storage only. |
| Organization deleting mark | Database | Atomic compare-and-set. |
| Migrations | Startup | sqlx takes an advisory lock, so concurrent starts serialise. |
| Repository policy cache | Per process | Revalidated against the head of `.pyn/pyn.toml` on each lookup (#181). |
| Setup settings | Per process | Re-read from the database every minute (#182). |
| Password hashing limit | Per process | Holds no correctness state. |

## Not safe yet

| State | Problem | Issue |
| --- | --- | --- |
| Setup token | One generated token per process unless `PYN_SETUP_TOKEN` is shared; any one completes setup. Documented in [access](access.md). | #182 |
| Object store | The filesystem store needs a shared volume; the in-memory store is single-process. | #183 |
| Clock use | Lease expiry and rate-limit windows use each process's clock. | #183 |
| In-memory stores | Selected when `PYN_DATABASE_URL` is unset; single-process and not durable. Development and tests only. | #183 |
