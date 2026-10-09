# pyn

Version control that merges what can be merged and locks what shouldn't be. Git-style collaboration for files that
can be merged, Perforce-style exclusive checkout (server-enforced, leased locks) for files that shouldn't be edited
concurrently. Which is which is set per path in [`pyn.toml`](docs/concepts/pyn-toml.md).

Local proof of concept, Phase 1 only (single mainline per repository, shared + exclusive paths, leased locks,
checkout/checkin, history), on a server that hosts many [repositories](docs/concepts/repositories.md). This repo holds the server, the `pyn` command-line client, and the shared libraries. The web UI is
`pyn-web` (a separate repo).

## Run it

One command brings up a server with a seeded demo (in memory, so nothing to install):

```bash
make dev                 # builds, starts the server in the background, creates accounts, repositories, files and locks
```

It prints the URL and the sign-ins: `admin` / `demo-password` (owns `admin/demo`), `alice` / `alice-password` and
`bob` / `bob-password` (`bob` owns the public `bob/tools`). `admin/demo` has nested folders, the policy in
[`scripts/demo.pyn.toml`](scripts/demo.pyn.toml) (shared `Source/`, `docs/`; exclusive `Content/`, `*.uasset`) and four
locks held by alice and bob. `make dev` is safe to repeat, `make stop` stops the server and `make demo` re-seeds a server that is
already running. Log: `_running/logs/pyn.log`. Set `PYN_ADDR` for another port and `PYN_DATABASE_URL` (via `.env`) to keep the data in PostgreSQL.

Drive it from the CLI; `make dev` keeps each account's sign-in under `_running/demo-config/<user>`:

```bash
export PYN_SERVER=http://127.0.0.1:7878 PYN_REPO=admin/demo PYN_CONFIG_DIR=_running/demo-config/bob
./target/debug/pyn ls Content          # modes, last change and locks, one folder at a time
./target/debug/pyn summary
./target/debug/pyn checkout Content/World/Main.umap # lock_held (409): alice has it
```

And in the browser, with the web app from the `pyn-web` repo (it proxies `/v1` to `http://127.0.0.1:7878`, or to `PYN_API` if set):

```bash
cd ../pyn-web && npm ci && npm run dev    # http://localhost:5173, sign in as alice or bob
```

For your own server instead of the demo: `cp .env.example .env`, then `make run` (foreground, reads `.env`) with
`PYN_BOOTSTRAP_ADMIN=<name> PYN_BOOTSTRAP_PASSWORD=<password>` and `pyn login <name>`; see [access](docs/concepts/access.md).

| Variable | Meaning |
| --- | --- |
| `PYN_DATABASE_URL` | PostgreSQL connection URL; unset means in-memory metadata |
| `PYN_CREATE_DATABASE` | `true` creates the database if it does not exist |
| `PYN_DATA_DIR` | Directory for uploaded content; unset means in-memory |
| `PYN_CONFIG` | Fallback policy for repositories that have no `.pyn/pyn.toml` yet; unset means everything is exclusive (`make dev` uses `scripts/demo.pyn.toml`) |
| `PYN_ADDR` | Listen address, default `127.0.0.1:7878` |
| `PYN_DEV_AUTH` | `true` accepts the `X-Pyn-User` header with every permission; for server tests only, not used by `make dev` |
| `PYN_BOOTSTRAP_ADMIN` | Creates this account at startup and prints a token for it; it then creates repositories like anyone else |
| `PYN_TOKEN` | Token the CLI signs in with, instead of the sign-in saved by `pyn login` (`PYN_USER` with `PYN_DEV_AUTH`) |
| `PYN_REGISTRATION` | `invite` (default), `open` or `closed`: how people create accounts |
| `PYN_MAX_LOCKS_ALLOWED_PER_USER` | How many locks one user may hold in a repository, default 5; a repository setting or its `pyn.toml` can override it |
| `PYN_SESSION_DAYS` | How long a `pyn login` lasts, default 30 |
| `PYN_BOOTSTRAP_PASSWORD` | Gives the `PYN_BOOTSTRAP_ADMIN` user this password |
| `PYN_CONFIG_DIR` | Where the CLI keeps `credentials.toml`, default `~/.config/pyn` |
| `PYN_CLI_CONFIG` | The CLI's user settings file, default `~/.config/pyn/config.toml` (not the server's `PYN_CONFIG`); `pyn config init` creates it, and the first command in a terminal offers to |
| `PYN_REPO` | The repository (`owner/name`) the CLI acts on, unless a workspace or `--repo` says otherwise |

`make test-live` runs the tests that need a database (`PYN_DATABASE_URL`); each uses its own temporary schema.
Documentation lives in [docs/](docs/README.md).

> **Not for production.** Never run with `PYN_DEV_AUTH=true` on a network you do not control.
