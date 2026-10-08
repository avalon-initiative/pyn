# pyn

Version control that merges what can be merged and locks what shouldn't be. Git-style collaboration for files that
can be merged, Perforce-style exclusive checkout (server-enforced, leased locks) for files that shouldn't be edited
concurrently. Which is which is set per path in [`pyn.toml`](docs/concepts/pyn-toml.md).

Local proof of concept, Phase 1 only (single mainline, shared + exclusive paths, leased locks, checkout/checkin,
history). This repo holds the server, the `pyn` command-line client, and the shared libraries. The web UI is
`pyn-web` (a separate repo).

```bash
make run PYN_CONFIG=pyn.example.toml     # server on 127.0.0.1:7878
cargo build -p pyn-cli                   # builds target/debug/pyn
export PYN_SERVER=http://127.0.0.1:7878
pyn --user alice checkout Content/Dungeon.umap
pyn --user bob   checkout Content/Dungeon.umap    # lock_held (409)
pyn --user alice checkin  Content/Dungeon.umap ./Dungeon.umap -m "first pass"
```

Documentation lives in [docs/](docs/README.md).

> **Not for production.** Auth is a trusted `X-Pyn-User` header and storage is in memory. Do not expose this server
> to a network you do not control.
