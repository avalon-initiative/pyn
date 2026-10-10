# pyn

Version control that merges what can be merged and locks what shouldn't be. Git-style collaboration for files that
can be merged, Perforce-style exclusive locks (server-enforced, leased locks) for files that shouldn't be edited
concurrently. Which is which is set per path in [`pyn.toml`](docs/concepts/pyn-toml.md).

Local proof of concept, Phase 1 only (single mainline per repository, shared + exclusive paths, leased locks,
lock/checkin, log), on a server that hosts many [repositories](docs/concepts/repositories.md). This repo holds the server, the `pyn` command-line client, and the shared libraries. The web UI is
`pyn-web` (a separate repo).

## Run it

One command brings up a server with a seeded demo (in memory, so nothing to install):

```bash
make dev                 # builds, starts the server in the background, creates accounts, repositories, files and locks
```

It prints the URL and the sign-ins: `root` / `demo-password` (owns `root/demo`), `alice` / `alice-password` and
`bob` / `bob-password` (`bob` owns the public `bob/tools`). `root/demo` has nested folders, the policy in
[`scripts/demo.pyn.toml`](scripts/demo.pyn.toml) (shared `Source/`, `docs/`; exclusive `Content/`, `*.uasset`) and four
locks held by alice and bob. `make dev` is safe to repeat, `make stop` stops the server and `make demo` re-seeds a server that is
already running. Log: `_running/logs/pyn.log`. Set `PYN_ADDR` for another port and `PYN_DATABASE_URL` (via `.env`) to keep the data in PostgreSQL.

Drive it from the CLI; `make dev` keeps each account's sign-in under `_running/demo-config/<user>`:

```bash
export PYN_SERVER=http://127.0.0.1:7878 PYN_REPO=root/demo PYN_CONFIG_DIR=_running/demo-config/bob
./target/debug/pyn ls Content          # modes, last change and locks, one folder at a time
./target/debug/pyn summary
./target/debug/pyn lock Content/World/Main.umap # lock_held (409): alice has it
```

And in the browser, with the web app from the `pyn-web` repo (it proxies `/v1` to `http://127.0.0.1:7878`, or to `PYN_API` if set):

```bash
cd ../pyn-web && npm ci && npm run dev    # http://localhost:5173, sign in as alice or bob
```

For your own server instead of the demo: `cp .env.example .env`, then `make run` (foreground, reads `.env`). A new
server is uninitialised and prints a one-time setup token in its log; `pyn setup <name> --setup-token <token>` (or `POST /v1/setup`)
creates the first administrator, then `pyn login <name>`. See [first-run setup](docs/concepts/access.md#first-run-setup).

| Variable | Meaning |
| --- | --- |
| `PYN_DATABASE_URL` | PostgreSQL connection URL; unset means in-memory metadata |
| `PYN_CREATE_DATABASE` | `true` creates the database if it does not exist |
| `PYN_DATA_DIR` | Directory for uploaded content; unset means in-memory |
| `PYN_CONFIG` | Fallback policy for repositories that have no `.pyn/pyn.toml` yet; unset means everything is exclusive (`make dev` uses `scripts/demo.pyn.toml`) |
| `PYN_ADDR` | Listen address, default `127.0.0.1:7878` |
| `PYN_DEV_AUTH` | `true` accepts the `X-Pyn-User` header with every permission; for server tests only, not used by `make dev` |
| `PYN_SETUP_TOKEN` | The one-time token that authorises first-run setup, at least 16 characters; unset, the server generates one and prints it to its log. Ignored once the server is set up |
| `PYN_TOKEN` | Token the CLI signs in with, instead of the sign-in saved by `pyn login` (`PYN_USER` with `PYN_DEV_AUTH`) |
| `PYN_REGISTRATION` | `invite` (default), `open` or `closed`: how people create accounts; the choice made in first-run setup replaces it |
| `PYN_ORG_CREATION` | `anyone` (default) or `admins`: who may create organizations; `admins` limits it to server administrators |
| `PYN_MAX_LOCKS_ALLOWED_PER_USER` | How many locks one user may hold in a repository, default 5; a repository setting or its `pyn.toml` can override it |
| `PYN_DEFAULT_MAX_REPOSITORIES`, `PYN_DEFAULT_MAX_ORG_MEMBERS`, `PYN_DEFAULT_MAX_STORAGE_BYTES` | Optional defaults for per-owner [limits](docs/concepts/limits.md); unset (the norm for a self-hosted server) means unlimited |
| `PYN_OIDC_ISSUER`, `PYN_OIDC_CLIENT_ID`, `PYN_OIDC_CLIENT_SECRET` | Optional sign-in with an OpenID Connect provider; unset (the norm) means password only. See [external sign-in](docs/concepts/external-sign-in.md), which also covers `PYN_OIDC_NAME`, `PYN_OIDC_USERNAME_CLAIM`, `PYN_OIDC_CREATE_ACCOUNTS`, `PYN_OIDC_REDIRECT_URL` and `PYN_PASSWORD_SIGN_IN` |
| `PYN_EMAIL_VERIFICATION` | On unless `false`: open sign-up needs an email address and the account stays inactive until its link is followed |
| `PYN_REQUIRE_APPROVAL` | `true` holds new open sign-ups until a server administrator approves them |
| `PYN_PUBLIC_URL` | Where the web app lives, for links in emails; default `http://<PYN_ADDR>`; the address given in first-run setup replaces it |
| `PYN_EMAIL` | How email is sent; only `log` (writes messages to the server log) exists |
| `PYN_RATE_SIGN_IN_ACCOUNT`, `PYN_RATE_SIGN_IN_CLIENT`, `PYN_RATE_REGISTER_CLIENT`, `PYN_RATE_MAIL_PER_EMAIL` | Rate limits, defaults 5, 30, 10 and 3; see [access](docs/concepts/access.md#protecting-open-registration) |
| `PYN_TRUST_FORWARDED_FOR` | `true` takes the client address from the last `X-Forwarded-For` entry; only behind a proxy that sets it |
| `PYN_PASSWORD_HASHES` | Most password hashes at once, default the number of CPUs |
| `PYN_SESSION_DAYS` | How long a `pyn login` lasts, default 30 |
| `PYN_CONFIG_DIR` | Where the CLI keeps `credentials.toml`, default `~/.config/pyn` |
| `PYN_CLI_CONFIG` | The CLI's user settings file, default `~/.config/pyn/config.toml` (not the server's `PYN_CONFIG`); `pyn config init` creates it, and the first command in a terminal offers to |
| `PYN_REPO` | The repository (`owner/name`) the CLI acts on, unless a workspace or `--repo` says otherwise |

`make test-live` runs the tests that need a database (`PYN_DATABASE_URL`); each uses its own temporary schema.
Documentation lives in [docs/](docs/README.md).

> **Not for production.** Never run with `PYN_DEV_AUTH=true` on a network you do not control.
