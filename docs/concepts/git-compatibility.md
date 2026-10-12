# Git compatibility

Decision #218: where pyn has an equivalent of a git command it uses git's name, flags, argument order and output shape,
so `alias git=pyn` works for everyday flows. pyn owns what git lacks (`lock`, `unlock`, `policy`, modes, leases,
organizations and teams). This page is the compatibility table: every difference from git is deliberate and listed here.
The table describes the CLI as it is; rows that say *planned* name the issue that will change them. Conformance tests
against git are tracked in #224.

## Conventions

Divergence rules:

1. **Every divergence is deliberate and listed.** A command, flag or argument form that differs from git has a row with a
   reason. If it is not in a table below, pyn behaves as git does or the gap is a bug.
2. **Never silently different.** A flag or argument form pyn cannot honour fails with a message; it is never ignored and
   never does something else. Where a `fatal:` message explains the refusal (for example `--cached`), the table says so.
3. **A git command with no pyn equivalent says so.** It prints `pyn: '<cmd>' is a git command pyn does not support yet; see
   docs/concepts/git-compatibility.md.` and exits `1`, not a usage error. A name that is neither pyn's nor a known git
   command prints `pyn: '<cmd>' is not a pyn command. See 'pyn --help'.` and exits `1`, as git does for an unknown command.
