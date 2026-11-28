# Troubleshooting

Errors from the server arrive as `{code, message}`; the CLI prints the message and the code is what scripts match. Codes are API
surface: they are added, never renamed. A request body that is not valid JSON, lacks a field or has one of the wrong type is
`invalid_request` with the status that names the problem (`400` for syntax, `415` without `Content-Type: application/json`, `422`
for a missing or mistyped field), and the message names the field. The usual ones, with what to do.

## Locks and commits

| Code | Meaning | What to do |
| --- | --- | --- |
| `lock_held` | Someone else holds the lock on that path. The message names them and when their lease ends. | Ask them to unlock, wait for the lease, or have a user with `force_unlock` remove it (`pyn unlock --force --reason "..."`). |
| `lock_required` | The path is exclusive and you committed without a live lock, or your lease lapsed. | `pyn lock <path>`, then commit again. |
| `not_lock_holder` | You tried to unlock or commit a path locked by someone else, or not locked by you. | `pyn locks` to see the holder. |
| `lock_limit_reached` | You already hold the per-user maximum of locks in this repository (5 by default). | Unlock one first, or have the limit raised in the policy ([lock limit](concepts/pyn-toml.md#lock-limit)). |
| `not_exclusive` | You tried to lock a path that is shared. | Nothing to do: edit it freely. To make it exclusive, change `.pyn/pyn.toml`. |
| `stale_base` | The change is based on an older version than the branch has. | `pyn pull`, resolve any conflict, push again. |
| `not_fast_forward` | The push does not build on the branch tip. | `pyn pull` (or `pyn pull --rebase`), then push. |
| `merge_conflict` | A merge found changes it cannot combine. Binary files conflict as whole files. | Resolve, `pyn add` the result, `pyn commit` (or `pyn pull --continue`). `pyn merge --abort` starts over. |
| `merge_stale` | The branch moved while the merge was computed. | Merge again. |

A file you cannot save in your editor is a read-only exclusive file: take the lock first. If the lease ran out during a long edit, the
commit is refused with `lock_required`; lock again and commit before doing anything else. Locks last 8 hours by default, and locking again as
the holder renews the lease.

## Transfers and limits

| Code | Meaning | What to do |
| --- | --- | --- |
| `object_too_large` | One file is over the 16 GiB object limit. | Split or exclude it. |
| `storage_limit_reached`, `repo_storage_limit_reached` | The owner's or repository's storage cap is reached. | An administrator raises the cap or content is removed ([limits](concepts/limits.md)). |
| `uploads_busy` | Too many uploads are in progress for the scope. | Retry shortly. |
| `owner_frozen` | An administrator froze the owner: reads work, writes are refused. | Contact the administrator. |
| `too_many_attempts` | Rate limit on sign-in or a similar action. | Wait the stated seconds. |

An interrupted download or upload resumes: run the same command again.

## Sign-in and setup

| Symptom | Cause |
| --- | --- |
| `invalid_setup_token` | The one-time setup token is wrong. Find it with `docker compose logs server \| grep -A4 "one-time setup token"`, or set `PYN_SETUP_TOKEN` yourself ([getting started](getting-started.md#5-set-up-the-server-first-administrator)). |
| `registration_closed` | The server is invite-only or closed. Ask an administrator for an invitation. |
| `403 origin_not_allowed` at web sign-in | The server does not know the web app's address. Set `PYN_ALLOWED_ORIGINS` ([getting started](getting-started.md#other-shapes)). |
| Warning about `http://` | Passwords and tokens travel in clear text. Put TLS in front of the server ([HTTPS](deployment.md#https)). |
| A git-only command prints that pyn does not support it yet | See the *planned* rows of [git compatibility](concepts/git-compatibility.md). |
| A command says it needs a pyn server | You are in a git home; see [which home](concepts/git-compatibility.md#which-home). |

## Servers

`curl http://localhost:8080/healthz` checks a server. `docker compose logs server` shows why a start failed; `required variable ... is
missing` means an empty setting in `.env`. Upgrades, backups and restores are in [deployment](deployment.md).
