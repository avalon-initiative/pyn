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

## Command line

`pyn ls [path]` prints the listing and `pyn summary` the summary.