4. **`checkout` is reserved for git's meaning** (switch branches, restore paths). Taking a lock is `pyn lock`. Until
   branches land (#235) `pyn checkout` fails with a message saying so and pointing at `pyn lock`.
5. **`checkin` is gone.** `pyn commit` is the command and there is no alias (decision #229, renamed in #230). `pyn checkin`
   is an unknown command. The server's `checkin` permission, route and audit action keep their names (API contract).
6. **pyn-owned commands are listed as pyn only** and are never given a git name: `lock`, `unlock`, `locks`, `policy`,
   `audit`, and the account, organization, team and server administration commands.
7. **Git's object format and wire protocol are out of scope** (decision #30). No git dependency; commits are identified
   by `cN`, `HEAD` and `HEAD~N` until hashes land (#206). A git import and export bridge may come later.
8. **Listings are aligned tables** with the path or variable column last, not git's free-form blocks. This is a
   repository-wide difference: it keeps columns such as mode, lock and revision readable.

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
| `git init` | none | planned | Fails with the git-command message. Repositories are created on the server with `pyn repo create` and then cloned; a local `init` has no issue yet (epic #219). |
| `git clone <repo> [<dir>]` | `pyn clone <source> [<dir>]` | same | Source is `<server>/owner/name`, or `owner/name` on the configured server. Output is one summary line, not progress. |
| `git clone --depth`, `-b`, `--branch`, `--bare`, `--mirror`, `-o`, `-n`, `--recurse-submodules` | none | unsupported | Usage error (`129`). `-b` and `--branch` become possible with branches (#235). Every clone has full history on the server; there is no local object store to shallow. |
| `pyn clone --revision <commit>` | | pyn only | Clone as of a commit (`cN`, `HEAD~N`). Git does this with `clone` then `checkout`. |
| `git status` | `pyn status` | different | Table with mode, revision, state, lock and path plus a summary line, because mode and lock state have no place in git's layout. |
| `git status -s`, `--short`, `--porcelain`, `-b`, `<pathspec>...` | none | unsupported | Usage error (`129`). A stable machine format is not scheduled. |
| `pyn status --revision <commit>` | | pyn only | Compare the workspace with a past commit (`behind` / `ahead`). |
| `git add <path>...` | none | planned | Fails with the git-command message. pyn has no index: `commit` takes paths directly. Whether a staging notion is added is open in #223. |
| `git commit -m <msg> [<path>...]` | `pyn commit -m <msg> <path>...` | different | One atomic commit of the named paths, uploaded to the server; there is no index and no local-only commit, so commit and publish are one step (no `push` needed). Paths are required and `-m` is required (no editor). Releases the locks it held. Output is `<path> is now at revision N`, not git's summary. |
| `git commit -a`, `-F`, `--amend`, `--allow-empty`, `-s`, `-S`, `--no-verify`, `--author`, `--date`, `-v` | none | unsupported | Usage error (`129`). Commits are immutable on the server, so `--amend` will not exist; `-F` is not accepted because `--file` already means the content file. |
| `pyn commit --file <local>` | | pyn only | Content file for a single path. In a workspace the file at the path is the default. |
| `pyn commit --base <rev>` | | pyn only | The revision the edit started from; the server rejects the commit if the head moved on an exclusive path. |
| `git log [-n <n>] [<path>]` | `pyn log [-n <n>] [<path>]` (alias `history`) | different | Without a path, newest first, 50 by default. With a path, oldest first and all revisions (git prints newest first). Output is a table of per-path revisions, not commit blocks. `-n` is also `--max-count`; `--limit` is an alias. |
| `git log --oneline`, `--graph`, `--stat`, `-p`, `--author`, `--since`, `--grep`, `--format`, `--follow`, `<rev>..<rev>`, `<rev> -- <path>` | none | unsupported | Usage error (`129`). A revision range is read as a path. Branch-aware log arrives with #235. |
| `pyn log --commits` | | pyn only | List commits (`COMMIT`, `AUTHOR`, `WHEN`, `FILES`, `MESSAGE`) instead of revisions. |
| `pyn log --filter <glob>` | | pyn only | Only paths matching the glob. |
| `git diff [<c1> [<c2>]] [-- <path>...]` | `pyn diff [<c1> [<c2>]] [-- <path>...]` | different | Unified output as git, computed in the CLI. Commits are `cN`, `HEAD`, `HEAD~N`, or `c1..c2`; two commits work outside a workspace. With no commit it compares with the last update (git compares with the index). Created and deleted files always print mode `100644`. |
| `git diff -U<n>`, `--unified=<n>`, `--stat`, `--name-only`, `--name-status`, `--exit-code`, `-q` | same | same | `--name-status` prints `A`, `M`, `D` only. |
| `git diff --cached`, `--staged` | none | unsupported | `fatal: pyn has no staging area` (`128`). |
| `git diff --shortstat`, `-w`, `--word-diff`, `--color`, `--no-index`, `--check`, `-M`, `-C` | none | unsupported | Usage error (`129`). |
| `git show [<commit>]` | `pyn show [<commit>]` | different | Commit header (`commit`, `Author:`, `Date:`), message, then the commit's diff. The newest commit if none is named. Commit names are `cN`, `HEAD`, `HEAD~N`. |
| `git show -U<n>`, `--stat`, `--name-only`, `--name-status` | same | same | |
| `git show <rev>:<path>` | `pyn get <path> [--rev N] [-o <file>]` | different | `show` takes commits only (`HEAD:foo` fails with `fatal: ... is not a commit`, `128`). File content at a revision is `pyn get`, to stdout or a file. `--rev` is a per-path revision, not a commit. |
| `git show --oneline`, `-s`, `--format`, `<tag>`, `<object>` | none | unsupported | Usage error (`129`) or `fatal: ... is not a commit`. |
| `git restore --source=<rev> <path>` | `pyn restore --source <rev> <path>` (or `pyn restore <path> <rev>`) | different | pyn's restore appends a new revision on the server: it needs the lock, the `restore` permission and a typed confirmation (`--yes` skips it); `-m` sets the message. The revision is required (git defaults to the index or HEAD) and `-s` is the short form of `--source`. Git's restore only edits the working tree. |
| `git restore --staged`, `--worktree`, `-p`, `--ours`, `--theirs`, `<pathspec>...` | none | unsupported | Usage error (`129`). One path per call. |
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
| `git ls-files` | `pyn files` | different | Table of every file with mode, head revision and lock holder. No flags. |
| `git ls-tree [<rev>] [<path>]` | `pyn ls [<path>]` | different | One folder with each entry's mode, last change and lock. Head only; no revision argument. |
| `git cat-file -p`, `git show <rev>:<path>` | `pyn get <path>` | different | See `show` above. |
| `git config --list`, `--get <key>`, `<key> <value>` | `pyn config list`, `get`, `set` | different | Subcommands instead of flags. `--global` and `--local` are accepted where they apply (`set` takes `--global` only; the default is the workspace). Keys are pyn's own. `pyn config init` is pyn only: it prompts for the user settings. |
| `git config --unset`, `-e`, `--system`, `--file` | none | unsupported | Usage error (`129`). |
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
| `git reset`, `git clean` | No index and no local history to reset; `pyn restore` and `pyn update` cover the daily cases. |
| `git rm`, `git mv` | Not a CLI verb yet; no issue schedules removing or renaming paths from the CLI. |
| `git rebase`, `git cherry-pick`, `git revert` | History is append-only on the server. |
| `git blame`, `git grep` | Not scheduled. |
| Anything that depends on git's object format, packfiles, `.git/` or the wire protocol (`gc`, `fsck`, `bundle`, `submodule`, `worktree`, hooks in `.git/hooks`) | Out of scope by decision #30. pyn's own hooks are tracked in #216. |

## Open items

- Unknown flags on supported commands are a usage error (`129`) from the argument parser, not a message that says the git
  flag is unsupported; making those messages explicit and pointing at the nearest pyn equivalent is #222's intent and is
  tracked with #223 and #224.
- `pyn status` and `pyn commit` output shapes are not aligned with git's, and `-C <dir>` and other global git flags are
  not taken.
