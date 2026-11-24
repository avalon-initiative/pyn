# Configuration

Environment variables for the server and the CLI. Everything is optional for the local demo (`make dev`); a server you
keep needs at least `PYN_DATABASE_URL` and either `PYN_DATA_DIR` or an S3 bucket (`PYN_S3_BUCKET`, below).

| Variable | Meaning |
| --- | --- |
| `PYN_DATABASE_URL` | PostgreSQL connection URL; unset means in-memory metadata |
| `PYN_CREATE_DATABASE` | `true` creates the database if it does not exist |
| `PYN_DATA_DIR` | Directory for uploaded content; unset (and no `PYN_S3_BUCKET`) means in-memory. The server keeps nothing else there. Setting it together with `PYN_S3_BUCKET` stops the server |
| `PYN_INSTANCES` | How many server copies share the database, default 1; above 1 the server refuses to start without `PYN_DATABASE_URL` and shared content storage (`PYN_S3_BUCKET`, or `PYN_DATA_DIR` on a shared volume), and every copy needs the same `PYN_SETUP_TOKEN` until set up |
| `PYN_CONFIG` | Initial policy of every repository created while it is set (stored with the repository), and the fallback of older repositories that have no `.pyn/pyn.toml`; unset means new repositories start shared and older ones are all-exclusive (`make dev` uses `scripts/demo.pyn.toml`) |
| `RUST_LOG` | Log filter in `tracing` syntax (`debug`, `pyn_server=debug,info`); default `info`. An unparsable value falls back to the default |
| `PYN_LOG_FORMAT` | `text` (default) or `json`: json writes one object per line on standard output with the fields in [log fields](#log-fields); any other value stops the server. The host-admin commands (`create-admin` and the like) keep writing logs to standard error whatever the format |
| `PYN_ACCESS_LOG` | On unless `false`: one `info` line per request with target `access`; any other value stops the server. `RUST_LOG` must let the `access` target through (`info` does). Requests to `/healthz` (health checks) are never logged |
| `PYN_ACCESS_LOG_CLIENT` | `true` adds the client address (`X-Forwarded-For` when `PYN_TRUST_FORWARDED_FOR` is on) and the user agent to the access line; default `false`; any other value stops the server |
| `PYN_AUDIT_LOG` | `true` also writes each audit event as a log line with target `audit`, so a log collector gets one stream; default `false`; any other value stops the server |
| `PYN_ADDR` | Listen address, default `127.0.0.1:7878` |
| `PYN_DEV_AUTH` | `true` accepts the `X-Pyn-User` header with every permission; for server tests only, not used by `make dev` |
| `PYN_DEV_AUTH_ALLOW_NON_LOOPBACK` | `true` lets `PYN_DEV_AUTH=true` start on a `PYN_ADDR` that is not loopback; without it the server refuses to start. Never set it on a server others can reach |
| `PYN_DEV_CLOCK_SPEED` | Development only: a number of at least 1 that makes the server's clock run that many times faster than real time, so a lease expires in seconds (`3600` turns the one-hour minimum lease into one second). Refused unless `PYN_DEV_AUTH=true`; unset means the real clock. Sessions and rate-limit windows run on the same clock, so they also expire faster, and a Postgres store takes its time from the server instead of the database, so do not share that database with other copies of the server |
| `PYN_SETUP_TOKEN` | The one-time token that authorises first-run setup, at least 16 characters; unset, the server generates one and prints it on standard error at start. Ignored once the server is set up |
| `PYN_TOKEN` | Token the CLI signs in with, instead of the sign-in saved by `pyn login` (`PYN_USER` with `PYN_DEV_AUTH`). Preferred over `--token`, whose value shows in the process list (the CLI warns); `--token -` reads the token from one line of standard input. The CLI warns on standard error when it sends a token or password over `http://` to a host that is not loopback |
| `PYN_ACCESS` | `private` (default) or `public`: whether signed-out visitors can read public things; the choice made in first-run setup or later by an administrator replaces it. See [server access mode](concepts/access.md#server-access-mode) |
| `PYN_REGISTRATION` | `invite` (default), `open` or `closed`: how people create accounts; the choice made in first-run setup replaces it |
| `PYN_ORG_CREATION` | `anyone` (default) or `admins`: who may create organizations; `admins` limits it to server administrators |
| `PYN_ORG_MEMBER_ADD` | `invite` (default) or `direct`: how adding an organization member works. `invite` creates a pending [invitation](concepts/repositories.md#invitations) the user accepts or declines; `direct` lets a server administrator who owns the organization add without consent (everyone else still invites) |
| `PYN_ORG_INVITATION_DAYS` | Days an organization invitation stays open, default 30; at least 1 |
| `PYN_MAX_LOCKS_ALLOWED_PER_USER` | How many locks one user may hold in a repository, default 5; a repository setting or its `pyn.toml` can override it |
| `PYN_DEFAULT_MAX_REPOSITORIES`, `PYN_DEFAULT_MAX_ORG_MEMBERS`, `PYN_DEFAULT_MAX_STORAGE_BYTES` | Optional defaults for per-owner [limits](concepts/limits.md); unset (the norm for a self-hosted server) means unlimited |
| `PYN_MAX_UNREFERENCED_UPLOAD_BYTES` | Most bytes one repository may hold in uploads no revision names yet, a whole number of bytes, default 34359738368 (32 GiB); `0` refuses every non-empty upload; an invalid value stops the server at start, and the effective value is logged. See [limits](concepts/limits.md#unreferenced-uploads) |
| `PYN_REQUEST_TIMEOUT_SECS` | Seconds a read route (and a read-only `POST`: fetch, merge and policy previews, the missing-objects check) has to answer, default 60; `0` turns the limit off; an answer that is late is `503 request_timeout`. Writes and streamed object routes are not cut off. See [limits](concepts/limits.md#per-request-bounds) |
| `PYN_SHUTDOWN_GRACE_SECS` | Seconds requests already running get to finish after `SIGTERM` or `SIGINT`, default 30, a whole number; `0` stops at once. After it, or on a second signal, the server exits with status 1 and logs how many requests it cut off; a clean stop exits 0. The grace counts from the moment the listener closes, which is after `PYN_SHUTDOWN_DRAIN_DELAY_SECS`. Set the container's stop grace period above delay plus grace. See [deployment](deployment.md#stopping-and-restarting) |
| `PYN_SHUTDOWN_DRAIN_DELAY_SECS` | Seconds the server stays up but not ready after `SIGTERM` or `SIGINT` before it closes its listener, default 0 (close at once), a whole number up to 3600; anything else stops the server at start. During it `/readyz` answers `503 draining`, lock streams end, and every other request is still served, so a balancer that has not yet noticed keeps working. A second signal ends the delay early. Set it a little above the balancer's health check interval times its unhealthy threshold. See [deployment](deployment.md#stopping-and-restarting) |
| `PYN_BODY_IDLE_TIMEOUT_SECS` | Seconds an upload body (object, chunk, assemble, push) may send nothing before the request fails, default 30; `0` turns it off |
| `PYN_MAX_CONCURRENT_EXPENSIVE` | Most fetch, history, verify, snapshot, summary, compare and merge-preview requests running at once, default four times the number of CPUs, at least 1; a request that cannot start within the request timeout is `503 server_busy` |
| `PYN_MAX_UPLOADS_PER_USER`, `PYN_MAX_UPLOADS` | Most uploads one user, and this server process, may run at once, defaults 8 and 64, at least 1; past either an upload is refused with `429 uploads_busy` and a `Retry-After`. Counted per process on purpose, so with several copies each enforces its own. See [limits](concepts/limits.md#concurrent-uploads) |
| `PYN_BRANCH_STALE_DAYS` | Days without a commit after which a branch other than the default counts as stale in the branch list, default 90; at least 1. See [branches](concepts/branches.md#the-branch-list) |
| `PYN_MAX_FORKS_PER_REPO` | Forks one repository may have, default 1000; 0 turns forking off. See [forks](concepts/repositories.md#forks) |
| `PYN_MAX_FORKS_PER_OWNER` | Forks one user or organization may own, default 100; 0 turns forking off. See [forks](concepts/repositories.md#forks) |
| `PYN_MAX_CONCURRENT_MERGES` | Most text merges computed at once across all repositories, default twice the number of CPUs, at least 1; more wait for a free slot. See [merging](concepts/merging.md) |
| `PYN_OIDC_ISSUER`, `PYN_OIDC_CLIENT_ID`, `PYN_OIDC_CLIENT_SECRET` | Optional sign-in with an OpenID Connect provider; unset (the norm) means password only. See [external sign-in](concepts/external-sign-in.md), which also covers `PYN_OIDC_NAME`, `PYN_OIDC_USERNAME_CLAIM`, `PYN_OIDC_CREATE_ACCOUNTS`, `PYN_OIDC_REDIRECT_URL` and `PYN_PASSWORD_SIGN_IN` |
| `PYN_EMAIL_VERIFICATION` | `true` or `false`. Default: on when a mailer is configured (`PYN_MAILER` other than `none`), off otherwise. On: open sign-up needs an email address and the account stays inactive until its link is followed. Off: open sign-up needs no address, as on an invite server. `true` without a mailer stops the server at start, because the links could never arrive. Any other value stops the server. `GET /v1/registration` reports what is in force |
| `PYN_REQUIRE_APPROVAL` | `true` holds new open sign-ups until a server administrator approves them |
| `PYN_SECRET_KEY` | 32 random bytes as 64 hex characters (`openssl rand -hex 32`): encrypts webhook signing secrets and [two-factor](concepts/access.md#two-factor-authentication) seeds at rest with AES-256-GCM. Unset, they are stored as plaintext and the server warns at start; an invalid value stops the server. See [webhooks](concepts/webhooks.md#encryption-at-rest) |
| `PYN_NEW_SECRET_KEY` | Read only by `pyn-server rotate-secret-key`: the key to re-encrypt webhook secrets and two-factor seeds under. See [recovery](deployment.md#lost-secret-key) |
| `PYN_WEBHOOKS_ALLOW_HTTP` | `true` lets webhook subscription URLs use `http` (development only); default `false`, https only; any other value stops the server |
| `PYN_WEBHOOKS_ALLOW_LOOPBACK` | `true` lets the webhook sender reach loopback receivers (`127.0.0.0/8`, `::1`), for local development and end-to-end tests only; private, link-local and metadata ranges stay refused. The server refuses to start unless `PYN_WEBHOOKS_ALLOW_HTTP=true` is set too, and logs a warning at start. Default `false`; any other value stops the server |
| `PYN_WEBHOOK_WORKER` | `false` stops this server copy from delivering queued webhooks; default `true`; any other value stops the server. See [delivery](concepts/webhooks.md#delivery) |
| `PYN_WEBHOOKS_ALLOWED_HOSTS` | Comma separated hosts (`example.com`, `*.example.com`); when set, webhooks are delivered only to them. It never allows private addresses |
| `PYN_WEBHOOKS_CONCURRENCY` | Webhook sends in flight at once in one server; default 16, at least 1 |
| `PYN_UPDATE_CHECK` | `on` or `off` (default): `on` makes the server ask `api.github.com` about once a day for the latest pyn releases, so administrators can see whether a newer one exists (`GET /v1/admin/update`); off, nothing is contacted. Any other value stops the server |
| `PYN_PUBLIC_URL` | Where the web app lives, for links in emails and the `Secure` flag of cookies (an `https://` address sets it); default `http://<PYN_ADDR>`; the address given in first-run setup replaces it |
| `PYN_CERT_CHECK_INTERVAL_SECS` | How often the server handshakes with the host of an `https://` `PYN_PUBLIC_URL` to read the TLS certificate's expiry and report it in `GET /v1/admin/settings` as `certificate`; default 21600 (6 hours), `0` turns it off, otherwise 60 to 604800 (anything else stops the server at start). Nothing is checked for an http address or a localhost or loopback host. See [deployment](deployment.md#the-server-watches-the-certificate) |
| `PYN_WEBAUTHN_RP_ID` | The WebAuthn relying-party id for [passkeys](concepts/access.md#passkeys-and-security-keys) when it is not `PYN_PUBLIC_URL`'s host: a parent domain of that host (`example.com` for `https://git.example.com`). Default: the host. Passkeys need a public address with a host name over `https://` (or `localhost`); without one the server offers authenticator apps only. An id that is not a parent of the host turns passkeys off and is logged at start |
| `PYN_ALLOWED_ORIGINS` | Comma-separated origins (`scheme://host[:port]`, no path) besides `PYN_PUBLIC_URL`'s that may sign in and send cookie-authenticated writes, such as the Vite dev server (`make dev` allows `http://localhost:5173` and `http://127.0.0.1:5173`); default none; an invalid entry stops the server at start. See [access](concepts/access.md#web-sessions) |
| `PYN_MAILER` | How the server sends email: `none` (default: nothing is sent), `log` (testing only: the whole message, links included, is written to the server log under the marker `PYN-MAILER-LOG`, nothing is delivered) or `smtp`. Any other value stops the server. See [email](deployment.md#email) |
| `PYN_MAILER_LOG_ALLOW_NON_LOOPBACK` | `true` lets `PYN_MAILER=log` start on a `PYN_ADDR` that is not loopback; without it the server refuses to start, because the verification links in the log take over accounts. The server also logs a warning at every start with `log` on. Never set it on a server whose log others can read |
| `PYN_SMTP_HOST`, `PYN_SMTP_FROM` | Needed with `PYN_MAILER=smtp`: the SMTP server, and the sender address (`noreply@example.org` or `pyn <noreply@example.org>`) |
| `PYN_SMTP_PORT` | Default 587 (use 465 with `PYN_SMTP_TLS=tls`) |
| `PYN_SMTP_USER`, `PYN_SMTP_PASSWORD` | Credentials for SMTP AUTH; set both or neither. The password is never logged, printed or returned by the API |
| `PYN_SMTP_TLS` | `starttls` (default: upgrade the connection, and refuse to send when the server will not), `tls` (encrypted from the first byte) or `none` (no encryption, and credentials travel in the clear: a trusted local relay only). Certificates are checked against the bundled public roots, so a server with a private certificate authority needs `none` on a trusted network or a relay with a public certificate |
| `PYN_RATE_SIGN_IN_ACCOUNT`, `PYN_RATE_SIGN_IN_CLIENT`, `PYN_RATE_REGISTER_CLIENT`, `PYN_RATE_MAIL_PER_EMAIL`, `PYN_RATE_PEOPLE_CLIENT` | Rate limits, defaults 5, 30, 10, 3 and 60 (the last is signed-out people directory requests per client address per minute; the first also limits wrong current passwords when changing a password); see [access](concepts/access.md#protecting-open-registration) |
| `PYN_RATE_SERVICE_CREDENTIAL` | Requests per service credential per minute, default `0` (no limit); a credential's own `rate_limit_per_minute` overrides it; see [service credentials](concepts/access.md#hardening-a-service-credential) |
| `PYN_TRUST_FORWARDED_FOR` | `true` takes the client address from `X-Forwarded-For` (the last entry, or the one `PYN_TRUSTED_PROXIES` positions from the right), for rate limits and service credential source restrictions; only behind a proxy that sets it; also trusts `X-Forwarded-Host` and `X-Forwarded-Proto` for the [LFS](concepts/lfs.md) URLs |
| `PYN_TRUSTED_PROXIES` | How many proxies append to `X-Forwarded-For` in front of the server, a whole number from 1 to 16, default 1. The client is the entry that many positions from the right (1 is the last entry); when the header has fewer entries, or that entry is empty, the connection's peer is used. Several header lines count as one list in order. Only with `PYN_TRUST_FORWARDED_FOR=true`: set without it, the server stops at start. See [deployment](deployment.md#public-address-and-client-addresses-per-layout) |
| `PYN_PASSWORD_HASHES` | Most password hashes at once, default the number of CPUs |
| `PYN_SESSION_DAYS` | How long a `pyn login` lasts, default 30 |
| `PYN_SESSION_REMEMBER_DAYS` | How long a web session lasts when the person chose "stay signed in", default 90; never shorter than `PYN_SESSION_DAYS` (a smaller value is raised to it). See [access](concepts/access.md#web-sessions) |
| `PYN_ACCOUNT_DELETION_WINDOW_DAYS` | Days a self-service (or scheduled) [account deletion](concepts/access.md#deleting-an-account) can be undone, default 30; `0` purges at the next pass; administrators read the effective value at `GET /v1/admin/settings` |
| `PYN_DISABLED_ACCOUNT_RETENTION_DAYS` | Days a disabled account is kept before the server deletes it, default 90; `0` turns automatic deletion off; administrators read the effective value at `GET /v1/admin/settings` |
| `PYN_CONFIG_DIR` | Where the CLI keeps `credentials.toml`, default `~/.config/pyn`; the directory is made `0700` and the file `0600`, and a file that cannot be parsed is an error, never replaced |
| `PYN_CLI_CONFIG` | The CLI's user settings file, default `~/.config/pyn/config.toml` (not the server's `PYN_CONFIG`); `pyn config init` creates it, and the first command in a terminal offers to |
| `PYN_REPO` | The repository (`owner/name`) the CLI acts on, unless a workspace or `--repo` says otherwise |

`pyn-server --healthcheck` does not start a server: it requests `/healthz` on `PYN_ADDR` (an unspecified address such as `0.0.0.0` is probed on loopback) with a 3 second timeout and exits 0 if it answers, non-zero otherwise. It never touches the database, so a container image can use it as its healthcheck without `curl`.

`pyn-server migrate-objects` copies the object store between a data directory and the bucket named by the `PYN_S3_*` variables below (in either direction) and exits; it takes no new variables. It reads `PYN_S3_*` for the bucket side and the directory from its own `--from-data-dir` / `--to-data-dir` flag (it does not read `PYN_DATA_DIR`, so both can be set up for the move), and `PYN_ADDR` to notice a running server. See [moving the object store](deployment.md#moving-the-object-store).

`pyn-server --help` and `pyn-server --version` print the usage and the version without starting anything, and an unknown argument is refused. `pyn-server recover-admin <username> [--password-stdin]` and `pyn-server rotate-secret-key` are host-level recovery commands that work on the database in `PYN_DATABASE_URL` and exit; see [recovery](deployment.md#recovery).

`make test-live` runs the tests that need a database (`PYN_DATABASE_URL`); each uses its own temporary schema.

`make e2e` runs the end-to-end suites (`crates/pyn-cli/tests/e2e.rs` and `e2e_second.rs`): it spawns the real `pyn-server` (in-memory store) and drives the real `pyn` binary through init, clone, locks, policy changes, add and commit, a plain git repository, branches and merge, `verify`, policy preview and `local_store` commits pushed and pulled between clones, on temporary folders. It is part of `make test` too, and needs `git` on the path only as an oracle.

> **Not for production.** Never run with `PYN_DEV_AUTH=true` on a network you do not control.

## S3-compatible object store

Setting `PYN_S3_BUCKET` keeps file content in a bucket of any S3-compatible service (AWS S3, DigitalOcean Spaces, Cloudflare R2, MinIO) instead of `PYN_DATA_DIR`; see [storage](concepts/storage.md#s3-compatible-store) and [several copies on a bucket](deployment.md#several-copies-on-a-bucket). Settings are checked at start and a bad one stops the server; an error names the variable and never prints a secret or the endpoint. An empty value counts as unset. Any `PYN_S3_*` variable without `PYN_S3_BUCKET` is an error, and so is `PYN_S3_BUCKET` together with `PYN_DATA_DIR`.

| Variable | Meaning |
| --- | --- |
| `PYN_S3_BUCKET` | The bucket name; selects the S3 store. The endpoint, the access key id and the secret access key are then required |
| `PYN_S3_ENDPOINT` | The service address: `https://` (for example `https://nyc3.digitaloceanspaces.com`), or `http://` for a local server. No user name or password in it, and no bucket name unless you use `PYN_S3_PATH_STYLE` |
| `PYN_S3_REGION` | The region requests are signed for, default `us-east-1`. Spaces and R2 take their own region codes (`nyc3`, `auto`) |
| `PYN_S3_ACCESS_KEY_ID`, `PYN_S3_SECRET_ACCESS_KEY` | The credentials. Each has a `_FILE` variant (`PYN_S3_ACCESS_KEY_ID_FILE`, `PYN_S3_SECRET_ACCESS_KEY_FILE`) naming a file that holds the value, for Docker and Kubernetes secrets; one trailing newline is dropped. Setting both a variable and its `_FILE` variant, an unreadable file, or an empty one stops the server (the error names the variable and the path, never the contents) |
| `PYN_S3_PREFIX` | Optional key prefix, so one bucket can hold several installations; a trailing `/` is added when missing |
| `PYN_S3_PATH_STYLE` | `true` or `false` (default): `false` addresses `https://<bucket>.<host>/<key>` (virtual-host), `true` addresses `https://<host>/<bucket>/<key>`. Use `true` for MinIO and other servers without wildcard DNS |
| `PYN_S3_SPOOL_DIR` | Where content over the threshold waits while it is hashed before upload. Default: a `pyn-s3-spool` folder in the system temporary directory (`PYN_DATA_DIR` cannot be set with a bucket, so it plays no part). In the Compose image `/tmp` is memory-backed; point this at a volume if large files are expected |
| `PYN_S3_SPOOL_THRESHOLD` | Bytes held in memory per upload before spilling to the spool directory, default 8388608 (8 MiB) |
| `PYN_S3_REQUEST_TIMEOUT_SECS` | Whole-request limit in seconds for calls without a large body, default 30; at least 1 |
| `PYN_S3_MAX_RETRIES` | Times a throttled or failed request is tried again after the first, default 4; 0 to 20 |

At start the server also sweeps spool files left by a process that died and runs a check: it writes, reads back and deletes a small marker object under the prefix, and stops with a message naming the failed operation and its status if any step fails. While running, the check repeats every 30 seconds, and `GET /v1/admin/settings` reports `storage.status` as `degraded` while it fails; the server keeps running and `GET /healthz` is unchanged (it is a liveness check).

## Checking a configuration

`pyn-server --check-config` runs the same start-up parsing and cross-checks as the real start, without starting anything, and reports every problem at once instead of the first. Run it with the environment you intend to start with, for example before `docker compose up`:

```bash
docker compose run --rm --no-deps server --check-config
```

`--no-deps` keeps Compose from starting the database. Outside Compose, run `pyn-server --check-config` with the same `PYN_*` variables set (a native install: the variables of the service's environment file).

It reads the environment, and the files the real start reads: the policy file named by `PYN_CONFIG` and the `_FILE` secrets of the S3 settings. It checks that every value has the right shape and range, and every combination the server enforces: the S3 settings against `PYN_DATA_DIR`, `PYN_INSTANCES` above 1 against a database and shared storage, `PYN_TRUSTED_PROXIES` against `PYN_TRUST_FORWARDED_FOR`, the shutdown, certificate and request limits, the mailer and its SMTP settings, sign-in and OpenID Connect settings, the setup token, and the development-only switches against `PYN_ADDR`. It never contacts anything and opens no port.

Each problem is one line on standard output, naming the variable and what is expected (`PYN_S3_PATH_STYLE must be true or false`). A secret is never printed: for `PYN_SECRET_KEY`, `PYN_SETUP_TOKEN`, `PYN_SMTP_PASSWORD`, the S3 keys, `PYN_OIDC_CLIENT_SECRET` and the user information in `PYN_DATABASE_URL` or `PYN_S3_ENDPOINT` the output says only what is wrong with the value, never the value. When the configuration is clean it prints `configuration ok` and exits 0; otherwise it prints `N problem(s) found` and exits 1. The last line always lists what was not checked, because it needs the network or the filesystem: that the database is reachable, that the bucket exists and its keys work, that `PYN_DATA_DIR` is writable, and that the address is free to bind.

Every start, and `--check-config`, also reports each environment variable that begins with `PYN_` and that the server does not read, with a "did you mean" suggestion when a known name is within two edits (a misspelt `PYN_PUBLIC_URL` gets `did you mean PYN_PUBLIC_URL?`). In `--check-config` the line starts with `warning:`; at start it is a log warning. It never stops the server. Variables owned by other parts of pyn that share the environment (the CLI's `PYN_TOKEN`, `PYN_SERVER` and `PYN_REPO`, and the Compose, web container and installer variables such as `PYN_SERVER_VERSION`, `PYN_SITE` and `PYN_CADDY_IMAGE`) are not reported.

## Log fields

With `PYN_LOG_FORMAT=json` every line is one flat JSON object. The names below are a stable contract; fields are only
added, and an absent value leaves the field out rather than writing `null`.

Every line: `timestamp` (RFC 3339), `level`, `target`, `message`, and, for a line written while a request is handled,
`span.request_id` (the request id; see below).

The access line (`target` `access`, `message` `request`):

| Field | Meaning |
| --- | --- |
| `request_id` | The request id, also returned in the `X-Request-Id` response header |
| `method` | HTTP method |
| `route` | The matched route template, such as `/v1/repos/{owner}/{name}/audit`; `unmatched` when no route matched. Never the raw path |
| `status` | HTTP status code |
| `latency_ms` | Milliseconds until the response headers were ready; a streamed body is not included |
| `response_bytes` | Response body size, present when known up front |
| `account` | The signed-in account, present when the request authenticated |
| `repo` | `owner/name`, present when the route names a repository |
| `client_ip`, `user_agent` | Only with `PYN_ACCESS_LOG_CLIENT=true` |

The audit line (`target` `audit`, `message` `audit`, only with `PYN_AUDIT_LOG=true`): `scope` (`owner/name`,
`@org:<name>` or the server), `actor`, `acting_admin` (only when an administrator acted as `actor` in an
[access session](concepts/access.md#administrator-access-sessions)), `action` and `path` when the event has one. The event's free-text detail is not
written; read it through the audit API.

Never logged: credentials (tokens, cookies, passwords), query strings, request or response bodies, email addresses and
avatar content.

### Request ids

A caller may send `X-Request-Id`: 1 to 128 visible ASCII characters (no spaces or control characters). The server uses
it as is; anything else is replaced by a generated 32 character hex id. The id is returned in the response header and
carried by every log line of the request, so a proxy's id can be followed through the server.
