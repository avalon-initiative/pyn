# Browsing a repository

Two read-only routes feed a repository landing page: a folder listing and a summary. Both need `read` and describe the single
mainline; there is no revision or branch parameter yet. Sections marked *provisional* are not settled and may change.

## Folder listing

`GET /v1/repos/{owner}/{name}/tree?path=<folder>` lists the children of one folder; the root when `path` is omitted or
empty (a trailing `/` is accepted). Folders come first, then files, each ordered by name. An unknown folder is
`404 path_not_found`; a file path is `400 invalid_request`.

Folders are derived: pyn stores files, so a folder exists while some path under it has a revision or a live lock. The
listing is not paginated.

| Field | Meaning |
| --- | --- |
| `name`, `path` | the last segment, and the repo-relative path to pass back as `path` for a folder |
| `kind` | `file` or `folder` |
| `mode` | `shared` or `exclusive` from the [rules](pyn-toml.md); a folder is `mixed` unless every file under it has the same mode |
| `last_change` | the newest revision at or under the entry (its `path` names the file); absent for a file that is locked but has no revision yet |
| `lock` | the live lock and its holder, on files only |

"Last change" is what phase 1 stores: revisions are numbered per path and carry author, message and time, so there is no
repository-wide commit id. Changesets and commits come with later phases; until then clients show the revision number with
the path. A folder's mode is computed from the files that exist, not from the rules alone, so an exclusive glob that matches
nothing yet does not make a folder `mixed`. *Provisional*

## Summary

`GET /v1/repos/{owner}/{name}/summary?activity=<n>` returns what a landing page's side panel needs:

- `default_branch` and `branch_count`: always `main` and `1`. Branches, tags and their counts come with branches
  (pyn#29). *Provisional*
- `files`, `exclusive_files`, `shared_files`: paths with at least one revision, split by current mode.
- `updated_at`: the time of the newest revision.
- `locks`: every live lock with its holder, ordered by path.
- `activity`: the newest `n` (default 10, at most 100) file and lock events from the [audit log](access.md): `checkout`,
  `release`, `checkin`, `restore`, `force_unlock`. Administrative events (members, roles, tokens, repository changes) and
  the free-text detail stay behind `view_audit`.

Watchers, forks and tags do not exist on the server and are not reported.

## History

`GET /v1/repos/{owner}/{name}/history` needs `read` and always answers `{ revisions, next_cursor }`. It has two modes.

- **One path** (`path=<file>`): every revision of that path, **oldest first**, in one response with no `next_cursor`.
  This is the order clients already rely on, so it is kept. `path` cannot be combined with the parameters below
  (`400 invalid_request`).
- **Repository-wide** (no `path`): revisions of every path, **newest first**, as the same `Revision` objects (`id`, `path`,
  `author`, `message`, `created_at`, ...), paged.

| Parameter | Meaning |
| --- | --- |
| `filter` | a glob over paths; only matching revisions are returned. A bad pattern is `400 invalid_request` |
| `limit` | page size, default 50, at most 200 |
| `before` | the previous page's `next_cursor`; omit for the first page |

**Filter matching** uses the `pyn.toml` [glob rules](pyn-toml.md#entries): `*` stays within one segment, `**` crosses
segments, `?` and `[abc]` work as there. A pattern **without a `/`** is matched against the path at any depth, as in
the [ignore file](workspace.md#the-ignore-file), so `*.ts` finds `a.ts` and `src/deep/a.ts`. A pattern **with a `/`** is
anchored at the repository root: `Content/*.uasset` matches only directly inside `Content/`, `Source/**` everything under
`Source/`. A trailing `/` (`Source/`) means everything under that folder. Matching is on the revision's path.

**Paging.** Revision ids are numbered per path, so they do not order the repository. The order is the key
`(created_at, path, id)`, descending, with paths compared bytewise; revisions made in the same instant therefore have a
fixed order. `next_cursor` is an opaque string encoding the last revision returned; pass it back unchanged as `before` to
get the revisions strictly older than it. It is absent on the last page. A page can be shorter than `limit` only on the
last page, even when a filter drops most revisions. Revisions committed after the first page are newer than every cursor
and never shift later pages. A cursor that does not parse is `400 invalid_request`.

## Command line

`pyn log <path>` (alias `pyn history`) lists one path's revisions, oldest first (`REV`, `AUTHOR`, `WHEN`, `MESSAGE`). `pyn log` without a
path lists the repository's, newest first, with a `PATH` column after `MESSAGE`; `--filter <glob>` narrows it and
`--limit <n>` (default 50) sets how many to show, following the cursor as needed.

`pyn ls [path]` prints the listing and `pyn summary` the summary, as tables in the format described in [workspace](workspace.md#listing-output).
