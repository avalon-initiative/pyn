# `pyn.toml`

The repository's collaboration policy, stored at `.pyn/pyn.toml` (see [the `.pyn/` folder](workspace.md)): which paths are `exclusive` (one editor at a time, enforced by the server) and
which are `shared` (concurrent edits, merged later).

```toml
[meta]
default = "exclusive"            # what an unlisted path is; "exclusive" if this is omitted
max_locks_per_user = 5           # optional: locks one user may hold at once (1 to 10000)

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

## Lock limit

`meta.max_locks_per_user` caps how many live locks one user may hold in this repository. A checkout past it is refused
with `lock_limit_reached` (HTTP 409), and the message names the limit; renewing a lock already held is never refused.
Releasing, a force-unlock and expiry each free a slot.

Where the limit comes from, first match wins:

1. `meta.max_locks_per_user` in the repository's `.pyn/pyn.toml`;
2. the repository's `max_locks_per_user` setting (see [repositories](repositories.md#what-each-repository-has));
3. the server's `PYN_MAX_LOCKS_ALLOWED_PER_USER`, 5 if unset.

The file wins over the setting: while it sets a limit, the setting cannot be changed (the API refuses with
`invalid_request`) and the repository reports `max_locks_set_by_policy`, so a settings page shows the value read-only.
Remove the key from the file and the setting applies again. Changing the limit in the file takes effect with the revision
and releases nothing: a user already over a lowered limit keeps their locks and cannot take new ones until they are under it.

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

The server is the only authority on a path's mode. Each repository enforces the `.pyn/pyn.toml` at its own head, and a
branch cannot override a path's mode. (Branches do not exist yet; when they do, only the default branch's copy counts, and
edits on other branches take effect when merged.)

A policy change is a check-in of `.pyn/pyn.toml`, made by someone with the `edit_policy` permission. It can arrive through
a pull request or as a direct commit; which one is the team's choice. The server validates the file before recording it: it
must be UTF-8 TOML that parses with no unknown table or key, invalid value or contradictory entry. A file that fails is
rejected with `invalid_rules`, and nothing changes, including the lock the committer holds. The new policy applies the
moment the revision is recorded, with no restart, and a server that starts reads the head revision of each repository's
file. Restoring an older revision of the file goes through the same checks and applies that revision's policy.

If the head of the file cannot be parsed when the repository is opened, the server refuses to open the repository rather
than guess at a policy.

- **Exclusive to shared** is always allowed. Live locks on the paths that become shared are released, each recorded in the
  audit log as a `force_unlock` by whoever made the change, and the change itself as `policy_changed`. There is no
  notification to the holders yet.
- **Shared to exclusive** is never refused. Branches that hold diverged versions of the path become stranded: their
  versions stay in history but can no longer merge into the path's single line. The server reports the affected branches
  and revisions to whoever made the change and to the branch owners, and shows it again at merge time. To keep a stranded
  version, take the lock and check it in as the new head. *Provisional:* until branches exist there is nothing to strand,
  and the report arrives with them.

Each revision records the mode its path had when it was made (`mode` on the revision in the API), so a path's history shows
where it changed. Revisions made before this was recorded have no mode.

A repository with no `.pyn/pyn.toml` uses the server's fallback file (`PYN_CONFIG`) if one is configured, and otherwise
everything is exclusive. The fallback stops applying to a repository once that repository has its own file; the two are
never merged.

## `pyn.toml` is a tracked file

`.pyn/pyn.toml` is an ordinary path in the repository, exclusive or shared as the team prefers. Unless it lists itself, it
is **exclusive**, even when `meta.default` is `shared`. To share it, list it:

```toml
[shared]
paths = [".pyn/pyn.toml"]
```

A change to the file's own mode is made under its current mode and takes effect once it lands. Any entry that covers the
file counts as listing it, so a folder entry such as `.pyn/` works too.

The very first `pyn.toml` is the owner's initial policy. It is accepted as submitted, without a lock (it still needs
`edit_policy`), and the mode it declares for itself applies from that revision, so a repository can be created with a
shared `pyn.toml`. Until it exists, the fallback or the all-exclusive default applies. The first revision records the mode
it declares for itself, like any other revision records the mode in force. Only the first revision skips the lock: once any
revision exists, a submission that claims to be the first is rejected as a stale base.

`pyn repo create` offers a default file when it creates a repository: `[meta] default = "exclusive"` and nothing else listed.
The owner accepts it, replaces it with `--policy <file>`, or skips it with `--no-policy`.
