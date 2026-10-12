# Commits, snapshots and diff

A **commit** is one atomic write to a repository: a single `pyn commit`, or a whole [changeset](README.md#exclusive-files-and-locks).
Commits are numbered 1, 2, 3, ... per repository in the order they landed and shown as `c1`, `c2`, ... A commit carries its
author, time and message and the revisions it made. A rejected write makes no commit and uses no number. (Revisions are hashed, see [integrity](integrity.md); commit hashes as
in decision #65 are provisional, and until they land a commit is identified by its number.)

## The tree of a commit

The repository as of commit N is the newest revision at or before N of every path: its **snapshot**. The snapshot is derived
from the revisions, so nothing is stored twice.

- a **shared** path is identified by the hash of its content at that commit;
- an **exclusive** path is identified by the revision the commit points at (decision #28), together with its blob id.

Each entry also carries the mode recorded with that revision, so a snapshot shows a path as it was when it changed. There is one
mainline until branches exist, so a commit's number is its position on it. Rolling an exclusive file back is not involved:
asking for an old commit reads old pointers and never changes any file's history.

## API

| Request | Returns |
| --- | --- |
| `GET /v1/repos/{owner}/{name}/commits?before=&limit=` | commits newest first (default 50, max 200), each with its revisions; `next_before` pages back |
| `GET /v1/repos/{owner}/{name}/commits/{commit}` | one commit; `404 commit_not_found` for a number the repository does not have |
| `GET /v1/repos/{owner}/{name}/snapshot?commit=&after=&limit=` | the snapshot of a commit (the newest if omitted), ordered by path, 200 per page (max 1000); `commit` is 0 for an empty repository |

File content at any revision is fetched with `GET .../content?path=&revision=`, so a snapshot entry's `revision` and `content` are
enough to fetch every file. Every revision in the API carries the `commit` it belongs to. Reading is allowed to whoever may read the repository.

## Command line

The CLI follows git where it has an equivalent; the differences are listed below.

- `pyn log --commits [--limit n]` lists commits (`COMMIT`, `AUTHOR`, `WHEN`, `FILES`, `MESSAGE`). `pyn log` keeps listing revisions.
- A commit is written `cN`, `HEAD` (the newest) or `HEAD~N`. Flags that take a commit also accept a bare number.
- `pyn clone --revision <commit> <source> [dir]` creates a workspace with every file as of that commit.
- `pyn status --revision <commit>` compares the workspace with that commit instead of the newest: `behind (cN has rM)` for a
  file whose revision there is newer, `ahead (cN has rM)` or `ahead (not in cN)` for one that is newer than, or missing from, it.
- `pyn update --revision <commit>` brings the workspace to that commit, older or newer: files are fetched at the commit's
  revision and files it does not have are removed. A file with local changes is never touched and is reported as skipped.
- `pyn show [<commit>]` prints `commit`, `Author:`, `Date:` and the message, then what the commit changed (the newest if none is named).
- `pyn diff` prints a [unified diff](#diff-output).

### `pyn diff`

| Form | Compares |
| --- | --- |
| `pyn diff [-- <path>...]` | the workspace with the revisions it was last updated to (tracked files only) |
| `pyn diff <commit> [-- <path>...]` | the workspace with that commit |
| `pyn diff <commit> <commit>` or `<commit>..<commit>` | two commits; works outside a workspace |

Arguments before `--` that are commits (`HEAD`, `HEAD~N`, `cN`) are taken as such, anything after is a path, relative to the
current directory inside a workspace. Options: `-U<n>` / `--unified=<n>` lines of context (default 3), `--stat`, `--name-only`,
`--name-status` (`A`, `M`, `D` and a tab), `--exit-code` (exit 1 when there are differences, 0 when none) and `--quiet` /
`-q` (no output, exit 1 when there are differences). Without `--exit-code` a diff exits 0, as in git.

#### Diff output

Output is the unified format git prints: `diff --git a/<path> b/<path>`, `new file mode` or `deleted file mode`, an `index
<old>..<new>` line with seven characters of each blob id, `--- a/<path>` and `+++ b/<path>` (`/dev/null` for a created or
deleted file), then `@@ -a,b +c,d @@` hunks, with `\ No newline at end of file` where it applies. A file with a NUL byte in its
first 8000 bytes is binary and prints `Binary files a/<path> and b/<path> differ`; only that first block of it is fetched.
The diff is Myers' algorithm, built in with no dependency on git (decision #30), computed in the CLI. For inputs with more than
2000 changed lines the changed region is shown as one replacement instead of a minimal diff, to bound memory.

#### Differences from git

- There is no staging area, so `--cached` and `--staged` are refused with a message; `pyn diff` compares with the last update.
- File modes are not tracked: created and deleted files always print mode `100644`.
- `status --revision`, `update --revision` and `log --commits` have no git equivalent; they are pyn's way to reach a past state
  until branches exist.
- Commit names are `cN` numbers and `HEAD`/`HEAD~N`, not hashes.
