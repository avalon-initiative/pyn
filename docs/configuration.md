# Configuration

Environment variables for the server and the CLI. Everything is optional for the local demo (`make dev`); a server you
keep needs at least `PYN_DATABASE_URL` and `PYN_DATA_DIR`.

| Variable | Meaning |
| --- | --- |
| `PYN_DATABASE_URL` | PostgreSQL connection URL; unset means in-memory metadata |
| `PYN_CREATE_DATABASE` | `true` creates the database if it does not exist |
| `PYN_DATA_DIR` | Directory for uploaded content; unset means in-memory |
| `PYN_INSTANCES` | How many server copies share the database, default 1; above 1 the server refuses to start without `PYN_DATABASE_URL` and `PYN_DATA_DIR` (a shared volume), and every copy needs the same `PYN_SETUP_TOKEN` until set up |
| `PYN_CONFIG` | Fallback policy for repositories that have no `.pyn/pyn.toml` yet; unset means everything is exclusive (`make dev` uses `scripts/demo.pyn.toml`) |
| `PYN_ADDR` | Listen address, default `127.0.0.1:7878` |
| `PYN_DEV_AUTH` | `true` accepts the `X-Pyn-User` header with every permission; for server tests only, not used by `make dev` |
| `PYN_SETUP_TOKEN` | The one-time token that authorises first-run setup, at least 16 characters; unset, the server generates one and prints it on standard error at start. Ignored once the server is set up |
| `PYN_TOKEN` | Token the CLI signs in with, instead of the sign-in saved by `pyn login` (`PYN_USER` with `PYN_DEV_AUTH`). Preferred over `--token`, whose value shows in the process list (the CLI warns); `--token -` reads the token from one line of standard input. The CLI warns on standard error when it sends a token or password over `http://` to a host that is not loopback |
| `PYN_REGISTRATION` | `invite` (default), `open` or `closed`: how people create accounts; the choice made in first-run setup replaces it |
| `PYN_ORG_CREATION` | `anyone` (default) or `admins`: who may create organizations; `admins` limits it to server administrators |
| `PYN_MAX_LOCKS_ALLOWED_PER_USER` | How many locks one user may hold in a repository, default 5; a repository setting or its `pyn.toml` can override it |
| `PYN_DEFAULT_MAX_REPOSITORIES`, `PYN_DEFAULT_MAX_ORG_MEMBERS`, `PYN_DEFAULT_MAX_STORAGE_BYTES` | Optional defaults for per-owner [limits](concepts/limits.md); unset (the norm for a self-hosted server) means unlimited |
| `PYN_MAX_UNREFERENCED_UPLOAD_BYTES` | Most bytes one repository may hold in uploads no revision names yet, a whole number of bytes, default 34359738368 (32 GiB); `0` refuses every non-empty upload; an invalid value stops the server at start, and the effective value is logged. See [limits](concepts/limits.md#unreferenced-uploads) |
| `PYN_REQUEST_TIMEOUT_SECS` | Seconds a read route (and a read-only `POST`: fetch, merge and policy previews, the missing-objects check) has to answer, default 60; `0` turns the limit off; an answer that is late is `503 request_timeout`. Writes and streamed object routes are not cut off. See [limits](concepts/limits.md#per-request-bounds) |
| `PYN_BODY_IDLE_TIMEOUT_SECS` | Seconds an upload body (object, chunk, assemble, push) may send nothing before the request fails, default 30; `0` turns it off |
| `PYN_MAX_CONCURRENT_EXPENSIVE` | Most fetch, history, verify, snapshot, summary and merge-preview requests running at once, default four times the number of CPUs, at least 1; a request that cannot start within the request timeout is `503 server_busy` |
| `PYN_MAX_UPLOADS_PER_USER`, `PYN_MAX_UPLOADS` | Most uploads one user, and this server process, may run at once, defaults 8 and 64, at least 1; past either an upload is refused with `429 uploads_busy` and a `Retry-After`. Counted per process, so with several copies each enforces its own. See [limits](concepts/limits.md#concurrent-uploads) |
| `PYN_MAX_CONCURRENT_MERGES` | Most text merges computed at once across all repositories, default twice the number of CPUs, at least 1; more wait for a free slot. See [merging](concepts/merging.md) |
| `PYN_OIDC_ISSUER`, `PYN_OIDC_CLIENT_ID`, `PYN_OIDC_CLIENT_SECRET` | Optional sign-in with an OpenID Connect provider; unset (the norm) means password only. See [external sign-in](concepts/external-sign-in.md), which also covers `PYN_OIDC_NAME`, `PYN_OIDC_USERNAME_CLAIM`, `PYN_OIDC_CREATE_ACCOUNTS`, `PYN_OIDC_REDIRECT_URL` and `PYN_PASSWORD_SIGN_IN` |
| `PYN_EMAIL_VERIFICATION` | On unless `false`: open sign-up needs an email address and the account stays inactive until its link is followed |
| `PYN_REQUIRE_APPROVAL` | `true` holds new open sign-ups until a server administrator approves them |
| `PYN_SECRET_KEY` | 32 random bytes as 64 hex characters (`openssl rand -hex 32`): encrypts webhook signing secrets at rest with AES-256-GCM. Unset, they are stored as plaintext and the server warns at start; an invalid value stops the server. See [webhooks](concepts/webhooks.md#encryption-at-rest) |
| `PYN_WEBHOOKS_ALLOW_HTTP` | `true` lets webhook subscription URLs use `http` (development only); default `false`, https only; any other value stops the server |
| `PYN_WEBHOOK_WORKER` | `false` stops this server copy from delivering queued webhooks; default `true`; any other value stops the server. See [delivery](concepts/webhooks.md#delivery) |
| `PYN_WEBHOOKS_ALLOWED_HOSTS` | Comma separated hosts (`example.com`, `*.example.com`); when set, webhooks are delivered only to them. It never allows private addresses |
| `PYN_WEBHOOKS_CONCURRENCY` | Webhook sends in flight at once in one server; default 16, at least 1 |
| `PYN_PUBLIC_URL` | Where the web app lives, for links in emails and the `Secure` flag of cookies (an `https://` address sets it); default `http://<PYN_ADDR>`; the address given in first-run setup replaces it |
| `PYN_ALLOWED_ORIGINS` | Comma-separated origins (`scheme://host[:port]`, no path) besides `PYN_PUBLIC_URL`'s that may sign in and send cookie-authenticated writes, such as the Vite dev server (`make dev` allows `http://localhost:5173` and `http://127.0.0.1:5173`); default none; an invalid entry stops the server at start. See [access](concepts/access.md#web-sessions) |
| `PYN_EMAIL` | How email is sent; only `log` (notes that a message was queued, without its body) exists |
| `PYN_EMAIL_LOG_BODIES` | `true` also logs each message body, links included; for local development only, the server warns at startup |
| `PYN_RATE_SIGN_IN_ACCOUNT`, `PYN_RATE_SIGN_IN_CLIENT`, `PYN_RATE_REGISTER_CLIENT`, `PYN_RATE_MAIL_PER_EMAIL` | Rate limits, defaults 5, 30, 10 and 3 (the first also limits wrong current passwords when changing a password); see [access](concepts/access.md#protecting-open-registration) |
| `PYN_TRUST_FORWARDED_FOR` | `true` takes the client address from the last `X-Forwarded-For` entry; only behind a proxy that sets it |
| `PYN_PASSWORD_HASHES` | Most password hashes at once, default the number of CPUs |
| `PYN_SESSION_DAYS` | How long a `pyn login` lasts, default 30 |
| `PYN_CONFIG_DIR` | Where the CLI keeps `credentials.toml`, default `~/.config/pyn`; the directory is made `0700` and the file `0600`, and a file that cannot be parsed is an error, never replaced |
| `PYN_CLI_CONFIG` | The CLI's user settings file, default `~/.config/pyn/config.toml` (not the server's `PYN_CONFIG`); `pyn config init` creates it, and the first command in a terminal offers to |
| `PYN_REPO` | The repository (`owner/name`) the CLI acts on, unless a workspace or `--repo` says otherwise |

`make test-live` runs the tests that need a database (`PYN_DATABASE_URL`); each uses its own temporary schema.

`make e2e` runs the end-to-end suites (`crates/pyn-cli/tests/e2e.rs` and `e2e_second.rs`): it spawns the real `pyn-server` (in-memory store) and drives the real `pyn` binary through init, clone, locks, policy changes, add and commit, a plain git repository, branches and merge, `verify`, policy preview and `local_store` commits pushed and pulled between clones, on temporary folders. It is part of `make test` too, and needs `git` on the path only as an oracle.

> **Not for production.** Never run with `PYN_DEV_AUTH=true` on a network you do not control.
