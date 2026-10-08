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

## Open questions

- Whether `pyn.toml` is a versioned file in the repository (and which branch's copy counts) or a server-side
  repository setting.
- Policy changes (`shared` to `exclusive` and back) must be recorded as history events. Moving shared to exclusive
  needs a canonical revision first, which depends on merge support.
- Whether per-section settings belong in the same file (for example a lease length under `[exclusive]`).
