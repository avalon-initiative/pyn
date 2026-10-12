# Git compatibility

Decision #218: where pyn has an equivalent of a git command it uses git's name, flags, argument order and output shape,
so `alias git=pyn` works for everyday flows. pyn owns what git lacks (`lock`, `unlock`, `policy`, modes, leases,
organizations and teams). This page is the compatibility table: every difference from git is deliberate and listed here.
The table describes the CLI as it is; rows that say *planned* name the issue that will change them. Conformance tests
against git are tracked in #224. Everything below is about a repository on a pyn server unless it says *git home*; a
repository that lives in git is described in [On a git home](#on-a-git-home).

## Conventions

Divergence rules:

1. **Every divergence is deliberate and listed.** A command, flag or argument form that differs from git has a row with a
   reason. If it is not in a table below, pyn behaves as git does or the gap is a bug.
2. **Never silently different.** A flag or argument form pyn cannot honour fails with a message; it is never ignored and
   never does something else. Where a `fatal:` message explains the refusal (for example a commit named to `diff --cached` on a pyn server), the table says so.
3. **A git command with no pyn equivalent says so.** It prints `pyn: '<cmd>' is a git command pyn does not support yet; see
   docs/concepts/git-compatibility.md.` and exits `1`, not a usage error. A name that is neither pyn's nor a known git
   command prints `pyn: '<cmd>' is not a pyn command. See 'pyn --help'.` and exits `1`, as git does for an unknown command.
4. **`checkout` is reserved for git's meaning** (switch branches, restore paths). Taking a lock is `pyn lock`. Until
   branches land (#235) `pyn checkout` fails with a message saying so and pointing at `pyn lock`.
5. **`checkin` is gone.** `pyn commit` is the command and there is no alias (decision #229, renamed in #230). `pyn checkin`
   is an unknown command. The server's `checkin` permission, route and audit action keep their names (API contract).
6. **pyn-owned commands are listed as pyn only** and are never given a git name: `lock`, `unlock`, `locks`, `policy`,
   `audit`, and the account, organization, team and server administration commands.
7. **Commits are git commits** (decision #245): identified by their git commit id (SHA-256 on a pyn server), shown
   abbreviated, and named by hash, `HEAD`, `~N` and `^`; there are no `cN` numbers. Git's wire protocol is still not
   implemented here, and there is no dependency on git (decision #30). A git import and export bridge may come later.
8. **Listings are aligned tables** with the path or variable column last, not git's free-form blocks. This is a
   repository-wide difference: it keeps columns such as mode, lock and revision readable.

9. **A repository has one home** (decision #245): a pyn server or git. In a git repository pyn behaves as git, through its own
   implementation, and commands that act on the repository and only a pyn server provides refuse and say so; see [On a git home](#on-a-git-home).

Exit codes follow git: `0` success, `128` for a failure (message prefixed `fatal: `), `129` for a usage error such as
an unknown flag on a supported command (raised by the argument parser, which names the flag and the command's usage),
`1` for an unknown or git-only command. `diff --exit-code` and `-q` exit `1` when there are differences, as in git.

Statuses used below:

| Status | Meaning |
| --- | --- |
| same | Name, arguments and output shape as git. |
| different | Supported, with a deliberate difference; the reason is given. |
| unsupported | Fails with a message; no plan (see [Not planned](#not-planned)) or no pyn equivalent. |
| planned | Fails with a message today; the issue that will implement it is named. |
| pyn only | No git equivalent; pyn's own. |

Global options: pyn takes `--server`, `--token`, `--user` and `--repo` (also as `PYN_SERVER`, `PYN_TOKEN`, `PYN_USER`,
`PYN_REPO`) before or after the command, and has no git global flags: `-C <dir>`, `-c <key=value>`, `--git-dir` and
`--work-tree` fail with a usage error (exit `129`). `--version` and `--help` behave as in git.

## Daily set

The commands named in decision #218.

| Git | pyn | Status | Notes |
| --- | --- | --- | --- |
| `git init` | `pyn init` | different | In an existing git repository it adds `.pyn/` to `.git/info/exclude` and creates `.pyn/local_only/` (see [On a git home](#on-a-git-home)); it never creates a repository. Anywhere else it fails with the git-command message: repositories are created on the server with `pyn repo create` and then cloned, and a local `init` has no issue yet (epic #219). |
| `git clone <repo> [<dir>]` | `pyn clone <source> [<dir>]` | same | Source is `<server>/owner/name`, or `owner/name` on the configured server. Output is one summary line, not progress. |
| `git clone --depth`, `-b`, `--branch`, `--bare`, `--mirror`, `-o`, `-n`, `--recurse-submodules` | none | unsupported | Usage error (`129`). `-b` and `--branch` become possible with branches (#235). Every clone has full history on the server; there is no local object store to shallow. |
| `pyn clone --revision <commit>` | | pyn only | Clone as of a commit (a hash, `HEAD~N`). Git does this with `clone` then `checkout`. |
| `git status` | `pyn status` | different | Table with mode, revision, staged, state, lock and path plus a summary line, because mode and lock state have no place in git's layout. `STAGED` is `added` or `modified` for a staged path, and `STATE` then describes what changed after it was staged. On a git home the table is `STAGED`, `UNSTAGED`, `PATH` with git's states. |
| `git status -s`, `--short`, `--porcelain`, `-b`, `<pathspec>...` | none | unsupported | Usage error (`129`). A stable machine format is not scheduled. `pyn status --revision` fails on a git home. |
| `pyn status --revision <commit>` | | pyn only | Compare the workspace with a past commit (`behind` / `ahead`). |
| `git add <path>...` | `pyn add <path>...` | same | Stages a path, a folder or `.`; a path is required unless `-u` or `-A` is given. On an exclusive path of a pyn server it fails unless you hold the lock (see below). |
| `git add -u`, `-A`, `-n`, `-v`, `-f` and their long forms | same | same | `-u` and `--update` stage tracked files only; `-A` and `--all` take new files too and, with no path, cover the whole repository. `-n` and `--dry-run` print `add '<path>'` and `remove '<path>'` and stage nothing. Ignored files are refused (exit `1`, as git) without `-f`. |
| `git add -p`, `-i`, `-e`, `--intent-to-add`, `--chmod`, `--renormalize`, `--pathspec-from-file`, glob and magic pathspecs | none | unsupported | Usage error (`129`), or `pathspec ... did not match any files` for a glob the shell did not expand. A path is a file or a folder, taken relative to where you stand. |
| `git commit -m <msg>` | `pyn commit -m <msg>` | different | Commits what is staged as one unit of work. On a pyn server it is one changeset uploaded to the server, so commit and publish are one step (no `push` needed), it releases the locks it held, and the output is `<path> is now at revision N` for each file. On a git home it is a git commit on the current branch and prints `[<branch> <id>] <subject>`. `-m` is required (no editor) and repeats for more paragraphs. Nothing staged exits `1` with a message. |
| `git commit -a`, `-am <msg>` | `pyn commit -a`, `-am <msg>` | same | Stages every changed tracked file, then commits; new files are not included. |
| `git commit <path>...` | `pyn commit -m <msg> <path>...` | different | On a pyn server: one atomic commit of the named paths as they are on disk, whatever is staged (git's `--only`). Not available on a git home, which says to `add` the paths first. |
| `git commit -F`, `--amend`, `--allow-empty`, `-s`, `-S`, `--no-verify`, `--author`, `--date`, `-v` | none | unsupported | Usage error (`129`). Commits are immutable on the server, so `--amend` will not exist; `-F` is not accepted because `--file` already means the content file. |
| `pyn commit --file <local>` | | pyn only | Content file for a single path. In a workspace the file at the path is the default. |
| `pyn commit --base <rev>` | | pyn only | The revision the edit started from; the server rejects the commit if the head moved on an exclusive path. |
| `git log [-n <n>] [<path>]` | `pyn log [-n <n>] [<path>]` (alias `history`) | different | Without a path, newest first, 50 by default. With a path, oldest first and all revisions (git prints newest first). Output is a table of per-path revisions, not commit blocks. `-n` is also `--max-count`; `--limit` is an alias. On a git home it is git's output (see below). |
| `git log --oneline`, `--graph`, `--stat`, `-p`, `--author`, `--since`, `--grep`, `--format`, `--follow`, `<rev>..<rev>`, `<rev> -- <path>` | none | unsupported | Usage error (`129`). A revision range is read as a path. Branch-aware log arrives with #235. |
| `pyn log --commits` | | pyn only | List commits (`COMMIT`, `AUTHOR`, `WHEN`, `FILES`, `MESSAGE`) instead of revisions. |
| `pyn log --filter <glob>` | | pyn only | Only paths matching the glob. |
| `git diff [<c1> [<c2>]] [-- <path>...]` | `pyn diff [<c1> [<c2>]] [-- <path>...]` | different | Unified output as git, computed in the CLI. Commits are hashes (or abbreviations of 4 or more digits), `HEAD`, `~N`, `^`, or `a..b`; two commits work outside a workspace. With no commit it compares the work tree with the index: the staged content, or the last update for what is not staged. File modes print as in git, including `old mode` and `new mode`. |
| `git diff -U<n>`, `--unified=<n>`, `--stat`, `--name-only`, `--name-status`, `--exit-code`, `-q` | same | same | `--name-status` prints `A`, `M`, `D` only. |
| `git diff --cached`, `--staged` | `pyn diff --cached`, `--staged` | different | The staged content against the last commit (the last update on a pyn server). On a git home it takes one optional commit to compare with, as git; on a pyn server it takes none. |
| `git diff --shortstat`, `-w`, `--word-diff`, `--color`, `--no-index`, `--check`, `-M`, `-C` | none | unsupported | Usage error (`129`). |
| `git show [<commit>]` | `pyn show [<commit>]` | different | Commit header (`commit`, `Author:`, `Date:`), message, then the commit's diff. The newest commit if none is named. The author line carries the email. On a git home the output is git's, except for merge commits (see below). |
| `git show -U<n>`, `--stat`, `--name-only`, `--name-status` | same | same | |
| `git show <rev>:<path>` | `pyn get <path> [--rev N] [-o <file>]` | different | `show` takes commits only (`HEAD:foo` fails with `fatal: ... is not a commit`, `128`). File content at a revision is `pyn get`, to stdout or a file. `--rev` is a per-path revision, not a commit. |
| `git show --oneline`, `-s`, `--format`, `<tag>`, `<object>` | none | unsupported | Usage error (`129`) or `fatal: ... is not a commit`. |
| `git restore --source=<rev> <path>` | `pyn restore --source <rev> <path>` (or `pyn restore <path> <rev>`) | different | pyn's restore appends a new revision on the server: it needs the lock, the `restore` permission and a typed confirmation (`--yes` skips it); `-m` sets the message. The revision is required (git defaults to the index or HEAD) and `-s` is the short form of `--source`. Git's restore only edits the working tree. |
| `git restore --staged <path>...` | `pyn restore --staged <path>...` (`-S`) | same | Puts the staged entries under the paths back to the last commit and leaves the files alone. On a git home `--source <commit>` takes the entries from that commit, as git; on a pyn server there is no commit to take them from and it is a usage error. |
| `git restore <path>` (the work tree), `--worktree`, `-p`, `--ours`, `--theirs` | none | unsupported | Usage error (`129`), or on a git home the "not supported yet" message naming #254. |
| `git branch`, `git switch`, `git checkout` | none | planned | Git-command message (`checkout` has its own, see rule 4). Branch core #235 under epic #205 (decision #233), with git's flags. |
| `git merge` | none | planned | Git-command message. Merge flow #236 and the merge library #234, under epic #205. |
| `git fetch`, `git pull`, `git push` | none | planned | Git-command message. Remotes and sync #237 under epic #205 (worked with #223). Today a commit goes straight to the server and `pyn update` brings the workspace forward. |
| `git remote` | none | planned | Git-command message. #237 (also listed in #223). There is one remote today, set by `--server` and `--repo` or `pyn config set server` and `repo`. |
| `git tag` | none | planned | Git-command message. #223. |
| `git stash` | none | planned | Git-command message. #223. |

`pyn update [<path>...]` is the pre-branch stand-in for `fetch` plus `pull`: it fast-forwards files without local
changes and never overwrites an edit, reporting skipped files. `--revision <commit>` moves the workspace to an older or
newer commit. It is pyn only, and is expected to become a thin layer over `pull` once #237 lands.

## History and inspection

Commands people reach for that are not in the daily set.

| Git | pyn | Status | Notes |
| --- | --- | --- | --- |
| `git ls-files` | `pyn files` | different | Table of every file with mode, head revision and lock holder. No flags. On a git home it lists the index, every file `shared`, with no revision or lock. |
| `git ls-tree [<rev>] [<path>]` | `pyn ls [<path>]` | different | One folder with each entry's mode, last change and lock. Head only; no revision argument. On a git home it lists `HEAD`'s tree, with no last change. |
| `git cat-file -p`, `git show <rev>:<path>` | `pyn get <path>` | different | See `show` above. |
| `git config --list`, `--get <key>`, `<key> <value>` | `pyn config list`, `get`, `set` | different | Subcommands instead of flags. `--global` and `--local` are accepted where they apply (`set` takes `--global` only; the default is the workspace). Keys are pyn's own. `pyn config init` is pyn only: it prompts for the user settings. |
| `git config --unset`, `-e`, `--system`, `--file` | none | unsupported | Usage error (`129`). |
| `git fsck` | `pyn verify [<path>]` | different | Checks every revision's hash and its link to the one before, and the audit log's chain. Silent and exit 0 when intact, as git; otherwise a `PROBLEM`, `WHAT`, `PATH` table and exit 1. `-v` / `--verbose` also prints what was checked. See [integrity](integrity.md). |
| `git reflog` | `pyn audit` | different | Append-only audit log of lock, revision and access events, filtered with `--path`, `--actor`, `--action`, `--before`, `--limit`. It is the closest analogue and is permission-gated (`view_audit`). |
| `git blame`, `git annotate` | none | unsupported | Git-command message. Line history needs merge-capable text history; not scheduled. |
| `git shortlog`, `git describe`, `git rev-parse`, `git archive`, `git bundle`, `git format-patch`, `git am`, `git apply`, `git notes`, `git bisect`, `git worktree`, `git submodule`, `git gc`, `git fsck` | none | unsupported | `pyn: '<cmd>' is not a pyn command` (`1`). Only the commands in the git-only list print the "git command pyn does not support yet" message, so these read as unknown until that list is widened (#223). |

## Branching and merging

Nothing here exists yet; every row is planned and fails with a message today. The model is decision #233 (commit graph,
per-commit trees, branch heads); policy per branch is #238; exclusive paths keep one linear history across branches (decision #28).

| Git | pyn | Status | Notes |
| --- | --- | --- | --- |
| `git branch [<name>]`, `-d`, `-m`, `-a`, `-v` | `pyn branch ...` | planned | #235. |
| `git switch <branch>`, `-c <new>` | `pyn switch ...` | planned | #235. |
| `git checkout <branch>`, `-b`, `-- <path>` | `pyn checkout ...` | planned | #235. Reserved for git's meaning; today it fails and points at `pyn lock`. |
| `git merge <branch>`, `--abort`, `--no-ff` | `pyn merge ...` | planned | #236, with #234. Shared files merge three-way; exclusive files are never merged and conflict only by lock. |
| `git rebase`, `git cherry-pick`, `git revert` | none | unsupported | See [Not planned](#not-planned). |

## Remotes

| Git | pyn | Status | Notes |
| --- | --- | --- | --- |
| `git remote [-v]`, `add`, `remove`, `set-url` | none | planned | #237 and #223. |
| `git fetch [<remote>]` | none (`pyn update`) | planned | #237. |
| `git pull [<remote> <branch>]` | none (`pyn update`) | planned | #237. |
| `git push [<remote> <branch>]` | none | planned | #237. Today `pyn commit` already writes to the server, so `push` has nothing to publish until local-only commits exist. |
| `git clone` | `pyn clone` | same | See the daily set. |

## pyn only

No git equivalent; these keep their own names and are never aliased to a git command.

| Command | What it does |
| --- | --- |
| `pyn lock <path> [--base <rev>]` | Take the leased lock on an exclusive file. |
| `pyn unlock <path> [--force --reason <text>]` | Give a lock up without committing, or remove another user's (needs `force_unlock`). |
| `pyn locks [--mine]` | List live locks in the repository, or yours everywhere. |
| `pyn policy preview` | Show which paths would change mode, and which locks would be released, if a policy file were committed. |
| `pyn audit` | Audit log (see `reflog` above). |
| `pyn get <path>`, `files`, `ls`, `summary`, `update` | Content, listings, repository summary and workspace update (see above). |
| `pyn login`, `logout`, `register`, `password`, `whoami`, `key`, `token` | Accounts, sessions, SSH keys and API tokens. |
| `pyn invite`, `user`, `member`, `role` | Invitations, repository users, roles and what each role grants. |
| `pyn org`, `pyn team` | Organizations and teams, and granting teams roles on repositories. |
| `pyn repo` (`create`, `list`, `explore`, `visibility`, `usage`, `delete`) | Repositories live on the server; there is no local `git init` equivalent. |
| `pyn setup`, `pyn admin`, `pyn usage` | First-run setup, server administration and per-owner usage and limits. |

## Not planned

Fail with the git-command message today (the unsupported list in `crates/pyn-cli/src/unsupported.rs`); no issue
schedules them.

| Git | Reason |
| --- | --- |
| `git reset`, `git clean` | No local history to reset; `pyn restore --staged`, `pyn restore` and `pyn update` cover the daily cases. |
| `git rm`, `git mv` | Not a CLI verb yet; no issue schedules removing or renaming paths from the CLI. |
| `git rebase`, `git cherry-pick`, `git revert` | History is append-only on the server. |
| `git blame`, `git grep` | Not scheduled. |
| Commands that maintain git's own storage or run its hooks (`gc`, `fsck`, `bundle`, `submodule`, `worktree`, hooks in `.git/hooks`) | pyn reads a git repository's `.git/` on a git home (decision #245) but does not maintain it or run its hooks. pyn's own hooks are tracked in #216. |

## On a git home

Decision #245: a repository has one home, a pyn server or git. In a git repository (a `.git` is found) pyn behaves as git,
through its own implementation of git's object store, refs and index, with no git library or binary (decision #30). A
`.pyn/pyn.toml` there is ignored: no path is exclusive, and the commands that need a pyn server refuse and name it. Going
from a git home to a pyn server is one explicit step, `pyn convert` (issue #95), which does not exist yet.

**Nothing git sees changes except what you asked for.** Tracked files, notes, hooks and line endings are never touched, and
the work tree is only ever read. Reading commands change nothing in `.git/`. `pyn init` writes `info/exclude` once. `pyn
add`, `pyn restore --staged` and `pyn commit -a` write the index, and `pyn commit` adds objects and moves the current branch;
those are the changes asked for, and [staging](#staging-and-committing) lists exactly what each writes. A test asserts that
the `.git` directory and the work tree are byte-identical before and after every reading command and after every refusal.

### Which home

pyn looks upward from the working directory for the nearest folder that holds either of these:

- a `.git`: a folder, or a `gitdir:` file as in a linked worktree or a submodule;
- a pyn server workspace: a `.pyn` folder that `pyn clone` filled in (it holds `local_only/state/files.toml`).

The nearer one decides, so a clone made inside a git repository is a pyn workspace and a git repository inside a pyn
workspace is a git home. In one folder, the pyn server workspace wins, which is also how a converted repository will say it
has moved. Any other `.pyn` folder, such as one `pyn init` created or a committed `.pyn/pyn.toml`, leaves the home git.
`GIT_DIR` and `GIT_WORK_TREE` are not read.

`pyn init` in a git repository adds `.pyn/` to `.git/info/exclude` (the shared one, found through the `.git` file in a
worktree or submodule) unless a line there already hides it, and creates `.pyn/local_only/`, where pyn keeps its own state
for a git home. It is safe to run again.

### Ignored files

`status` honors git's ignore rules in full: every `.gitignore` from the work tree root down (a deeper file wins),
`.git/info/exclude`, and the global excludes file (`core.excludesFile`, else `$XDG_CONFIG_HOME/git/ignore`, else
`~/.config/git/ignore`). Patterns follow git's own matching: negation, folder-only patterns, `**`, character classes,
escapes and `core.ignoreCase`. A file inside an ignored folder cannot be re-included, and a tracked file stays tracked
whatever the rules say.

### Commands

| Command | On a git home |
| --- | --- |
| `status` | Header (`On branch main`, or `HEAD detached at <id>`), a `STAGED`, `UNSTAGED`, `PATH` table, and a count line; `nothing to commit, working tree clean` when clean. States are `added`, `modified`, `deleted`, `typechange`, `untracked` and `unmerged`. Staged is `HEAD` against the index, unstaged is the index against the work tree, so a path can be in both. A folder with nothing tracked in it is listed once, as `dir/`. Paths are from the repository root. Mode changes count as modified; symlinks are compared by their target. |
| `diff` | Index against the work tree; one commit against the work tree (the paths in the commit or the index); two commits, or `a..b`. A name that is both a revision and a file needs `--`. Shows `old mode` and `new mode`, symlinks and binary files as git does. `--cached` and `--staged` compare the index with `HEAD` (or with one named commit). |
| `show [<rev>]` | Commit header and message, then the diff against the first parent. A merge commit prints no diff (git prints a combined diff, which pyn does not compute). |
| `log [-n <n>] [<path or rev>]` | Git's output: commit blocks, newest first across all parents, with git's history simplification for a path. The argument is a path if one exists (now or at `HEAD`), else a revision. `--commits` is the pyn table. `--filter` fails. |
| `files`, `ls` | `files` lists the index and `ls` lists `HEAD`'s tree. Every entry is `shared`, with no revision, lock or last change. |
| `add`, `restore --staged`, `commit` | See [staging](#staging-and-committing). |
| `init` | See above. |
| `login`, `logout`, `register`, `password`, `key`, `token`, `user`, `org`, `team`, `repo`, `usage`, `setup`, `admin`, `whoami`, `member`, `role`, `invite` | Run as anywhere else; see [Refused commands](#refused-commands). |
| `lock`, `unlock`, `locks`, `policy`, `audit`, `verify`, `summary` | Refuse; see [Refused commands](#refused-commands). |
| `config`, `clone` | Unchanged: they do not depend on the repository's home. A clone made inside a git repository is its own pyn workspace. |

Revisions are `HEAD`, `@`, a branch, a tag (annotated tags are followed to their commit), a full or abbreviated id of 4 or
more digits, and any of `~N`, `^` and `^N` after one. SHA-1 and SHA-256 repositories are both read. Dates print in the
offset recorded in the commit, as git does.

Differences from git, all deliberate or not yet done:

- Files are read as they are on disk: `core.autocrlf`, `.gitattributes` filters and `core.eol` are not applied, so a
  repository that depends on them can show files as modified that git does not.
- No rename or copy detection: a rename is a delete and an add.
- Submodule entries are not compared, and a submodule folder is not descended into.
- `log` and `show` print no ref decorations; commit abbreviations are 7 digits and are not lengthened to stay unique;
  `diff --stat` prints `Bin` without sizes for a binary file.
- A nested repository that the index does not track is one untracked `dir/`.

### Staging and committing

`add`, `restore --staged`, `commit` and `diff --cached` work on git's own index, so git sees the same state and the two tools
can be mixed freely.

**Index.** `.git/index` of the work tree (a linked worktree has its own) is read and written through `pyn-git`. Versions 2, 3
and 4 are read and the version found is the version written (a new index is version 2); the entry stat data, the stage
bits, assume-valid, skip-worktree and intent-to-add flags are kept. The optional cache extensions (`TREE`, `UNTR`, `REUC`,
`FSMN` and the like) are not kept: git rebuilds them, at the cost of one slower first command. A split or sparse index is
refused. The file is written through `index.lock` and a rename, as git does, and pyn first checks that nobody changed the index
while it was working; an existing `index.lock` stops the command.

**`add`.** Writes the blob of each file as a loose object and a stage 0 entry with the file's stat data, mode (`100644`,
`100755`, or `120000` for a symlink, whose target is the blob) and id. A tracked file that was deleted is staged as a removal.
`core.filemode=false` keeps the index's mode. Ignore rules are `.gitignore` in every folder, `.git/info/exclude` and the global
excludes file; a file or folder named on the command line that they hide is refused with git's message and exit status `1`
(the other paths are still staged) unless `-f` is given, and a hidden file met while walking a folder is skipped silently. A
nested git repository is skipped with a warning (pyn adds no submodules), and `.pyn/local_only` is never added. An unmerged
path is refused.

**`commit`.** A linear commit on the current branch:

1. It refuses, changing nothing, a detached `HEAD`, a merge, cherry-pick, revert, rebase or sequencer in progress,
   unmerged index entries, and `commit.gpgsign=true` (pyn cannot sign yet).
2. The author and committer are `GIT_AUTHOR_*` and `GIT_COMMITTER_*` (name, email, date) when set, else `author.*` and
   `committer.*`, else `user.name` and `user.email`; without a name and email it stops. The time zone is the system's.
3. With `-a`, every changed tracked file is staged first, in memory; the index is written only once the commit is built.
4. The index becomes trees (written as loose objects), and the commit has `HEAD` as its only parent. A tree equal to the parent's
   is `nothing to commit` (exit `1`). The message gets git's whitespace cleanup: no trailing blanks, no leading or trailing blank
   lines, a final newline.
5. The index is written (only with `-a`), then the branch ref is moved through its `.lock` file after checking `HEAD` did not move.

There is no reflog entry, no hook runs (a warning names hooks that exist), and no `COMMIT_EDITMSG` is written. A commit made
this way has the object id git would give the same tree, parent, identity, dates and message. `commit <path>` is refused; `add`
the paths and run `commit`.

**`restore --staged`.** Replaces the index entries under the paths with `HEAD`'s (or `--source`'s), removing entries `HEAD`
lacks. The replaced entries carry no stat data, so git re-reads those files on its next status.

**`diff --cached`.** `HEAD` (or the named commit) against the index, in the same output as `diff`.

### Refused commands

Only the commands that act on the repository in the working directory refuse, with exit `128` and this message, naming the
home that has them:

```text
fatal: `pyn lock` needs a pyn server, and this repository is a git repository; `pyn convert` moves it to a pyn server
```

These are `lock`, `unlock`, `locks`, `policy`, `audit`, `verify` and `summary`.

The account and server-level commands run from anywhere, including inside a git repository, because they address a server
through the global config (`--server`, `PYN_SERVER`, the user config file) and not the repository in the working directory:
`login`, `logout`, `register`, `password`, `key`, `token`, `user`, `org`, `team`, `repo`, `usage`, `setup`, `admin` and
`whoami`. So are `member`, `role` and `invite`, which address one repository on a server: a git home has no workspace to name
it, so the repository comes from `--repo owner/name`, `PYN_REPO` or the user config, and without one they fail with
`no repository: this is a git repository, so pass --repo owner/name (or set PYN_REPO)` instead of the refusal above. The same
holds for `user add`, `whoami` (it prints the account when no repository is given) and `token create` without `--repos`. A
`.pyn` folder in a git repository is never read for these commands.

Commands a git home will gain fail with exit `1`, as the other git-only commands do, and name the issue:

| Command | Needs |
| --- | --- |
| `restore` (work tree) | Writing the work tree, #254. |
| `branch`, `switch`, `checkout`, `merge`, `tag`, `stash` | Writing refs, the work tree and objects, #254. |
| `fetch`, `pull`, `push`, `remote`, `update` | Git transport, #255. |
| `get` | Per-path revisions, which git has no equivalent of; not scheduled. |

`pyn convert` fails with `pyn: 'convert' is not available yet` and names #95.

## Open items

- Unknown flags on supported commands are a usage error (`129`) from the argument parser, not a message that says the git
  flag is unsupported; making those messages explicit and pointing at the nearest pyn equivalent is #222's intent and is
  tracked with #223 and #224.
- `pyn status` and `pyn commit` output shapes are not aligned with git's, and `-C <dir>` and other global git flags are
  not taken.
