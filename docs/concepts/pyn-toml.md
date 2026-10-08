# `pyn.toml`

The repository's collaboration policy: which paths are `exclusive` (one editor at a time, enforced by the server) and
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

To move a file between modes, first land a change to `pyn.toml` on the default branch (a maintainer or admin, through
the normal pull request path), and only then make the edits that need the new mode. The server checks the change when it
is merged:

- **Exclusive to shared** is always allowed. Live locks on the path are released.
- **Shared to exclusive** is refused while branches hold different versions of the path. They must be merged or a
  canonical version chosen first; the chosen version becomes the next revision on the path's single line.

Each revision records the mode it was made under, so a path's history shows where it changed. Until pull requests
exist, the policy is the server's own `pyn.toml`, changed by whoever administers the server.
