# Commits, snapshots and diff

A **commit** is one atomic write to a repository: a single `pyn commit`, or a whole [changeset](README.md#exclusive-files-and-locks).
Commits are git commits (decision #245): each has git's commit id, the SHA-256 hash of the commit object, and is named by
that hash or an abbreviation of it. A rejected write makes no commit. There is one mainline until branches exist (#235),
so a commit has at most one parent.

## The commit record

A commit carries what a git commit carries:

- the **author** and **committer**: name, email, time and timezone offset. The server records the signed-in account's
  name and email (an account without an email gets `<name>@pyn.invalid`), the time of the write, and offset `+0000`;
  the author and committer are the same person. The offset is part of the record so that imported commits (#95) keep
  theirs;
- the **parents**: none for the first commit, else the previous one;
- the **message**;
- room for a **signature** (a `gpgsig` header), which signed commits (#60) will fill;
- the **tree**: a git tree of every file in the repository as of this commit, with nested trees and per-entry git file
  modes (`100644`, `100755`, `120000`) and blob ids. The tree is a plain git tree, built with `pyn-object`.

The commit id is the git commit id of exactly those fields, so the encoding is git's and the id would be the same in a git
repository of the same object format (SHA-256 for a pyn server). The id covers the tree and the parent, so changing any
file, mode or earlier commit changes every later id, and a [revision's v2 hash](integrity.md) covers its commit id.
Existing history was migrated once into this mainline; see [integrity](integrity.md#existing-data).

## The tree of a commit

The tree of a commit is stored with it, built from its parent's tree by replacing the entries the commit wrote, so only
the folders on the changed paths are rewritten. A repository's state as of a commit is read from the revisions:

- a path's **revision at a commit** is its highest-numbered revision whose commit is that commit or an ancestor of it
  (decision #245 item 9). On the single mainline that is the newest revision at or before the commit, a range lookup that
  needs no stored copy; a per-commit cache of these pointers arrives with branches, when the lookup needs a walk;
- a path's **mode** (`shared` or `exclusive`) is what `.pyn/pyn.toml` says at that commit: the policy file in the
  commit's own snapshot, or the server's fallback rules while the repository has none;
- an exclusive path is not special in the tree: its entry is the blob of the revision it has at that commit, like any
  file. The revision number is not stored in the tree, so trees stay plain git trees and rolling back or merging
  needs no pointer handling.

Each revision also records the git file mode of its content, so a snapshot shows a path as it was when it changed. Asking
for an old commit reads old revisions and never changes any file's history.

## API

| Request | Returns |
| --- | --- |
| `GET /v1/repos/{owner}/{name}/commits?before=&limit=` | commits newest first (default 50, max 200), each with its revisions; `next_before` pages back |
| `GET /v1/repos/{owner}/{name}/commits/{commit}` | one commit; `404 commit_not_found` for an id the repository does not have |
| `GET /v1/repos/{owner}/{name}/snapshot?commit=&after=&limit=` | the snapshot of a commit (the newest if omitted), ordered by path, 200 per page (max 1000); `commit` is null for an empty repository |

A commit is given as its full id or as an abbreviation of at least 4 lowercase hex digits; an abbreviation that matches
more than one commit is `400 ambiguous_commit`. `HEAD~N` and `^` are resolved by clients, which walk `parents`.

File content at any revision is fetched with `GET .../content?path=&revision=`, so a snapshot entry's `revision` and `content` are
enough to fetch every file. Every revision in the API carries the `commit` it belongs to, its `file_mode` and, for revisions
made since the commit record landed, the structured `author_identity` its hash covers. A checkin or changeset entry takes an
optional `file_mode` (`100644` by default, `100755` or `120000`; a symbolic link's content is its target path). Reading is allowed to whoever may read the repository.

## Command line

The CLI follows git where it has an equivalent; the differences are listed below.

- `pyn log --commits [--limit n]` lists commits (`COMMIT`, `AUTHOR`, `WHEN`, `FILES`, `MESSAGE`); `COMMIT` is the abbreviated hash. `pyn log` keeps listing revisions.
- A commit is written as its hash or an abbreviation of at least 4 hex digits (`pyn log --commits` shows 10), `HEAD` (the
  newest), or either followed by `~N` (N commits back) or `^` (the parent; `^^` is two back). There is one mainline, so
  `^2` names no commit. `cN` numbers no longer exist. An argument that is also the name of something in the current
  directory is read as a path by `pyn diff`.
- `pyn clone --revision <commit> <source> [dir]` creates a workspace with every file as of that commit.
- `pyn status --revision <commit>` compares the workspace with that commit instead of the newest: `behind (<commit> has rM)` for a
  file whose revision there is newer, `ahead (<commit> has rM)` or `ahead (not in <commit>)` for one that is newer than, or missing from, it.
- `pyn update --revision <commit>` brings the workspace to that commit, older or newer: files are fetched at the commit's
  revision and files it does not have are removed. A file with local changes is never touched and is reported as skipped.
- `pyn show [<commit>]` prints `commit <full hash>`, `Author: <name> <<email>>`, `Date:` and the message, then what the commit changed against its parent (the newest if none is named).
- `pyn diff` prints a [unified diff](#diff-output).

### `pyn diff`

| Form | Compares |
| --- | --- |
| `pyn diff [-- <path>...]` | the workspace with the revisions it was last updated to (tracked files only) |
| `pyn diff <commit> [-- <path>...]` | the workspace with that commit |
| `pyn diff <commit> <commit>` or `<commit>..<commit>` | two commits; works outside a workspace |

Arguments before `--` that are commits (`HEAD`, `HEAD~N`, `HEAD^`, a hash) are taken as such, anything after is a path, relative to the
current directory inside a workspace. Options: `-U<n>` / `--unified=<n>` lines of context (default 3), `--stat`, `--name-only`,
`--name-status` (`A`, `M`, `D` and a tab), `--exit-code` (exit 1 when there are differences, 0 when none) and `--quiet` /
`-q` (no output, exit 1 when there are differences). Without `--exit-code` a diff exits 0, as in git.

#### Diff output

Output is the unified format git prints: `diff --git a/<path> b/<path>`, `new file mode <mode>`, `deleted file mode <mode>` or `old mode`/`new mode`, an `index
<old>..<new>` line with seven characters of each blob id, `--- a/<path>` and `+++ b/<path>` (`/dev/null` for a created or
deleted file), then `@@ -a,b +c,d @@` hunks, with `\ No newline at end of file` where it applies. A file with a NUL byte in its
first 8000 bytes is binary and prints `Binary files a/<path> and b/<path> differ`; only that first block of it is fetched.
The diff is Myers' algorithm, built in with no dependency on git (decision #30), computed in the CLI. For inputs with more than
2000 changed lines the changed region is shown as one replacement instead of a minimal diff, to bound memory.

#### Differences from git

- `pyn diff` compares the work tree with the staged content, as git does (the last update where nothing is staged), and `--cached` or
  `--staged` compares the staged content with the last update; see [staging](workspace.md#staging).
- File modes are tracked: created and deleted files print their git mode, and a changed mode prints `old mode` and `new mode`.
- `status --revision`, `update --revision` and `log --commits` have no git equivalent; they are pyn's way to reach a past state
  until branches exist.

