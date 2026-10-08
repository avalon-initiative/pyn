# pyn

Version control that merges what can be merged and locks what shouldn't be. Git-style collaboration for files that
can be merged, Perforce-style exclusive checkout (server-enforced, leased locks) for files that shouldn't be edited
concurrently. Which is which is set per path in [`pyn.toml`](docs/concepts/pyn-toml.md).

Local proof of concept, Phase 1 only (single mainline, shared + exclusive paths, leased locks, checkout/checkin,
history). This repo holds the server, the `pyn` command-line client, and the shared libraries. The web UI is
`pyn-web` (a separate repo).

## Run it

```bash
cp .env.example .env     # set PYN_DATABASE_URL to use PostgreSQL; without it everything is in memory
make run                 # server on 127.0.0.1:7878, reads .env
make demo                # in another terminal: seeds sample files and locks
./target/debug/pyn --user alice files
./target/debug/pyn --user bob checkout Content/World/Main.umap    # lock_held (409)
```

| Variable | Meaning |
| --- | --- |
| `PYN_DATABASE_URL` | PostgreSQL connection URL; unset means in-memory metadata |
| `PYN_CREATE_DATABASE` | `true` creates the database if it does not exist |
| `PYN_DATA_DIR` | Directory for uploaded content; unset means in-memory |
| `PYN_CONFIG` | Path to the `pyn.toml` policy file; unset means everything is exclusive |
| `PYN_ADDR` | Listen address, default `127.0.0.1:7878` |

`make test-live` runs the tests that need a database (`PYN_DATABASE_URL`); each uses its own temporary schema.
Documentation lives in [docs/](docs/README.md).

> **Not for production.** Auth is a trusted `X-Pyn-User` header. Do not expose this server to a network you do not
> control.
