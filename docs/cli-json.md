# CLI JSON output

`--json` turns the commands an editor or engine integration calls into one JSON document on standard output. It is a global
flag (`pyn locks --json`, `pyn --json locks`) and works against a pyn server; on a git repository it is refused. Sign-in,
server and repository selection are unchanged (`--server`, `--token`, `--repo`, `PYN_SERVER`, `PYN_TOKEN`, `PYN_REPO`, the
workspace and user config), so an integration inherits the user's sign-in.

Supported: `whoami`, `status`, `locks` (and `--mine`, and `--watch`, which prints one document per line: see below), `lock`, `unlock` (and `--force`), `files`, `ls`, `summary` and `admin access-session start|end|list`. Any other
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
| `admin access-session start <user> --reason ...` | `{"version", "session": AccessSessionInfo, "secret": string}` (the secret is shown once) |
| `admin access-session end <id>` | `{"version", "session": AccessSessionInfo}` |
| `admin access-session list` | `{"version", "sessions": [AccessSessionInfo]}` |

`Lock` is `{path, owner, acquired_at, expires_at}`; `MyLock` adds `name` (the repository) next to `owner`. `FileEntry` is
`{path, mode, revision, file_mode, lock}` with `mode` `shared` or `exclusive`, `revision` null when the path has none yet
and `lock` null when unlocked. `TreeEntry` is `{name, path, kind, mode, last_change, lock}`. `AccessSessionInfo` is `{id, admin, user, reason, notes, started_at, expires_at, ended_at, state, reached: [{method, route, target, refused, count, first_at, last_at}]}`. `RepoSummary` is
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
             "locked_by_you": 1, "locked_by_others": 0, "needs_lock": 0, "blocked_by_lock": 0},
  "unpublished_commits": 0
}
```

Like the table, `files` lists only paths that differ, are staged or are locked. `revision` is null for a file the
workspace has not synced. `staged` is `-`, `added`, `modified` or `deleted`. `states` holds stable tokens: `modified`,
`behind`, `ahead`, `deleted_locally`, `untracked`, `new`, `locked_unpublished`, `local_commit`, `unmerged`, `merged`,
`needs_lock` and `blocked_by_lock`. The last two are for an exclusive file that is modified, deleted, staged or in a local
commit while the signed-in user holds no lock on it: `needs_lock` when nobody holds it, `blocked_by_lock` when someone else
does (`lock.owner` names them). `pyn push` refuses both; `counts.needs_lock` and `counts.blocked_by_lock` total them.
`lock.mine` is true when the signed-in user holds it. `pyn edit` has no `--json` document.

## `locks --watch`

`pyn locks --watch` keeps running and follows the repository's lock changes live. Without `--json` it prints one line per
change (time, event, holder, detail, path last). With `--json` it prints one document per line, flushed as it happens, and
never an enclosing array:

```json
{"version": 1, "cursor": "812", "type": "lock.acquired", "at": "2030-01-01T12:00:00Z", "path": "Content/m.umap",
 "holder": "alice", "expires_at": "2030-01-01T20:00:00Z", "actor": "alice", "reason": null}
```

`type` is `lock.acquired`, `lock.released`, `lock.expired` or `lock.forced`, the event types of
[lock events](concepts/events.md#lock-events); a type a later version adds is skipped by this version. `expires_at` is when
the lease ends (`lock.acquired`) or ended (`lock.expired`), and null otherwise. `actor` is who did it: the holder for
`lock.acquired` and `lock.released`, the one who removed it for `lock.forced`, null for `lock.expired`. `reason` is `checkin`
for a release by a check-in or push, the unlock reason for `lock.forced`, else null. Only changes after the watch started
are printed.

`cursor` identifies the change; `--after <cursor>` starts a new watch just after it, so an integration that restarts
resumes without a gap (changes older than the server keeps events, 30 days, are gone). The watch reconnects by itself,
resuming from the last cursor, when the connection drops or the server answers a 5xx, 408 or 429, waiting 1 second and then
longer up to 30 seconds, with a note on standard error. A refusal (no read access, no such repository, a bad token) ends it
with the error the other commands give, exit code included.

A server that has no change stream (an older one) is polled instead, every `--interval` seconds (default 2). Polling
compares successive lock lists, so its documents have `cursor` null and a `lock.released` cannot tell a release from a
check-in or a forced unlock (`actor` and `reason` are null); a lock that disappears after its expiry is `lock.expired`.
A lock taken and released between two polls is never seen.

The watch needs a pyn server (a git repository refuses it) and cannot combine with `--mine`. It ends when its output closes
or it is interrupted.

## Errors

A failure prints one document to standard output instead of text on standard error, and the exit code is the one the
command has without `--json`:

```json
{"version": 1, "error": {"code": "lock_held", "status": 409, "message": "...", "lock": {"path": "...", "owner": "bob", "acquired_at": "...", "expires_at": "..."}}}
```

`code` and `status` are the server's (`lock_held`, `forbidden`, `not_repo_member`, and the rest of the API's stable codes).
`lock` appears on a `lock_held` refusal from `lock` and names the holder and expiry. Failures that did not come from the
server (no server reachable, no repository named) have `code` `error` and no `status`. Treat `message` as text for people.
