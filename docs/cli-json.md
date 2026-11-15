# CLI JSON output

`--json` turns the commands an editor or engine integration calls into one JSON document on standard output. It is a global
flag (`pyn locks --json`, `pyn --json locks`) and works against a pyn server; on a git repository it is refused. Sign-in,
server and repository selection are unchanged (`--server`, `--token`, `--repo`, `PYN_SERVER`, `PYN_TOKEN`, `PYN_REPO`, the
workspace and user config), so an integration inherits the user's sign-in.

Supported: `whoami`, `status`, `locks` (and `--mine`), `lock`, `unlock` (and `--force`), `files`, `ls` and `summary`. Any other
command with `--json` is a usage error (exit 129). `status --json` cannot be combined with `--short`, `--porcelain` or `-b`.

## Versioning

Every document is one line holding an object with `"version": 1`. Within a version, fields are only ever added, so ignore
fields you do not know. A change that removes or renames a field, or changes its meaning, raises `version`. Timestamps are
RFC 3339 in UTC, and paths are repository-relative. The wire types come from the API (`docs/generated/openapi.json`), so a
field here means what it means there. Shapes are provisional until the first release that ships them.

## Success

| Command | Document |
| --- | --- |
| `whoami` in a repository | `{"version", "user", "user_id", "repo", "permissions": [string]}` |
| `whoami` elsewhere | `{"version", "user", "user_id", "display_name": string or null, "admin": bool}` |
| `locks` | `{"version", "locks": [Lock]}` |
| `locks --mine` | `{"version", "locks": [MyLock]}` |
| `lock <path>` | `{"version", "lock": Lock}` |
| `unlock <path>` | `{"version", "unlocked": path}` |
| `unlock --force <path> --reason ...` | `{"version", "removed": Lock}` (the lock that was removed) |
| `files` | `{"version", "files": [FileEntry]}` |
| `ls [path]` | `{"version", "path", "entries": [TreeEntry]}` |
| `summary` | `{"version", "summary": RepoSummary}` |

`Lock` is `{path, owner, acquired_at, expires_at}`; `MyLock` adds `name` (the repository) next to `owner`. `FileEntry` is
`{path, mode, revision, file_mode, lock}` with `mode` `shared` or `exclusive`, `revision` null when the path has none yet
and `lock` null when unlocked. `TreeEntry` is `{name, path, kind, mode, last_change, lock}`. `RepoSummary` is
`{default_branch, branch_count, files, exclusive_files, shared_files, updated_at, locks, activity}`.

### `status`

```json
{
  "version": 1,
  "files": [
    {
      "path": "Content/m.umap",
      "mode": "exclusive",
      "revision": 3,
      "staged": "-",
      "states": ["modified", "behind"],
      "lock": {"owner": "alice", "mine": true, "acquired_at": "...", "expires_at": "..."}
    }
  ],
  "counts": {"staged": 0, "modified": 1, "behind": 1, "ahead": 0, "unmerged": 0, "new": 0, "untracked": 0,
             "locked_by_you": 1, "locked_by_others": 0},
  "unpublished_commits": 0
}
```

Like the table, `files` lists only paths that differ, are staged or are locked. `revision` is null for a file the
workspace has not synced. `staged` is `-`, `added`, `modified` or `deleted`. `states` holds stable tokens: `modified`,
`behind`, `ahead`, `deleted_locally`, `untracked`, `new`, `locked_unpublished`, `local_commit`, `unmerged`, `merged`.
`lock.mine` is true when the signed-in user holds it.

## Errors

A failure prints one document to standard output instead of text on standard error, and the exit code is the one the
command has without `--json`:

```json
{"version": 1, "error": {"code": "lock_held", "status": 409, "message": "...", "lock": {"path": "...", "owner": "bob", "acquired_at": "...", "expires_at": "..."}}}
```

`code` and `status` are the server's (`lock_held`, `forbidden`, `not_repo_member`, and the rest of the API's stable codes).
`lock` appears on a `lock_held` refusal from `lock` and names the holder and expiry. Failures that did not come from the
server (no server reachable, no repository named) have `code` `error` and no `status`. Treat `message` as text for people.
