# The `.pyn/` folder

Everything pyn keeps in a workspace lives in one folder at the workspace root, `.pyn/`, the way a git repository keeps
its files in `.git/`. A developer's project directory stays free of pyn files, and there is one obvious place for
anything related to pyn.

```text
.pyn/
  config.toml    workspace settings: server, repository, identity
  ignore         paths pyn leaves alone
  state/         which revision each local file is at
  cache/         downloaded content
```

The ignore file is `.pyn/ignore`, not a `.pynignore` in the project root.

## Overriding settings

The defaults are all under `.pyn/`, and every setting and location can be overridden. The first of these that has a value
wins:

1. command-line flags
2. environment variables (`PYN_*`)
3. the workspace configuration, `.pyn/config.toml`
4. the user configuration, `~/.config/pyn/config.toml`
5. built-in defaults

`pyn config get` and `pyn config set` read and write settings, with `--local` for the workspace and `--global` for the
user, like `git config`. `PYN_DIR` relocates the whole folder, and individual files such as the ignore file can be
pointed elsewhere.

## Shared and local files (provisional)

`.pyn/` is pyn's own metadata and is not tracked as content, except for two files that belong to the repository and are
shared with everyone: the ignore file and the policy file. Everything else is local to the workspace: `config.toml`,
`state/` and `cache/`. The policy file may move from the repository root to `.pyn/pyn.toml`; until that is decided it stays
at the root as described in [pyn.toml](pyn-toml.md).

The workspace commands that read and write this folder are not built yet.
