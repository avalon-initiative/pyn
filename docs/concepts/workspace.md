# The `.pyn/` folder

Everything pyn keeps in a workspace lives in one folder at the workspace root, `.pyn/`, the way a git repository keeps
its files in `.git/`. A developer's project directory stays free of pyn files, and there is one obvious place for
anything related to pyn.

```text
.pyn/
  pyn.toml          the repository's collaboration policy (shared)
  ignore            paths pyn leaves alone (shared)
  local_only/       never tracked
    config.toml     workspace settings: server, repository, identity
    state/          which revision each local file is at
    cache/          downloaded content
```

The ignore file is `.pyn/ignore`, not a `.pynignore` in the project root.

## Shared and local files

Everything in `.pyn/` is shared and tracked as part of the repository, except the `local_only/` folder, which is never
tracked. A file's place in the tree says which it is.

## Overriding settings

The defaults are all under `.pyn/`, and every setting and location can be overridden. The first of these that has a value
wins:

1. command-line flags
2. environment variables (`PYN_*`)
3. the workspace configuration, `.pyn/local_only/config.toml`
4. the user configuration, `~/.config/pyn/config.toml`
5. built-in defaults

`pyn config get` and `pyn config set` read and write settings, with `--local` for the workspace and `--global` for the
user, like `git config`. `PYN_DIR` relocates the whole folder, and individual files such as the ignore file can be
pointed elsewhere.

## The policy file

The collaboration policy is `.pyn/pyn.toml`, shared like any other file in the folder and described in
[pyn.toml](pyn-toml.md). It is not called `config.toml`, because the server enforces it and a local file cannot override
it, whereas the local settings layer in the order above.

## Working in a workspace

`pyn clone <server>/owner/name [dir]` (or `pyn clone owner/name` on the configured server) creates the folder, which defaults to
a folder named after the repository, downloads every file at its head revision, records the repository in
`.pyn/local_only/config.toml`, and records in `.pyn/local_only/state/` which revision each file is at and a hash of its content. After that, commands work from wherever
you are inside the workspace and take paths relative to your current directory.

- **Read-only exclusive files.** Exclusive files are read-only until you hold the lock. `pyn checkout` makes the file
  writable; `pyn checkin` and `pyn release` make it read-only again. Shared files are always writable.
- **No base revisions to remember.** `checkout` and `checkin` use the revision the workspace has, so `--base` is only for
  working outside a workspace. If the server has moved on, the command tells you to run `pyn update`.
- **`pyn status`** lists what differs: files you modified or deleted, files that are behind the server, files that are new
  on the server, untracked files, and who holds which lock. Clean, unlocked files are left out.
- **`pyn update`** downloads new and newer files. A file with local changes is never overwritten; it is reported as skipped.
- **`pyn checkin <path>`** uploads the file at `<path>` in the workspace; a file name is only needed outside one.

## The ignore file

`.pyn/ignore` lists paths that `pyn status` should not report as untracked, one pattern per line, with `#` for comments.
A pattern ending in `/` ignores that folder wherever it appears (`node_modules/`), a pattern without a `/` matches at any
depth (`*.log`), and a pattern containing a `/` is relative to the workspace root (`Saved/cache.bin`). The file is shared
like the rest of `.pyn/`, so a team keeps one list.

## Settings

`pyn config get|set|list` reads and writes settings. For now there are three: `server`, `user` (the development
identity) and `repo` (`owner/name`, set by `pyn clone`). `--repo` or `PYN_REPO` beats the workspace's `repo`, so one
workspace can still reach another repository for a single command. Outside a workspace, repository commands need one of them. `set` writes the workspace's file by default and your user file with `--global`; `get` shows the effective value,
or one layer with `--local` or `--global`. A `PYN_SERVER` or `--server` always wins, and `PYN_DIR` points at a `.pyn`
folder kept somewhere else.
