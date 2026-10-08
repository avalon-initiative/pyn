# `pyn.toml`

The repository's collaboration policy, stored at `.pyn/pyn.toml` (see [the `.pyn/` folder](workspace.md)): which paths are `exclusive` (one editor at a time, enforced by the server) and
which are `shared` (concurrent edits, merged later).

```toml
[meta]
default = "exclusive"            # what an unlisted path is; "exclusive" if this is omitted

[exclusive]
paths = [
  "Content/",                    # a folder: trailing slash, everything beneath it
  "Config/ProductionConfig.cpp", # an exact file
  "**/*.uasset",                 # a glob
]

[shared]
paths = [
  "Source/",
  "docs/",
  "README.md",
]
```

One entry per line keeps diffs readable. Every section is optional; a missing or empty file means everything is
exclusive.

## Default is exclusive

A path that no entry covers is **exclusive**. That is the safe side: the worst outcome is someone has to check a file
out, never two people silently editing something unmergeable. Mostly-code repositories will usually flip it with
`default = "shared"` and list only the exclusive paths; asset-heavy repositories keep the default and list the shared
ones. This differs from the original proposal, where unlisted paths were shared.

## Entries

| Form | Example | Matches |
| --- | --- | --- |
| Exact file | `Config/ProductionConfig.cpp` | that file only |
| Folder (trailing `/`) | `Content/` | everything beneath it, whole path segments only (`ContentExtra/` is not inside `Content/`) |
| Glob | `**/*.uasset` | `*` stays within one segment, `**` crosses segments, `?` one character, `[abc]` a class |

Paths are repository-relative with `/` separators: no leading `/` or `./`, no `.` or `..` segments.

## Precedence (provisional)

When several entries cover a path, the most specific wins:

1. an exact file entry;
2. a glob entry. If a path matches globs in both lists, `exclusive` wins;
3. a folder entry, the deepest folder winning (`Content/docs/` beats `Content/`);
4. `meta.default`.

The same exact file or folder in both lists is an error, not a silent choice. Unknown tables, unknown keys and
invalid values are errors too: with a safe default, a typo like `[exlusive]` must not be quietly ignored.

## Changing policy

The server is the only authority on a path's mode, and it enforces the `pyn.toml` on the default branch for every
branch. Edits to `pyn.toml` on other branches have no effect until they are merged, and a branch cannot override a path's
mode.

A policy change is a commit to `pyn.toml` on the default branch, made by someone with the policy permission. It can
arrive through a pull request or as a direct commit; which one is the team's choice. The server validates the commit
(the file parses and has no contradictory entry), and the new policy applies from it. Make the edits that need the new
mode after it lands.

- **Exclusive to shared** is always allowed. Live locks on the path are released.
- **Shared to exclusive** is never refused. Branches that hold diverged versions of the path become stranded: their
  versions stay in history but can no longer merge into the path's single line. The server reports the affected branches
  and revisions to whoever made the change and to the branch owners, and shows it again at merge time. To keep a stranded
  version, take the lock and check it in as the new head.

Each revision records the mode it was made under, so a path's history shows where it changed. Until branches exist
there is nothing to strand.

## `pyn.toml` is a tracked file

`.pyn/pyn.toml` is an ordinary path in the repository, exclusive or shared as the team prefers. Unless it lists itself, it
is **exclusive**, even when `meta.default` is `shared`. To share it, list it:

```toml
[shared]
paths = [".pyn/pyn.toml"]
```

A change to the file's own mode is made under its current mode and takes effect once it lands.

The very first `pyn.toml` is the owner's initial policy. It is accepted as submitted, without a lock, and the mode it
declares for itself applies from that revision, so a repository can be created with a shared `pyn.toml`. Until it exists,
everything is exclusive. The initial mode is recorded on that first revision like any other.
