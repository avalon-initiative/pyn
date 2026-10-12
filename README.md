# pyn

Version control that merges what can be merged and locks what shouldn't be.

Git is great for text. It fails on files that cannot be merged: a `.uasset`, a `.glb`, a scene, a texture, a binary
blob. Two people edit the same one, and the second has to throw their work away. Teams that hit this bolt on Git LFS
locking or move to Perforce, and then live in two tools.

pyn is one tool for both. It works like git day to day, and when a path should only ever have one editor, you mark it
`exclusive` and the server enforces a leased lock on it. A repository with no exclusive paths behaves like any other
version-controlled repository, and git is enough for it. pyn is for when you sometimes need the lock, and want to have it
without switching tools later.

## How it works

Each path in a repository is either:

- **shared**: edited concurrently and merged, as in git.
- **exclusive**: one editor at a time. `pyn lock <path>` takes a lease (8 hours by default), the file is otherwise
  read-only, and `pyn commit` uploads the change and releases the lock. Locks expire, so an abandoned one cannot block a
  file forever.

Which is which is a versioned part of the repository, set per path in `.pyn/pyn.toml`:

```toml
[meta]
default = "shared"              # what a path becomes when no rule below matches it

[exclusive]                     # one editor at a time: take a lock with `pyn lock`, release it by committing
paths = ["Content/", "**/*.uasset", "**/*.glb"]    # folders, globs or single files

[shared]                        # edited concurrently, merged like git
paths = ["Source/", "docs/"]
```

The mode is a policy choice, not a file-type rule: a text file can be exclusive and a binary can be shared. See the
[`pyn.toml` specification](docs/concepts/pyn-toml.md).

The CLI uses git's command names, flags and output shapes wherever pyn has an equivalent (`clone`, `status`, `commit`,
`log`, `diff`, `show`, `restore`), so `alias git=pyn` is the goal for everyday flows. Every difference is listed in the
[git compatibility table](docs/concepts/git-compatibility.md). History is tamper-evident (`pyn verify`), and pyn has no
dependency on git.

## Status

An early, local proof of concept. What works today: a server hosting many repositories, the `pyn` CLI, the web UI
([`pyn-web`](https://github.com/avalon-initiative/pyn-web)), shared and exclusive paths, leased locks, atomic
multi-path commits, per-path and per-commit history, branches and merge (three-way text merge, whole-file conflicts for
binaries, `pyn merge --abort`), diff and show, restore, chunked content storage, accounts,
organizations and teams, roles, API tokens and SSH keys, optional OpenID Connect sign-in, and an audit log.

Not built yet: remotes (`push`, `pull`, `fetch`), tags, stash, per-branch policy, and git import and export. A shared file's
commit must be based on the revision its branch has, so a stale edit is rejected rather than silently overwriting someone
else's work. The git-style rows in the compatibility table marked *planned* are the roadmap.

## Run it

One command brings up a server with a seeded demo (in memory, so nothing to install):

```bash
make dev                 # builds, starts the server in the background, creates accounts, repositories, files and locks
```

It prints the URL and the sign-ins: `root` / `demo-password` (owns `root/demo`), `alice` / `alice-password` and
`bob` / `bob-password` (`bob` owns the public `bob/tools`). `root/demo` has nested folders, the policy in
[`scripts/demo.pyn.toml`](scripts/demo.pyn.toml) (shared `Source/`, `docs/`; exclusive `Content/`, `*.uasset`) and four
locks held by alice and bob. `make dev` is safe to repeat, `make stop` stops the server and `make demo` re-seeds a server
that is already running. Log: `_running/logs/pyn.log`.

Drive it from the CLI; `make dev` keeps each account's sign-in under `_running/demo-config/<user>`:

```bash
export PYN_SERVER=http://127.0.0.1:7878 PYN_REPO=root/demo PYN_CONFIG_DIR=_running/demo-config/bob
./target/debug/pyn ls Content          # modes, last change and locks, one folder at a time
./target/debug/pyn summary
./target/debug/pyn lock Content/World/Main.umap # lock_held (409): alice has it
```

And in the browser, with the web app from the `pyn-web` repo (it proxies `/v1` to `http://127.0.0.1:7878`, or to
`PYN_API` if set):

```bash
cd ../pyn-web && npm ci && npm run dev    # http://localhost:5173, sign in as alice or bob
```

For your own server instead of the demo: `cp .env.example .env`, then `make run` (foreground, reads `.env`). A new
server is uninitialised and prints a one-time setup token in its log; `pyn setup <name> --setup-token <token>` (or
`POST /v1/setup`) creates the first administrator, then `pyn login <name>`. See
[first-run setup](docs/concepts/access.md#first-run-setup) and [configuration](docs/configuration.md) for every
environment variable.

## Layout

pyn is a few repositories, each public under [avalon-initiative](https://github.com/avalon-initiative):

| Repository | What it holds |
| --- | --- |
| [`pyn`](https://github.com/avalon-initiative/pyn) (this one) | The server, the `pyn` CLI, the shared libraries and the [documentation](docs/README.md) |
| [`pyn-web`](https://github.com/avalon-initiative/pyn-web) | The web UI: routing, the API client and the views that talk to the server |
| [`pyn-common-ui`](https://github.com/avalon-initiative/pyn-common-ui) | The shared Vue component library the web UI is built on (presentation only) |

New to pyn? Start with the [concepts](docs/concepts/README.md).

Licensed under Apache-2.0.
