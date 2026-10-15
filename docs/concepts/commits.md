# Commits, snapshots and diff

A **commit** is one atomic write to a repository: a single `pyn commit`, or a whole [changeset](README.md#exclusive-files-and-locks).
Commits are git commits (decision #245): each has git's commit id, the SHA-256 hash of the commit object, and is named by
that hash or an abbreviation of it. A rejected write makes no commit. A commit is made on a [branch](branches.md) and has the
branch's head as its parent; a [merge commit](merging.md) has two, the target's head first.

## The commit record

A commit carries what a git commit carries:

- the **author** and **committer**: name, email, time and timezone offset. The server records the signed-in account's
  name and email (an account without an email gets `<name>@pyn.invalid`), the time of the write, and offset `+0000`;
  the author and committer are the same person. The offset is part of the record so that imported commits (#95) keep
  theirs;
- the **parents**: none for the first commit of a repository, else the head of the branch it was made on;
- the **message**;
- room for a **signature** (a `gpgsig` header), which signed commits (#60) will fill;
- the **tree**: a git tree of every file in the repository as of this commit, with nested trees and per-entry git file
  modes (`100644`, `100755`, `120000`) and blob ids. The tree is a plain git tree, built with `pyn-object`.

The commit id is the git commit id of exactly those fields, so the encoding is git's and the id would be the same in a git
repository of the same object format (SHA-256 for a pyn server). The id covers the tree and the parent, so changing any
file, mode or earlier commit changes every later id, and a [revision's v2 hash](integrity.md) covers its commit id.
The exact commit bytes are stored with each commit and the id is their hash; the fields above are what those bytes say.
A server-created commit's bytes are the encoding of its fields, so every id is unchanged, and the bytes are what lets a
commit built elsewhere keep its id (provisional: no path ingests such a commit yet). Commits that predate the stored
bytes were re-encoded from their fields by a migration.
Existing history was migrated once into a single mainline, now the default branch; see [integrity](integrity.md#existing-data).

## The tree of a commit

The tree of a commit is stored with it, built from its parent's tree by replacing the entries the commit wrote, so only
the folders on the changed paths are rewritten. A repository's state as of a commit is read from the revisions:

- a path's **revision at a commit** is its highest-numbered revision whose commit is that commit or an ancestor of it
  (decision #245 item 9). The server caches it per commit and reads it by walking the commit's first-parent chain; see
  [branches](branches.md#the-revision-of-a-path-at-a-commit);
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
| `GET /v1/repos/{owner}/{name}/commits?branch=&before=&limit=` | the commits a branch contains (the default branch's if omitted), newest first (default 50, max 200), each with its revisions; `next_before` pages back |
| `GET /v1/repos/{owner}/{name}/commits/{commit}` | one commit; `404 commit_not_found` for an id the repository does not have |
| `GET /v1/repos/{owner}/{name}/snapshot?commit=&branch=&after=&limit=` | the snapshot of a commit, or of a branch's head (the default branch's if neither is given), ordered by path, 200 per page (max 1000); `commit` is null for an empty repository |

A commit is given as its full id or as an abbreviation of at least 4 lowercase hex digits; an abbreviation that matches
more than one commit is `400 ambiguous_commit`. `HEAD~N` and `^` are resolved by clients, which walk `parents`.

File content at any revision is fetched with `GET .../content?path=&revision=`, so a snapshot entry's `revision` and `content` are
enough to fetch every file. Every revision in the API carries the `commit` it belongs to, its `file_mode` and, for revisions
made since the commit record landed, the structured `author_identity` its hash covers. A checkin or changeset entry takes an
optional `file_mode` (`100644` by default, `100755` or `120000`; a symbolic link's content is its target path). Reading is allowed to whoever may read the repository.

## Pushed commits (provisional)

Decision #300 lets a client build commits itself and publish them. The core accepts them through `RepoService::push_commits` and
one atomic store step, `MetadataStore::push_commits_on`; no HTTP route, wire type or CLI command exists for it yet, and everything
here is provisional until those slices land.

- A push is a chain of commits, oldest first, plus the trees they need. Commits keep their ids: each is checked to hash to the id
  it is sent under, to be a SHA-256 git commit, and to be a child of the one before it.
- It must fast-forward: the first commit's parent is the branch's head (a first commit with no parent is for an empty branch), or
  `not_fast_forward`. A commit already in the repository is `push_invalid`. The branch must exist.
- Each commit's tree is compared with its parent's: every added or changed file becomes one revision, with the commit's author as its
  person, the pusher as its account, the commit's time and message, and the path's mode under the branch's policy (an unlisted path
  is exclusive by default). The blob of each must already be in the object store (`object_missing`), and a tree the commit needs must
  be in the push or already stored (`push_invalid`).
- An exclusive path needs the pusher's live lock at the moment of the push (`lock_required`, or `lock_held` when someone else has
  it); a lapsed lease is refused. The lock is used up with the path's last revision in the push, so a push may carry several
  commits that edit it. Each revision is based on the branch's pointer for the path as the commits before it leave it
  ([branches](branches.md)), not on the newest revision of another branch.
- The whole push is accepted or refused as one: a refusal at any commit leaves no commit, revision, tree, lock change or branch
  move behind. Concurrent pushes to a branch are serialized, so one wins and the others get `not_fast_forward`.
- Refused with `push_unsupported`, for now: a commit that changes `.pyn/pyn.toml`; a deletion, including a file turned into a
  folder or back; a commit with two parents; a commit that changes no file; a submodule entry; a commit whose author and committer
  times differ (a commit records one time); a name, path or message that is not UTF-8; and a message without its final newline.
- The pusher needs the `checkin` permission. Whether the commit's author must match the pusher is undecided: it is not checked.

## Command line

The CLI follows git where it has an equivalent; the differences are listed below.

- `pyn log --commits [--limit n]` lists commits (`COMMIT`, `AUTHOR`, `WHEN`, `FILES`, `MESSAGE`); `COMMIT` is the abbreviated hash. `pyn log` keeps listing revisions.
- A commit is written as its hash or an abbreviation of at least 4 hex digits (`pyn log --commits` shows 10), `HEAD` (the
  head of the workspace's [branch](branches.md)), or either followed by `~N` (N commits back along the first parent) or `^` (the
  parent; `^^` is two back). Only the first parent is followed, so `^2` names no commit. `cN` numbers no longer exist. An argument that is also the name of something in the current
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
- `status --revision`, `update --revision` and `log --commits` have no git equivalent; they are pyn's way to reach a past commit of the
  workspace's branch. Another line of work is a [branch](branches.md): `pyn switch`.

