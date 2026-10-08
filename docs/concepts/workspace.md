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

The workspace commands that read and write this folder are not built yet.
