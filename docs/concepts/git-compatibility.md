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
4. **`checkout` is reserved for git's meaning** (switch branches, restore paths). Taking a lock is `pyn lock`; a `checkout`
   that names neither a branch nor a file points at it.
5. **`checkin` is gone.** `pyn commit` is the command and there is no alias (decision #229, renamed in #230). `pyn checkin`
   is an unknown command. The server's `checkin` permission, route and audit action keep their names (API contract).
6. **pyn-owned commands are listed as pyn only** and are never given a git name: `lock`, `unlock`, `locks`, `policy`,
   `audit`, and the account, organization, team and server administration commands.
7. **Commits are git commits** (decision #245): identified by their git commit id (SHA-256 on a pyn server), shown
   abbreviated, and named by hash, `HEAD`, `~N` and `^`; there are no `cN` numbers. Git's wire protocol is spoken in process
   (see [Transports and credentials](#transports-and-credentials)), and there is no dependency on git (decision #30): git does not
   need to be installed. A git import and export bridge may come later.
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
| `git clone <repo> [<dir>]` | `pyn clone <source> [<dir>]` | same | A source that is `<server>/owner/name` or `owner/name` on the configured server is cloned from a pyn server, and prints one summary line, not progress. A git URL (`https://`, `ssh://`, `user@host:path`) is cloned into a git repository with git's behaviour, flags and output; see [Clone from a git URL](#clone-from-a-git-url), which also says how an `http(s)` address is told apart. |
| `git clone -b`, `--bare`, `-o`, `-n`, `--single-branch`, `--no-tags`, `-c`, `-q`, `--progress` | `pyn clone ...` | git URL: supported | Git's behaviour. On a pyn server clone they are a usage error (`129`): that clone takes everything the server has, on its default branch. |
| `git clone --depth`, `--shallow-*`, `--filter`, `--mirror`, `--recurse-submodules`, `--sparse`, `--reference`, `--separate-git-dir`, `--template`, `--local`, `--no-hardlinks` | none | unsupported | Usage error (`129`) naming the flag: shallow, partial, mirror and submodule clones are not built, and a local-path source is refused by the transport. |
| `pyn clone --revision <commit>` | | pyn only | Clone as of a commit (a hash, `HEAD~N`). Git does this with `clone` then `checkout`. |
| `pyn clone --no-local-store` | | pyn only | A pyn server clone keeps a partial local store of the history under `.pyn/local_only/store/` and commits locally by default (`--local-store` says the same); this opts out and commits straight to the server. See [the local store](workspace.md#the-local-store). A usage error for a git URL. |
| `pyn migrate` | | pyn only | Converts a workspace made before local commits; see [migrating](workspace.md#migrating-a-workspace). |
| `git status` | `pyn status` | different | Header `On branch <name>`, then a table with mode, revision, staged, state, lock and path plus a summary line, because mode and lock state have no place in git's layout. `STAGED` is `added` or `modified` for a staged path, and `STATE` then describes what changed after it was staged. On a git home the table is `STAGED`, `UNSTAGED`, `PATH` with git's states. |
| `git status -s`, `--short`, `--porcelain[=v1]`, `-b`, `--branch`, `<pathspec>...` | same | same | Git's `XY <path>` lines; see [the short format](#the-short-status-format). Pathspecs are literal files and folders, taken relative to where you stand, and limit every format. `-z`, `--porcelain=v2`, `--ignored`, `-u<mode>`, `--ahead-behind` and glob or magic pathspecs are a usage error (`129`). `pyn status --revision` fails on a git home. |
| `pyn status --revision <commit>` | | pyn only | Compare the workspace with a past commit (`behind` / `ahead`). |
| `git add <path>...` | `pyn add <path>...` | same | Stages a path, a folder or `.`; a path is required unless `-u` or `-A` is given. On an exclusive path of a pyn server it fails unless you hold the lock (see below). |
| `git add -u`, `-A`, `-n`, `-v`, `-f` and their long forms | same | same | `-u` and `--update` stage tracked files only; `-A` and `--all` take new files too and, with no path, cover the whole repository. `-n` and `--dry-run` print `add '<path>'` and `remove '<path>'` and stage nothing. Ignored files are refused (exit `1`, as git) without `-f`. |
| `git add -p`, `-i`, `-e`, `--intent-to-add`, `--chmod`, `--renormalize`, `--pathspec-from-file`, glob and magic pathspecs | none | unsupported | Usage error (`129`), or `pathspec ... did not match any files` for a glob the shell did not expand. A path is a file or a folder, taken relative to where you stand. |
| `git commit -m <msg>` | `pyn commit -m <msg>` | different | Commits what is staged as one unit of work. On a pyn server it is one changeset uploaded to the server, so commit and publish are one step (no `push` needed), it releases the locks it held, and the output is `<path> is now at revision N` for each file. On a git home it is a git commit on the current branch and prints `[<branch> <id>] <subject>`. `-m` is required (no editor) and repeats for more paragraphs. Nothing staged exits `1` with a message. |
| `git commit -a`, `-am <msg>` | `pyn commit -a`, `-am <msg>` | same | Stages every changed tracked file, then commits; new files are not included. |
| `git commit <path>...` | `pyn commit -m <msg> <path>...` | different | On a pyn server: one atomic commit of the named paths as they are on disk, whatever is staged (git's `--only`). Not available on a git home, which says to `add` the paths first. |
| `git commit -F <file>`, `--file=<file>`, `--allow-empty`, `--author=<name <email>>`, `--date=<date>`, `-s`, `--signoff`, `--amend`, `--no-edit` | same | git home: supported | Git's behaviour on a git home; on a pyn server a usage error (`129`) naming the flag, because commits there are immutable (no `--amend`) and `--file` is the content file. See [Commit flags on a git home](#commit-flags-on-a-git-home). |
| `git commit -S`, `--no-verify`, `-v`, `-c`, `-C`, `--fixup`, `--squash`, `--reset-author`, `--allow-empty-message`, `-e` | none | unsupported | Usage error (`129`). |
| `pyn commit --file <local>` | | pyn only | Content file for a single path. In a workspace the file at the path is the default. |
| `pyn commit --base <rev>` | | pyn only | The revision the edit started from; the server rejects the commit if the branch has moved on from it on an exclusive path. |
| `git log [-n <n>] [<path>]` | `pyn log [-n <n>] [<path>]` (alias `history`) | different | Without a path, newest first, 50 by default. With a path, oldest first and all revisions (git prints newest first). It lists the workspace's branch (`On branch <name>` goes to standard error). Output is a table of per-path revisions, not commit blocks. `-n` is also `--max-count`; `--limit` is an alias. On a git home it is git's output (see below). |
| `git log --oneline`, `--format=`, `--pretty=`, `--date=`, `--author`, `--committer`, `--grep`, `-i`, `--invert-grep`, `--all-match`, `--since`, `--until`, `--no-merges`, `--merges`, `--skip`, `--reverse`, `--stat`, `--shortstat`, `--name-only`, `--name-status`, `-p`, `-U<n>`, `<rev>`, `<a>..<b>`, `<a>...<b>`, `^<rev>`, `[<rev>...] -- <path>...` | same | git home: supported | Git's output on a git home; see [Log on a git home](#log-on-a-git-home). On a pyn server a usage error (`129`) naming the option: that log is a per-path table of revisions. |
| `git log --graph`, `--follow`, `--decorate`, `--all`, `-S`, `-G`, `--first-parent`, `--show-signature`, `--notes`, `-E`, `-F`, `-P`, `-<n>` | none | unsupported | Usage error (`129`). Rename-aware output is #274. `pyn log` is of the workspace's branch; another branch's log needs a switch (`pyn log <branch>` is not built). |
| `pyn log --commits` | | pyn only | List commits (`COMMIT`, `AUTHOR`, `WHEN`, `FILES`, `MESSAGE`) instead of revisions. |
| `pyn log --filter <glob>` | | pyn only | Only paths matching the glob. |
| `git diff [<c1> [<c2>]] [-- <path>...]` | `pyn diff [<c1> [<c2>]] [-- <path>...]` | different | Unified output as git, computed in the CLI. Commits are hashes (or abbreviations of 4 or more digits), `HEAD`, `~N`, `^`, or `a..b`; two commits work outside a workspace. With no commit it compares the work tree with the index: the staged content, or the last update for what is not staged. File modes print as in git, including `old mode` and `new mode`. |
| `git diff -U<n>`, `--unified=<n>`, `--stat`, `--name-only`, `--name-status`, `--exit-code`, `-q` | same | same | `--name-status` prints `A`, `M`, `D` only. |
| `git diff --cached`, `--staged` | `pyn diff --cached`, `--staged` | different | The staged content against the last commit (the last update on a pyn server). On a git home it takes one optional commit to compare with, as git; on a pyn server it takes none. |
| `git diff --shortstat`, `-w` (`--ignore-all-space`), `--check` | same | same | Both homes. `--shortstat` prints only the totals line. `-w` compares lines with all whitespace removed, prints context lines as they are in the new file, and leaves out a file that differs only in whitespace (so `--exit-code` is `0` when nothing else changed); `--name-only` and `--name-status` still list it, as git 2.43 does (newer git versions differ on this edge; pyn lists the file). `--check` prints `path:line: trailing whitespace.` (or `space before tab in indent.`, or both) and the added line, then `path:line: new blank line at EOF.`, for git's default whitespace rules, and exits `2` if it printed anything; `-w` does not affect it (newer git versions differ here too). `--check` and `--shortstat` combine with no other format (`--stat`, `--name-only`, `--name-status`): a usage error (`129`), where git prints both stats or refuses with `128`. |
| `git diff --word-diff`, `--color`, `--no-index`, `-M`, `-C` | none | unsupported | Usage error (`129`). |
| `git show [<commit>]` | `pyn show [<commit>]` | different | Commit header (`commit`, `Author:`, `Date:`), message, then the commit's diff. The newest commit if none is named. The author line carries the email. On a git home the output is git's, except for merge commits (see below). |
| `git show -U<n>`, `--stat`, `--shortstat`, `-w`, `--check`, `--name-only`, `--name-status` | same | same | The diff flags behave as on `diff`; `--check` exits `2` when it reports a problem. |
| `git show <rev>:<path>` | `pyn get <path> [--rev N] [-o <file>]` | different | `show` takes commits only (`HEAD:foo` fails with `fatal: ... is not a commit`, `128`). File content at a revision is `pyn get`, to stdout or a file. `--rev` is a per-path revision, not a commit. |
| `git show --oneline`, `-s`, `--format`, `<tag>`, `<object>` | none | unsupported | Usage error (`129`) or `fatal: ... is not a commit`. |
| `git restore --source=<rev> <path>` | `pyn restore --source <rev> <path>` (or `pyn restore <path> <rev>`) | different | pyn's restore appends a new revision on the server: it needs the lock, the `restore` permission and a typed confirmation (`--yes` skips it); `-m` sets the message. The revision is required (git defaults to the index or HEAD) and `-s` is the short form of `--source`. Git's restore only edits the working tree. |
| `git restore --staged <path>...` | `pyn restore --staged <path>...` (`-S`) | same | Puts the staged entries under the paths back to the last commit and leaves the files alone. On a git home `--source <commit>` takes the entries from that commit, as git; on a pyn server there is no commit to take them from and it is a usage error. |
| `git restore [-s <rev>] [-S] [-W] <path>...` (the work tree) | `pyn restore ...` on a git home | git home only | Takes the paths from the index (or `--source`) into the work tree, and with `--staged --worktree` into both. See [Branches, switching and the work tree](#branches-switching-and-the-work-tree). On a pyn server `--worktree` is a usage error (`129`): a restore there makes an older revision the new head. |
| `git restore -p`, `--ours`, `--theirs`, `--merge` | none | unsupported | Usage error (`129`). On a git home `pyn checkout --ours -- <path>` and `--theirs` take a side of an unmerged path. |
| `git branch`, `git switch`, `git checkout` | `pyn branch`, `pyn switch`, `pyn checkout` | supported | See [branching and merging](#branching-and-merging). On a git home they work on `.git` as git does, see [Branches, switching and the work tree](#branches-switching-and-the-work-tree). |
| `git merge` | `pyn merge` | supported | See [branching and merging](#branching-and-merging). On a git home it merges as git does, see [Merging](#merging). |
| `git fetch` | `pyn fetch` on a git home | git home only | See [Fetch on a git home](#fetch-on-a-git-home). On a pyn server it fetches into the local store (a workspace cloned with `--no-local-store` runs `pyn update`), see [Remotes on a pyn server](#remotes-on-a-pyn-server). |
| `git pull` | `pyn pull` on a git home | git home only | Fetch, then merge through `pyn merge`; see [Pull on a git home](#pull-on-a-git-home). On a pyn server it fetches into the local store and merges (a workspace cloned with `--no-local-store` runs `pyn update`), see [Remotes on a pyn server](#remotes-on-a-pyn-server). |
| `git push` | `pyn push` on a git home | git home only | See [Push on a git home](#push-on-a-git-home). On a pyn server it publishes the local commits (a workspace cloned with `--no-local-store` prints that there is nothing to push, because a commit goes straight to the server); see [Remotes on a pyn server](#remotes-on-a-pyn-server). |
| `git remote` | `pyn remote` | git home; pyn server partly | On a git home it lists and edits the remotes in `.git/config`, see [Remotes](#remotes). On a pyn server `pyn remote` and `pyn remote -v` show the one remote, `origin`: the server and repository set by `--server` and `--repo` or `pyn config set server` and `repo`; the editing subcommands say so and exit `1`. |
| `git tag [-a] [-m <msg>] [-f] [<name> [<commit>]]`, `-d`, `-l` | `pyn tag ...` on a git home | git home only | See [Tags](#tags). On a pyn server it prints the git-command message (a server's commits are named by hash and have no tags yet, #223). |
| `git stash [push] [-u] [-m <msg>]`, `list`, `pop`, `apply`, `drop`, `clear` | `pyn stash ...` on a git home | git home only | See [Stashing](#stashing). On a pyn server it prints the git-command message. |

`pyn update [<path>...]` is the pre-branch stand-in for `fetch` plus `pull`: it fast-forwards files without local
changes and never overwrites an edit, reporting skipped files. `--revision <commit>` moves the workspace to an older or
newer commit. It is pyn only; on a pyn server `pyn fetch` and `pyn pull` run it on a pyn server. A git home has `pyn pull` instead, and `pyn update` is refused there.

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

Branches and merge exist on a pyn server ([branches](branches.md), [merging](merging.md)); remotes on a pyn server are the one server and repository (see [Remotes](#remotes)). The model is
decision #233 (commit graph, per-commit trees, branch heads); policy per branch is [branches](branches.md#policy-and-modes); exclusive paths form a tree of
revisions with a pointer per branch (decision #299).

| Git | pyn | Status | Notes |
| --- | --- | --- | --- |
| `git branch [<name> [<start>]]`, `-d`, `-D`, `-m`, `-l`, `-v`, `--show-current` | `pyn branch ...` | supported | `-d` refuses a branch that is not on the default branch (git: not merged into `HEAD`). `-a` and `-r` list the server as the remote `origin` (see [Remotes on a pyn server](#remotes-on-a-pyn-server)); `-M`, `-c` and patterns are not built; they are usage errors. The workspace's branch cannot be deleted. |
| `git switch <branch>`, `-c <new> [<start>]`, `-f` | `pyn switch ...` | supported | Local changes carry over where the target branch has the same file, and stop the switch (nothing changed, the paths listed) where it does not; `-f` discards them. `-`, `--detach` and `-C` are not built. Prints `Switched to branch '<name>'` on standard error, as git. |
| `git checkout <branch>`, `-b <new> [<start>]`, `-f`, `[<tree-ish>] -- <path>...` | `pyn checkout ...` | supported | Reserved for git's meaning. `-- <path>` restores from the revision the workspace has (git: the index; a staged path must be unstaged first), `<branch\|commit> -- <path>` from that branch or commit. There is no detached HEAD: `checkout <commit>` is refused with what to use instead. Taking a lock is `pyn lock`. |
| `git merge <branch\|commit>`, `-m`, `--no-ff`, `--ff-only`, `--no-commit`, `--abort`, `--continue` | `pyn merge ...` | supported | Shared files merge three-way with diff3 conflict markers; a binary conflicts as a whole file. Exclusive files take the pointer that descends from the other, and conflict where both branches changed one or a path became exclusive while a branch edited it (a person chooses a side, taking the lock). A conflicted merge leaves the workspace as git does and is concluded with `pyn add` and `pyn commit` (no `-m` needed), or given up with `--abort`. Differences: `--preview` (pyn's own) reports what would merge and conflict and changes nothing; a merge needs a clean index and no local edits to the files it touches; `--squash`, `-s`, `-X`, `--no-verify`, `--edit`, `--signoff`, `-S` and `--ff` are usage errors. `git checkout --ours` and `--theirs` are `pyn checkout --ours\|--theirs -- <path>`. `pyn status` marks unmerged paths in its `STATE` column. Recursive merge of criss-cross histories as git does, with the base of a conflict shown as `merged common ancestors`. |
| `git rebase`, `git cherry-pick`, `git revert` | none | unsupported | See [Not planned](#not-planned). |

## Remotes

| Git | pyn | Status | Notes |
| --- | --- | --- | --- |
| `git remote [-v]`, `add`, `remove`, `rename`, `set-url`, `get-url` | `pyn remote ...` | git home: supported | Configuration only, no network. Output, exit statuses and the resulting `.git/config`, refs and reflogs match git. See [Remotes on a git home](#remotes-on-a-git-home). On a pyn server: `remote` and `remote -v` only, see the table above. |
| `git ls-remote [--heads] [--tags] [--refs] [--symref] [--exit-code] [<repository> [<pattern>...]]` | `pyn ls-remote ...` | same | Works on a git home and anywhere with a URL. Output is git's: `<id>`, a tab and the name, with `<tag>^{}` for a peeled tag and `ref: <target>`, a tab and `HEAD` for `--symref`. `<repository>` is a remote name (default `origin`, which prints `From <url>` on standard error as git does) or a URL; see [Transports and credentials](#transports-and-credentials). `--get-url`, `--sort`, `-o`, `-q` and `--upload-pack` are usage errors (`129`). |
| `git remote add -f` | `pyn remote add -f ...` | git home: supported | Adds the remote, then runs `pyn fetch <name>`. If the fetch fails the remote stays defined, as in git. |
| `git remote show [-n] [<name>...]`, `prune [-n] <name>...`, `update [-p] [<group>\|<remote>...]`, `set-head <name> (-a \| -d \| <branch>)`, `set-branches [--add] <name> <branch>...` | `pyn remote ...` | git home: supported | Ask the remote over ssh or smart HTTP, with git's output and exit statuses; see [Querying remotes on a git home](#querying-remotes-on-a-git-home). Other options are usage errors (`129`). |
| `git fetch [<options>] [<remote>\|<url>] [<refspec>...]` | `pyn fetch ...` | git home: supported | Git's behaviour, flags, output and exit statuses; see [Fetch on a git home](#fetch-on-a-git-home). |
| `git pull [<remote> <branch>]` | `pyn pull` | git home: supported | See [Pull on a git home](#pull-on-a-git-home). Pyn server: runs `update`. |
| `git push [<options>] [<repository> [<refspec>...]]` | `pyn push ...` | git home: supported | Git's behaviour, flags, output and exit statuses; see [Push on a git home](#push-on-a-git-home). Pyn server: says there is nothing to publish, because `pyn commit` already writes to the server; local commits and a real `push` there are decision #300, built in #313. |
| `git clone` | `pyn clone` | same | See the daily set and [Clone from a git URL](#clone-from-a-git-url). |

### Remotes on a pyn server

A pyn server home has exactly one remote, `origin`: the server and repository it is set to use.

| Command | Behaviour |
| --- | --- |
| `pyn remote -v` | `origin`, a tab and `<server>/<owner>/<name> (fetch)`, then the same with `(push)`. |
| `pyn fetch`, `pyn pull` | Fetch into the local store and `pull` then fast-forwards, makes a merge commit when the histories diverged as git does, or with `--rebase` replays local commits ([commits](commits.md#fetch-pull-and-push-with-a-local-store)). With any argument or option they are usage errors (`129`), except `pull --rebase` and `--no-rebase`. A workspace cloned with `--no-local-store` runs `pyn update` instead and takes no arguments. |
| `pyn push` | Publishes the local commits, merge commits included, and exits `1` on a refusal. Arguments are usage errors. A workspace cloned with `--no-local-store` prints that there is nothing to push, because commits go straight to the server; exits `0`. |
| `pyn branch -r`, `-a` | `-r` lists the server's branches as `origin/<branch>`; `-a` lists the local ones, then `remotes/origin/<branch>`. `-v` works with both. Only listing is allowed with them. |

### Remotes on a git home

`pyn remote` reads and edits `remote.<name>.*` and `branch.<name>.*` in the repository's `.git/config` and the refs under
`refs/remotes/`. The subcommands in this table never contact a remote; those that do are under [Querying remotes on a git home](#querying-remotes-on-a-git-home).

| Command | Does |
| --- | --- |
| `pyn remote` | The remote names, sorted, one per line. |
| `pyn remote -v` | `<name>`, a tab and `<url> (fetch)` (the first URL), then one `<url> (push)` line for each push URL (`pushurl`, else each `url`). A remote with no URL prints `<name>` and a tab. |
| `pyn remote add <name> <url>` | Writes `url` and the default `fetch = +refs/heads/*:refs/remotes/<name>/*`. An invalid name is `fatal: '<name>' is not a valid remote name` (`128`), an existing one `error: remote <name> already exists.` (`3`). `-f` then fetches the remote (see [Fetch on a git home](#fetch-on-a-git-home)). |
| `pyn remote remove\|rm <name>` | Deletes the `[remote]` section, every ref and reflog under `refs/remotes/<name>/`, and the `branch.<b>.remote`, `merge` and `pushRemote` of branches that tracked it, and `remote.pushDefault` if it named it. A missing remote is `error: No such remote: '<name>'` (`2`). |
| `pyn remote rename <old> <new>` | Renames the section, moves `refs/remotes/<old>/*` (and its reflogs, with a `remote: renamed ...` line) to `<new>`, rewrites fetch refspecs whose destination is under `refs/remotes/<old>/`, and repoints `branch.<b>.remote`, `pushRemote` and `remote.pushDefault`. A refspec that does not fit is kept and reported as git does (`warning: Not updating non-default fetch refspec`). |
| `pyn remote set-url [--push] <name> <new> [<old>]` | Sets the URL, or replaces the URLs that contain `<old>`. `--add` appends one, `--delete` removes the URLs that contain `<url>` (refusing to remove every non-push URL). |
| `pyn remote get-url [--push] [--all] <name>` | The first (or every) URL; with `--push` the push URLs. |

Editing keeps every other line of `.git/config`, with its comments and order, and quotes values and subsection names as git does.
Differences from git: `<old>` and the `--delete` argument match as plain substrings, not regular expressions; `url.<base>.insteadOf` is not applied
to the URLs shown; legacy `.git/remotes/` and `.git/branches/` files are not read; `remove` deletes the refs under
`refs/remotes/<name>/` rather than those the fetch refspecs map.

### Querying remotes on a git home

These subcommands of `pyn remote` contact the remote (ssh or smart HTTP, see [Transports and credentials](#transports-and-credentials)) and
change nothing except what is stated. A remote that cannot be reached is `fatal:` (`128`). A name that is not a configured remote is taken as a
URL by `show -n`, `show` and `prune`, as git does.

| Command | Does |
| --- | --- |
| `pyn remote show [-n] [<name>...]` | Per remote: `* remote <name>`, the `Fetch URL` and each `Push  URL` (the name when none is set), `HEAD branch` (`(unknown)` when the remote has none, a list when it is ambiguous, `(not queried)` with `-n`), `Remote branches` with `tracked`, `new (next fetch will store in remotes/<name>)` or `stale (use 'git remote prune' to remove)` (a stale ref keeps its full `refs/remotes/...` name, as in git), `Local branches configured for 'git pull'` (`merges with remote`, `rebases onto remote` and the interactive forms from `branch.<b>.rebase`) and `Local refs configured for 'git push'` with `up to date`, `fast-forwardable`, `local out of date`, `create` or `delete`, or `Local refs will be mirrored by 'git push'` with `remote.<name>.mirror`. Pushes come from `remote.<name>.push`, else every branch both sides have. `-n` lists the branches stored under the fetch refspecs and the push refspecs as written, with `(status not queried)`. With no name it lists the remotes. It stops at the first remote it cannot reach. |
| `pyn remote prune [-n\|--dry-run] <name>...` | Deletes the refs a fetch refspec's destination covers whose source the remote lost (and their reflogs), printing `Pruning <name>`, `URL: <url>` and ` * [pruned] <ref>` (` * [would prune] <ref>` with `-n`) for each. Nothing stale prints nothing. |
| `pyn remote [-v] update [-p\|--prune\|--no-prune] [<group>\|<remote>...]` | Fetches each, as `pyn fetch --multiple` does (`Fetching <name>` on standard output, `error: Could not fetch <name>` and exit `1` on a failure). A name is a `remotes.<group>` first, else a configured remote, else `fatal: no such remote or remote group: <name>` (`1`). With none, the `remotes.default` group, else every remote except those with `skipFetchAll` or `skipDefaultUpdate`; a lone remote picked that way is fetched with no heading, as git does. `-p` prunes as `fetch --prune`; otherwise `remote.<name>.prune` and `fetch.prune` decide. `-v` is `fetch -v`. |
| `pyn remote set-head <name> (-a\|--auto \| -d\|--delete \| <branch>)` | Points `refs/remotes/<name>/HEAD` at `refs/remotes/<name>/<branch>`, at the branch the remote's `HEAD` names (`-a`, printing `<name>/HEAD set to <branch>`; `error: Cannot determine remote HEAD` or `error: Multiple remote HEAD branches...`, `1`, when it cannot tell), or deletes it. A branch with no tracking ref is `error: Not a valid ref: refs/remotes/<name>/<branch>` (`1`). Any other combination is a usage error. The wording of the `-a` message is that of git 2.43 and is provisional. |
| `pyn remote set-branches [--add] <name> <branch>...` | Replaces (or with `--add`, extends) the `fetch` refspecs with `+refs/heads/<branch>:refs/remotes/<name>/<branch>`. A missing remote is `error: No such remote '<name>'` (`2`). At least one branch is required; git accepts none and leaves the remote with no refspec. |

Differences from git: `show` reads `branch.<b>.rebase` only (not `pull.rebase`) and ignores `push.default`, as git's `show` does; a remote branch no fetch
refspec maps is not listed and never shown as `skipped`; the push status of a tag or other non-commit is `local out of date` when the ids differ;
`remote.<name>.followRemoteHEAD` and the legacy `.git/remotes/` files are not consulted by `set-head`.

### Fetch on a git home

`pyn fetch [<remote>|<url>] [<refspec>...]` brings objects and refs from a git remote over the system `ssh` or smart HTTP, with
no `git` program (see [Transports and credentials](#transports-and-credentials)). `<remote>` is a configured remote, a group
(`remotes.<name>`), or a URL. With no argument the remote is the current branch's `branch.<name>.remote`, else `origin`, else the
only remote there is; with none to pick it does nothing and exits `0`, as git does. A remote with no `url` is
`fatal: '<name>' does not appear to be a git repository` (`128`). Only the first URL is fetched from; `url.<base>.insteadOf` is
applied.

| Option | Does |
| --- | --- |
| `<refspec>...` | Replace the configured `remote.<name>.fetch`. A name that matches nothing is `fatal: couldn't find remote ref <name>` (`128`); two sources for one local ref is `fatal: Cannot fetch both ... and ... to ...`. `tag <name>` means `refs/tags/<name>:refs/tags/<name>`. A destination without `refs/` is under `refs/heads/` (`heads/`, `tags/` and `remotes/` are under `refs/`). With no destination the value goes to `FETCH_HEAD` only. |
| `--all` | Every remote in configuration order except those with `remote.<name>.skipFetchAll` or `skipDefaultUpdate`; prints `Fetching <name>` on standard output, reports a failure as `error: Could not fetch <name>` and goes on, and exits `1` if any failed. A repository argument or refspecs with it is `fatal` (`128`). `--multiple` takes every argument as a remote or group name. |
| `-a`, `--append` | Append to `FETCH_HEAD` instead of replacing it. |
| `-p`, `--prune`, `--no-prune` | Delete refs that a fetch refspec's destination covers whose source the remote lost, before fetching, printing `- [deleted]         (none)     -> <ref>`. Default: `remote.<name>.prune`, then `fetch.prune`. |
| `-P`, `--prune-tags`, `--no-prune-tags` | With pruning, also `refs/tags/*:refs/tags/*` (fetching every tag and deleting the ones the remote lost). Default: `remote.<name>.pruneTags`, then `fetch.pruneTags`. |
| `-t`, `--tags` | Also fetch every tag; `-n`, `--no-tags` fetches none, not even the ones that follow. `remote.<name>.tagOpt` (`--tags`, `--no-tags`) is the default. |
| `-f`, `--force` | Update refs whatever the refspec's `+` says. |
| `-u`, `--update-head-ok` | Allow fetching into the branch checked out in any work tree; without it that is `fatal: refusing to fetch into branch 'refs/heads/<b>' checked out at '<path>'` (`128`). |
| `--dry-run` | Print the lines, change no ref, reflog or `FETCH_HEAD`. As in git the objects are still downloaded. |
| `-v`, `-q` | `-v` also prints `= [up to date]` lines; `-q` prints nothing (the status is the same). |
| `--progress`, `--no-progress` | The server's progress text, as `remote: <text>` lines. By default it is shown only when standard error is a terminal. |
| `-j`, `--jobs` | Accepted; remotes are fetched one after the other. `fetch.parallel` is ignored. |
| `--auto-gc`, `--write-commit-graph` and their `--no-` forms, `--no-recurse-submodules` | Accepted and ignored. |
| `--depth`, `--deepen`, `--shallow-since`, `--shallow-exclude`, `--unshallow`, `--update-shallow` | Usage error (`129`): shallow fetch is not built. |
| `--filter`, `--refetch` | Usage error (`129`): partial fetch is not built. |
| `--atomic`, `--set-upstream`, `--recurse-submodules` | Usage error (`129`): not built. Any other unknown flag is a usage error as everywhere. |

What a fetch does, in git's order:

1. Asks the remote for its refs and maps them through the refspecs: the configured ones, or the command line's. Fetching named
   refs also updates the configured tracking ref of each (`git fetch origin main` updates `origin/main`), printed but not
   recorded in `FETCH_HEAD`. Unless `--no-tags`, a tag the remote has and the repository lacks is fetched when the object it
   names, or the commit it points at, is here or arrives with the fetch (a second round fetches tags whose target arrived but
   whose own object did not).
2. Prunes, then downloads one pack (thin, offset deltas) and checks that everything the new tips reach is present before any ref moves.
3. Writes `FETCH_HEAD`, then moves each local ref with a compare-and-swap through its lock file, so nothing is half updated. A
   ref that cannot be written is reported `! ... (unable to update local ref)`, the rest still update, and the exit status is `1`.

Each update line is ` <code> <summary>  <remote ref> -> <local ref>` on standard error after one `From <url>` line (the URL
without user name, `.git` or trailing slashes), with git's columns. The summary column is `2 * abbrev + 3` wide and the ref column at
least 10. Ids are `core.abbrev` digits (default 7) and are not lengthened to stay unique.

| Line | Meaning |
| --- | --- |
| `* [new branch]`, `* [new tag]`, `* [new ref]` | A ref that did not exist (named by the remote ref: a branch fetched to `refs/tags/` is `[new branch]`). |
| `  abc1234..def5678` | Fast-forward. |
| `+ abc1234...def5678 ... (forced update)` | Not a fast-forward, allowed by `+` or `--force`. |
| `! [rejected] ... (non-fast-forward)` | Not a fast-forward and not forced. Exit `1`. |
| `! [rejected] ... (would clobber existing tag)` | A tag differs; `--force` gives `t [tag update]`. Exit `1`. |
| `- [deleted]         (none)     -> <ref>` | Pruned. |
| `* branch  main -> FETCH_HEAD` | A value that is only recorded in `FETCH_HEAD` (`tag`, `branch` or the ref's kind). |

`FETCH_HEAD` is in the per-work-tree git directory: `<id>`, a tab, `not-for-merge` or nothing, a tab, `branch '<name>' of <url>`
(`tag '<name>' of`, `remote-tracking branch`, or `<url>` alone for `HEAD`). Entries that are for merging come first: the ones
named on the command line, the first configured refspec when it is not a pattern and the branch has no `merge`, and the
`branch.<current>.merge` ref when the current branch's `branch.<name>.remote` is this remote. With only a URL, or a remote with no
`fetch` refspec, `HEAD` is fetched (`fatal: couldn't find remote ref HEAD` when the remote's `HEAD` is unborn). It is emptied at
the start of every fetch that is not `--append`, even when the fetch then fails.

Remote-tracking refs get a reflog by `core.logAllRefUpdates` (the default and `true` log `refs/remotes/*`; `always` also logs tags;
`false` logs nothing; an existing log is always appended to). The line is `fetch <arguments as typed>: <action>` with the action
`storing head`, `storing tag`, `storing ref`, `fast-forward`, `forced-update` or `updating tag` (`fetch --append --no-auto-gc
--no-write-commit-graph <name>` for each remote of `--all`). Updating the checked-out branch with `-u` logs `HEAD` too.

`remote.<name>.followRemoteHEAD`, as in git 2.48 and later: after a fetch of a named remote with its configured refspecs (no
refspec on the command line, not `--dry-run`), when the remote's `HEAD` is a symbolic ref to a branch that the refspecs map to an
existing `refs/remotes/<name>/<branch>`, `refs/remotes/<name>/HEAD` is made a symbolic ref to it. `create` (the default) only creates
it when missing; `warn` also creates it and prints a one-line warning, without changing it, when it points elsewhere;
`warn-if-not-<branch>` warns only when the remote's `HEAD` is not `<branch>`; `always` also moves it; `never` does nothing. A
detached or unborn remote `HEAD` changes nothing. The symbolic ref gets no reflog, and the warning wording is pyn's. `remote set-head`
sets the same ref by hand, see [Querying remotes on a git home](#querying-remotes-on-a-git-home).

Object format. The remote's `object-format` must be the repository's: otherwise `fatal: mismatched algorithms: client sha1;
server sha256` (`128`). A repository with no objects, refs or index takes the remote's format (it sets
`core.repositoryformatversion = 1` and `extensions.objectformat`), which git itself refuses to do outside `clone`.

Differences from git, all deliberate or not yet done: `--dry-run` downloads objects like git does, but git lists auto-followed tags twice on a dry run and pyn once;
ids are not lengthened to stay unique and do not scale with the repository's size; there is no per-object "Receiving objects" progress,
only what the server sends; local-path and `file://` remotes are refused (see [Transports and credentials](#transports-and-credentials));
`fetch.output`, `fetch.showForcedUpdates`, `fetch.recurseSubmodules`, `fetch.writeCommitGraph`, `remote.<name>.promisor`,
`--refmap`, `--negotiation-tip`, `--server-option`, `--stdin` and `-4`/`-6` are not read or taken; `--all` runs
the remotes one at a time whatever `-j` or `fetch.parallel` say, so `FETCH_HEAD` lists them in configuration order; the stale-ref hint after a ref could
not be written names `pyn remote prune`.

### Pull on a git home

`pyn pull [<options>] [<repository> [<refspec>...]]` is `pyn fetch <repository> <refspec>...` (same remote choice, same output,
`FETCH_HEAD` written) followed by `pyn merge` of the fetched merge candidates into the current branch, as `git pull` with the
merge mode. Git's own prose is not copied everywhere: where a message names a command it names `pyn`.

1. Rebase is not built, but it is not refused up front: `--rebase`, `-r`, `pull.rebase` or `branch.<name>.rebase` set to `true`,
   `merges` or `interactive` (and `--rebase=` those or `i`, `m`) is decided after the fetch. A candidate the branch already
   contains prints `Current branch <b> is up to date.` and exits `0`; a branch that can fast-forward (or is unborn) fast-forwards
   exactly as without rebase, with the same dirty-tree protection; a branch with local commits the candidate lacks needs a real
   rebase and is `fatal: rebase is not built; use --no-rebase` (`128`) with the fetched refs kept and nothing else changed:
   run `pyn pull --no-rebase`. `--no-rebase`, `--rebase=false`, `pull.rebase=false` and
   `branch.<name>.rebase=false` merge. With unmerged index entries it is `fatal: Exiting because of an unresolved conflict.`
   and with `MERGE_HEAD` present `fatal: You have not concluded your merge (MERGE_HEAD exists).` (both `128`).
2. The fetch. A failed fetch exits `1` (git's `pull` does, not `fetch`'s `128`); `--dry-run` fetches only and ends there.
3. The merge candidates are the `FETCH_HEAD` lines that are not `not-for-merge`: the branch named by `branch.<name>.merge` when
   no refspec is given, else what the command line fetched. None: git's explanation and exit `1` (`There is no tracking
   information for the current branch.`, `You are not currently on a branch.`, `You asked to pull from the remote '<r>', but did
   not specify a branch...`, `Your configuration specifies to merge with the ref '<ref>' from the remote, but no such ref was
   fetched.`, or the wildcard note). More than one candidate is an octopus merge, which is not built: `fatal: octopus merges
   are not built; name one branch to pull` (`128`, after the fetch).
4. An unborn branch is pointed at the candidate and its tree is checked out, silently, as git does; several candidates are
   `fatal: Cannot merge multiple branches into empty head.`
5. Otherwise the candidate (peeled to a commit) is merged as in [Merging](#merging): `Already up to date.`, fast-forward
   (`Updating <old>..<new>`, `Fast-forward`), or a merge commit titled `Merge branch '<name>' of <url>` (`tag`, `remote-tracking
   branch` and `commit` for other refs, plain `Merge <url>` for a bare `HEAD`, and ` into <branch>` unless on `main` or
   `master`). A conflict leaves `MERGE_HEAD`, `MERGE_MSG` and unmerged index entries and exits `1`. Local changes the merge would
   overwrite stop it with `error: Your local changes to the following files would be overwritten by merge` (exit `1` for a
   fast-forward, `2` for a merge commit, as git).
6. Divergent branches (neither side contains the other) with no `--ff`, `--no-ff`, `--ff-only`, `--no-rebase`, `pull.ff` or
   `pull.rebase` to say how to reconcile them are `fatal: Need to specify how to reconcile divergent branches.` (`128`, after the
   fetch), with a hint.

Integration config: `--ff`/`--no-ff`/`--ff-only` beat `pull.ff` (`true`, `false`, `only`), which beats `merge.ff`; `--ff-only`
that cannot fast-forward is `fatal: Not possible to fast-forward, aborting.` (`128`).

| Flag | Meaning |
| --- | --- |
| `-q`, `-v`, `--progress`, `--no-progress` | Forwarded to the fetch; `-q` also silences the merge's `Updating`, `Fast-forward`, `Auto-merging` and summary lines. |
| `-p`, `--prune`, `--no-prune`, `-t`, `--tags`, `--no-tags`, `-f`, `--force`, `-a`, `--append`, `--dry-run` | As for `pyn fetch`. |
| `--ff`, `--no-ff`, `--ff-only`, `--commit`, `--no-commit` | As for `pyn merge`; `--no-commit` stops before the commit with `MERGE_HEAD` set. |
| `--no-edit` | Accepted: no editor is ever started. |
| `--no-rebase`, `--rebase`, `-r` | `--no-rebase` merges; `--rebase` fast-forwards or stops as above. |
| `--edit`, `-e`, `--squash`, `-s`/`--strategy`, `-X`/`--strategy-option`, `--autostash`, `-S`/`--gpg-sign`, `--signoff`, `--cleanup`, `--stat`, `-n`/`--no-stat`, `--recurse-submodules`, `--allow-unrelated-histories`, `--all` | Usage error (`129`): not built (`--all`: use `pyn fetch --all`). |

Config read: `pull.ff`, `pull.rebase`, `branch.<name>.rebase`, `branch.<name>.merge`, `branch.<name>.remote`, `merge.ff`, plus what
`pyn fetch` reads. The summary after a merge is `pyn merge`'s file count, not git's diffstat, and the message of an annotated tag
is not appended to a tag merge's message. Pulling on a pyn server is #310 and still prints the git-command message.

### Clone from a git URL

`pyn clone [<options>] <url> [<dir>]` makes a new git repository from a git remote over the system `ssh` or smart HTTP, with no
`git` program (see [Transports and credentials](#transports-and-credentials)). It prints `Cloning into '<dir>'...` (or
`Cloning into bare repository '<dir>'...`) on standard error, the server's progress only with `--progress` or on a terminal, and
nothing but errors with `-q`. A failure is `fatal: ...` with exit `128`; an unsupported flag is a usage error (`129`).

**Which sources are git.** `ssh://`, `git+ssh://`, `ssh+git://`, scp-like `[user@]host:path`, and any other URL scheme or local path
(refused with the transport's `local-path remotes are not supported yet`) are always git. A bare `owner/name` is a pyn server
repository on the configured server, and `<server>/owner/name` over `http(s)` is a pyn server when the server says so. For an
`http(s)` address whose last two path segments could be `owner/name`, pyn asks the part in front of them for
`/openapi.json`, without credentials and within three seconds, and calls it a pyn server only when the answer is a JSON document
that lists the `/v1/registration` route. A 404, another status, another document, or a URL with fewer than three path segments
(`https://host/repo.git`, which has no `owner/name`) means a git host and is cloned as git with no probe. Nothing is printed about
the probe on success.

The probe can be inconclusive: a refused connection, a timeout, a TLS failure or a `5xx`. pyn then tries git, and when that fails
the error ends with a `hint:` line saying the address gave no answer as a pyn server and may be one that is down. A pyn
server that is down therefore shows up as a git failure, plus that hint. There is no flag to force either side; use `owner/name`
with `--server`, or an `ssh://` URL, to be exact.

**What it does.** The directory defaults as git names it: the last path part of the URL without a trailing slash, `/.git` or
`.git` (a scp-like `host:path` and a URL with only a host work too), plus `.git` for `--bare`. A target that exists and is not an
empty directory is `fatal: destination path '<dir>' already exists and is not an empty directory.` (`128`). pyn creates the
directory and `.git`, sets `remote.<name>.url` (as typed), the fetch refspec `+refs/heads/*:refs/remotes/<name>/*` and, for
`--no-tags`, `remote.<name>.tagOpt`, fetches the way [`pyn fetch`](#fetch-on-a-git-home) does (the repository is created
with the remote's object format, so a SHA-256 remote gives a SHA-256 clone), then:

- creates `refs/remotes/<name>/HEAD` when the remote's `HEAD` is a branch that was fetched, without regard to
  `remote.<name>.followRemoteHEAD`;
- picks the branch: `-b <name>`, else the branch the remote's `HEAD` names, else (a server that sends no symref) the branch at the
  commit `HEAD` names, preferring `init.defaultBranch`; with `-b <tag>`, or a `HEAD` that no branch is at, `HEAD` is detached at that
  commit (an annotated tag is peeled) and `Note: switching to` is printed unless `-q`; a `-b` that is neither a branch nor a tag is
  `fatal: Remote branch <name> not found in upstream <origin>`;
- creates `refs/heads/<branch>` at the remote's tip, sets `branch.<branch>.remote` and `.merge`, and points `HEAD` at it; `HEAD` and
  the branch get the reflog line `clone: from <url>` (without credentials), as does `refs/remotes/<name>/HEAD`; the other
  remote-tracking refs have no reflog and there is no `FETCH_HEAD`, as with git;
- checks out the work tree and writes the index (file modes, executable bits and symlinks as in the commit); `-n` leaves both empty
  and writes no index;
- creates no `.pyn/`: the clone is an ordinary git repository, and the first pyn command that needs pyn state creates it.

Tags: a full clone fetches every tag and stores no `tagOpt`; `--single-branch` follows only the tags that point into the history it
fetched; `--no-tags` fetches none and stores `remote.<name>.tagOpt = --no-tags`. `--tags` is the default of a full clone.
`--single-branch` narrows the fetch refspec to the cloned branch (or to `+refs/tags/<tag>:refs/tags/<tag>` for `-b <tag>`) and
`--no-single-branch` undoes it. `--bare` creates `<dir>` as the git directory with `core.bare = true`, copies the heads to
`refs/heads` and the tags to `refs/tags` (no remote-tracking refs, no fetch refspec, no branch tracking), and points `HEAD` at the
branch. `-o <name>` names the remote. `-c <key>=<value>` is written to the new repository's config before the fetch, so transport
settings such as `http.extraHeader` or `url.<base>.insteadOf` apply to it. `-v` and `-j` are accepted and change nothing.

**Empty and unusual remotes.** An empty remote gives `warning: You appear to have cloned an empty repository.` (also with `-q`), an
unborn `HEAD` on the branch the remote's `HEAD` names (or, when the server cannot say, `init.defaultBranch`, else `master`) with
`branch.<name>.*` set, and no checkout. A remote whose `HEAD` names a branch it does not have, while other
branches exist, keeps that unborn `HEAD`, fetches the rest and warns `remote HEAD refers to nonexistent ref, unable to checkout`.

**Failure.** If the clone fails before the fetch is complete (an unreachable host, a failed sign-in, a bad `-b`), what it created is
removed: the new directory, or only `.git` (or the contents of a bare target) when the directory already existed and was empty. A
failure while checking out leaves the repository in place, as git does.

Differences from git, all deliberate or not yet done: no hooks, templates or `description` file are created; there is no "Updating files" progress; `--mirror`, `--depth`,
`--shallow-since`, `--shallow-exclude`, `--shallow-submodules`, `--filter`, `--also-filter-submodules`, `--recurse-submodules`,
`--sparse`, `--reference`, `--reference-if-able`, `--dissociate`, `--separate-git-dir`, `--template`, `--local`, `--no-local`,
`--no-hardlinks`, `--shared`, `--upload-pack`, `--server-option`, `--bundle-uri` and `--ref-format` are usage errors.

### Push on a git home

`pyn push [<options>] [<repository> [<refspec>...]]` sends commits and refs to a git remote over the system `ssh` or smart HTTP,
with no `git` program (see [Transports and credentials](#transports-and-credentials)). It prints git's table on standard error
and exits `0` when every ref was pushed, `1` when a ref was rejected or failed, `128` on a fatal error and `129` on a usage
error.

**Which remote.** `<repository>` is a remote name or a URL. Without one: `branch.<name>.pushRemote`, then `remote.pushDefault`,
then `branch.<name>.remote`, else `origin`; with none configured it is `fatal: No configured push destination.` (`128`). The
URLs are the remote's `pushurl`, else its `url`; every one is pushed to in turn. `url.<base>.pushInsteadOf` and `insteadOf` are
applied. `remote.<name>.mirror` is a usage error.

**What is pushed.** Each `<refspec>` is `[+]<src>[:<dst>]`:

| Refspec | Meaning |
| --- | --- |
| `<name>` | The local ref to the ref of the same full name. |
| `<src>:<dst>` | `<src>` is matched like git does (`main`, `heads/main`, `refs/heads/main`, `HEAD`, a revision). A short `<dst>` is matched against the remote's refs; if it matches none, `refs/heads/` or `refs/tags/` is added according to what `<src>` is, otherwise it is `error: The destination you provided is not a full refname` and the push fails (`1`). More than one match is an error. |
| `+<src>:<dst>` | Allow a non-fast-forward update of that ref. |
| `:<dst>`, `--delete <name>` | Delete the remote ref. A name the remote lacks is `error: unable to delete '<name>': remote ref does not exist`. |
| `<src>*:<dst>*` | Every local ref that matches the pattern. |
| `tag <name>` | `refs/tags/<name>:refs/tags/<name>`. |
| `:` | Every local branch that the remote also has. |
| `HEAD` | The current branch; on a detached `HEAD` it is `fatal: HEAD cannot be resolved to branch` (`128`). |

With no refspec: `remote.<name>.push` if set, else `push.default`: `simple` (the default; the current branch to the upstream
branch of the same name, and `fatal` when the branch has no upstream or the upstream has another name, or when pushing to a remote
that is not the branch's own, in which case it acts as `current`), `current`, `upstream` (`tracking`), `matching` or `nothing`.
`push.followTags` is read. A detached `HEAD` with no refspec is `fatal: You are not currently on a branch.` The advice that
goes with these fatal errors names `pyn`. Refspecs that cannot be parsed are `fatal: invalid refspec` (`128`).

| Option | Does |
| --- | --- |
| `-f`, `--force` | Update refs whatever the refspec's `+` says. |
| `--force-with-lease[=<ref>[:<expect>]]`, `--no-force-with-lease` | Refuse (`[rejected] (stale info)`) unless the remote ref is where expected: the remote-tracking ref (none means the ref must not exist), or `<expect>`. With a ref it covers only that ref. A lease that holds forces the update. The options are not order-sensitive: `--no-force-with-lease` cancels all. |
| `-d`, `--delete` | Delete the named remote refs. |
| `--tags` | Push `refs/tags/*` (and only that when no other refspec is given). |
| `--follow-tags`, `--no-follow-tags` | Also push the annotated tags that point into the history being sent and are missing on the remote. |
| `--all`, `--branches` | Push every local branch. Not with refspecs or `--tags` (`fatal`). |
| `-u`, `--set-upstream` | For each branch pushed (or up to date) write `branch.<name>.remote` and `.merge`, and print `branch '<name>' set up to track '<remote>/<branch>'.` on standard output. `branch.<name>.pushRemote` is not touched. |
| `-n`, `--dry-run` | Contact the remote and print the table as if it succeeded; send nothing and change no ref, configuration or tracking ref. |
| `--atomic` | All refs or none: a ref that fails the checks here makes the others `[rejected] (atomic push failed)` and nothing is sent; a server without the capability is `fatal`. |
| `--porcelain` | `<flag> <tab> <from>:<to> <tab> <summary> (<reason>)` lines and a final `Done`, on standard output. |
| `-q`, `-v` | `-q` prints errors only; `-v` also prints `Pushing to <url>`, `= [up to date]` lines and the tracking refs it updates. |
| `--progress`, `--no-progress`, `--verify`, `--no-verify`, `--thin`, `--no-thin`, `-4`, `-6`, `--recurse-submodules=no` | Accepted and ignored. No hooks run; the server's own messages (`remote: ...`, including its hooks' output) are always shown. |
| `--mirror`, `--prune`, `--signed`, `-o`, `--push-option`, `--recurse-submodules` (other than `no`), `--receive-pack`, `--exec`, `--repo`, `--force-if-includes` | Usage error (`129`) naming the flag. `--force-if-includes` is refused because ignoring it would push less safely than asked. |

What a push does, in git's order:

1. Connects and reads the remote's advertised refs, then works out, for each ref, create, fast-forward, forced update, delete or
   nothing. An update is refused as `[rejected] (non-fast-forward)` when the old tip is not an ancestor of the new one,
   `(fetch first)` when the remote's old tip is not in this repository, `(already exists)` for a tag that exists with another
   value, `(needs force)` when either end is not a commit, and `(stale info)` for a failed lease. `+`, `--force` and a held lease
   turn these into `(forced update)`. A ref that is not named by a refspec is never touched; a refspec that matches nothing is
   `error: src refspec <name> does not match any` and fails the push (`1`) before anything is sent.
2. Sends one pack with the objects the new tips need and the remote lacks (none when only deleting), then the ref commands, and
   relays the server's messages as `remote: ...` lines. The server decides each ref: `[remote rejected] (<its reason>)`, for example a
   `pre-receive` hook, `receive.denyNonFastForwards` or deleting its current branch.
3. Prints `To <url>` (without user name or password) and then up to date lines (`-v`), the refs that were pushed, then the failures,
   each ` <flag> <summary> <from> -> <to> (<reason>)` in git's columns. The summary column is `2 * abbrev + 3` wide, the ids are
   `core.abbrev` digits (default 7). Then `-u`, then the tracking refs, then `error: failed to push some refs to '<url>'` and one
   `hint:` block (naming `pyn pull`) for the first kind of rejection, unless `advice.pushUpdateRejected` or the specific
   `advice.push...` setting is false; or `Everything up-to-date` when nothing needed sending.
4. For a push to a named remote it moves each remote-tracking ref the remote's fetch refspecs map the pushed ref to (and deletes
   it for a delete), for every ref that was pushed or already up to date, with the reflog line `update by push` (following
   `core.logAllRefUpdates`, as fetch does).

Differences from git, all deliberate or not yet done: no per-object "Enumerating objects" or "Writing objects" progress, only what the
server sends; ids are not lengthened to stay unique; `push.autoSetupRemote`, `push.negotiate`, `push.gpgSign`, `push.recurseSubmodules`,
`push.useForceIfIncludes` and `remote.<name>.receivepack` are not read; pre-push hooks are never run; `--dry-run` with `-u` prints `Would set upstream of ...`
instead of setting anything; local-path and `file://` remotes are refused (see [Transports and credentials](#transports-and-credentials)).
A push to a pyn server is not this command: see the push row above.

### Text from a remote

Text a server sends to be shown (`remote:` lines, error packets, the reason a ref update was refused, a pyn server's error
message) is printed without control characters other than newline, carriage return and tab. Paths a commit or switch would write
are checked first, as in [the work tree rules](workspace.md#paths-written-to-the-work-tree), and a refused path stops the command
with nothing changed.

## Transports and credentials

Decisions #245 and #255. pyn reaches a git remote without git: the protocol, the pack handling and the credential flow are
implemented in `pyn-gittransport`, `pyn-gitwire` and `pyn-git`. **git does not need to be installed.** pyn never starts a `git`
program; `make boundary-check` fails when non-test source under `crates/` spawns one. The programs pyn can start are the system
`ssh` client, and the credential helpers and askpass programs your own configuration names.

`pyn ls-remote`, `pyn fetch`, `pyn pull`, `pyn push`, `pyn remote add -f` and `pyn clone <git url>` use them; the rest of the remote commands arrive on top of the same
connections (epic #301). The library functions `fetch_objects` and `push_objects` (thin packs and offset deltas on fetch, a
streamed pack file on push) are what those commands call.

Tests use real git as the oracle server (tests only, never at runtime): `crates/pyn-gittransport/tests/common/gitsrv.rs` holds
the shared fixtures (bare repositories, the `GIT_SSH_COMMAND` shim, `git http-backend` behind a test HTTP server), and
`crates/pyn-cli/tests/git_remote_matrix.rs` runs `ls-remote`, `clone`, `fetch`, `pull` and `push` over ssh, HTTP, HTTP with
credentials, a local path and a `file://` URL, each for SHA-1 and SHA-256 repositories. The last two are not supported yet, so the matrix
asserts that refusal. The per-command suites (`git_clone.rs`, `git_fetch.rs`, `git_pull.rs`, `git_push.rs`) go deeper on one command.

### URLs

| Form | Transport |
| --- | --- |
| `https://host/path`, `http://host/path` | Smart HTTP |
| `ssh://[user@]host[:port]/path`, `git+ssh://...`, `ssh+git://...` | System `ssh` |
| `[user@]host:path` (scp-like; `[::1]:path` for IPv6) | System `ssh` |
| `/path`, `./path`, `file:///path` | Not supported: `local-path remotes are not supported yet` (serving a local repository needs git itself) |
| `git://host/path` | Not supported: unauthenticated and unencrypted |

`url.<base>.insteadOf` and `pushInsteadOf` (longest matching prefix wins; `pushInsteadOf` first when pushing) are applied when
connecting, so `remote -v` still shows what is configured. A password or token in a URL is never printed: messages show the URL
without its user information, and error text from the network layer is redacted the same way.

### ssh

pyn runs `ssh` and, on the remote, `git-upload-pack '<path>'` or `git-receive-pack '<path>'` (the remote needs git installed,
as it always does to serve git over ssh). The path is quoted for the remote shell. Your `ssh` does the rest: keys, agent,
`known_hosts`, `~/.ssh/config`, jump hosts and prompts, which appear on your terminal.

- The client is `GIT_SSH_COMMAND` (a shell command line), else `core.sshCommand`, else `GIT_SSH` (a program), else `ssh`.
  A missing client is reported with what to install or set.
- `GIT_SSH_VARIANT` or `ssh.variant` (`ssh`, `simple`, `plink`, `putty`, `tortoiseplink`) says how the client takes a port and
  options; otherwise the name of the program decides, and an unrecognized program is treated as OpenSSH. `ssh` gets `-p <port>`
  and `-o SendEnv=GIT_PROTOCOL`, `plink` and `putty` get `-P <port>`, `simple` gets neither and refuses a port. A host or user
  that starts with `-` is refused.
- Protocol version 2 is requested through `GIT_PROTOCOL=version=2`. A server that does not accept it answers in version 0 and
  pyn carries on; pushes use version 0.
- Exit status `255` is reported as a connection or sign-in failure, `127` as a missing `git-upload-pack` on the remote, and
  any other status as the remote command failing; ssh's own messages come first on standard error.
- With the default `ssh` client (or `GIT_SSH` with an OpenSSH-style client) pyn adds `-o ConnectTimeout=30`,
  `-o ServerAliveInterval=30` and `-o ServerAliveCountMax=4`, so a connect or a silent peer ends after about two minutes. These
  options win over `~/.ssh/config`; set `GIT_SSH_COMMAND` or `core.sshCommand` to manage them yourself.

### Smart HTTP

`GET <url>/info/refs?service=git-upload-pack` (`git-receive-pack` to push) and then one `POST` per request, with git's content
types. The `# service=` line is checked. Dumb HTTP (an `info/refs` that is not a smart advertisement) is not supported and says so.

- Redirects: only the first request follows them (`http.followRedirects`: `initial`, the default, `true` or `false`), the
  repository URL is rebased onto the final location, and a redirect from https to http is refused. Credentials are sent only to
  the host (scheme, name and port) they were found for, never to a host a redirect leads to; `http.extraHeader` is likewise
  sent to the configured host only.
- Request bodies up to 1 MiB are sent whole (fetch requests above 1 KiB are gzipped, as git does); a larger push streams as a
  chunked upload and cannot be retried after a `401`, so the credentials must already be accepted (the `info/refs` request of a push
  normally settles that).
- Settings read: `http.extraHeader`, `http.proxy` (and the usual proxy environment variables; an empty value turns proxies off),
  `http.sslVerify` and `GIT_SSL_NO_VERIFY`, `http.sslCAInfo` and `GIT_SSL_CAINFO`, `http.followRedirects`, each also scoped as
  `http.<url>.<key>`, and `http.lowSpeedTime` (see below). Not read: client certificates, `http.cookieFile`, `http.lowSpeedLimit`,
  `http.version`.
- Timeouts: connecting waits 30 seconds. There is no total time limit, so large transfers keep working, but a request or a body
  read that receives nothing for 300 seconds fails. `http.lowSpeedTime` sets that wait in seconds (`0` disables it).
- Only HTTP Basic authentication is supported; a server that asks for Digest, Negotiate or NTLM gets a message naming it.
- `User-Agent` is `git/2.0 (pyn/<version>)`. The `git/` prefix is what hosts that gate smart HTTP on a git client look for; GitHub
  accepts any agent, others differ.

### Credentials

The flow is git's, run in process, for HTTP remotes (ssh authenticates itself). On a `401` pyn takes the first source that has
something new, and tries up to three prompts:

1. Credentials in the URL: `https://user:password@host/...`, including `https://x-access-token:<token>@host/...`.
2. `credential.helper` entries, repository config over global, with `credential.<url>.helper` sections that match the remote
   (an empty value forgets the helpers before it), asked with `get`. `credential.username` and `credential.useHttpPath` are honoured.
3. A URL with only a user name, `https://<token>@host/...`, is sent as that user with an empty password, which is how hosts
   such as GitHub take a token.
4. An askpass program (`GIT_ASKPASS`, `core.askPass`, `SSH_ASKPASS`, in that order), else a prompt on the terminal. Terminal
   prompts need a terminal on standard input and are off when `GIT_TERMINAL_PROMPT=0`; with neither, pyn fails with
   `terminal prompts disabled` rather than waiting.

A credential that the server accepts is passed to every helper with `store` (except one that came from the URL, which pyn never
copies into a helper); one that it refuses is passed with `erase`. A password is never logged, printed or sent to another host.

Every credential field (protocol, host, path, user name, password and any other attribute) must be free of newline, carriage
return and NUL characters, whether it comes from the URL (after percent-decoding), configuration, an askpass program, a prompt
or a helper's output. A value that has one is refused with `credential field '<name>' contains a control character` before it is
sent to a helper or used.

| Helper | Behaviour |
| --- | --- |
| `store [--file <path>]` | In process. Reads `~/.git-credentials` then `$XDG_CONFIG_HOME/git/credentials` (or only `<path>`), one URL per line with percent-encoded user and password, as git's `store` does; writes the first file that exists (else `~/.git-credentials`) with mode `0600`. |
| `cache` | There is no daemon in process; `git-credential-cache` is used through the helper protocol when it is on `PATH` and skipped otherwise. |
| `!<command>` | Run through `sh` with `get`, `store` or `erase` appended. |
| `/absolute/path [args]` | Run through `sh` the same way. |
| any other name, such as `osxkeychain` or `manager` | The program `git-credential-<name>` through the helper protocol (`key=value` lines ended by a blank line on standard input; `get` replies on standard output), when it is on `PATH`. A helper that is not installed is skipped silently; one that fails is ignored, as git does. |

Differences from git: no `credential.<url>.*` keys other than `helper`, `username` and `useHttpPath`; no `credential.interactive`,
`credentialCache`, partial `capability` negotiation or `authtype`; no `include` directives in config files.

### Limits

Input from a remote is bounded so that a fetch cannot use unbounded memory, disk or time. Hitting a bound is a clear error
and nothing is stored.

| What | Default | Config key (`pyn.` section, sizes accept `k`, `m`, `g`) |
| --- | --- | --- |
| Objects in a pack | 20,000,000 | `ingestMaxObjects` |
| Size of one object or delta result | 256 MiB | `ingestMaxObjectSize` |
| Delta chain depth (ingest and reading stored packs) | 4096 | `ingestMaxDeltaDepth` |
| Size of a received pack | 64 GiB | `ingestMaxPackSize` |
| Inflated bytes in one fetch, including resolved deltas | 128 GiB | `ingestMaxTotalInflated` |
| A delta result against its base | 256 times the base, unless under the floor | `ingestMaxDeltaRatio` |
| Floor for the ratio check | 4 MiB | `ingestDeltaSizeFloor` |
| Lines in one ref advertisement, listing or push report | 1,000,000 | not configurable |
| Bytes buffered for one of those | 256 MiB | not configurable |
| Directory nesting walked in a tree | 4096 levels | not configurable |

A `*` pattern in an ignore file or pathspec nests at most 1024 groups; a deeper pattern matches nothing.

## pyn only

No git equivalent; these keep their own names and are never aliased to a git command.

| Command | What it does |
| --- | --- |
| `pyn lock <path> [--base <rev>]` | Take the leased lock on an exclusive file. |
| `pyn unlock <path> [--force --reason <text>]` | Give a lock up without committing, or remove another user's (needs `force_unlock`). |
| `pyn locks [--mine]` | List live locks in the repository, or yours everywhere. |
| `pyn policy preview` | Show which paths would change mode, which locks would be released and which branch versions would be stranded, if a policy file were committed. |
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
| `git reset`, `git clean` | `pyn reset` exists for unpublished local commits only ([workspace](workspace.md#the-local-store)); otherwise there is no local history to reset: `pyn restore --staged`, `pyn restore` and `pyn update` cover the daily cases. `git clean` is not planned. |
| `git rm`, `git mv` | Not a CLI verb yet; no issue schedules removing or renaming paths from the CLI. |
| `git rebase`, `git cherry-pick`, `git revert` | History is append-only on the server. |
| `git blame`, `git grep` | Not scheduled. |
| Commands that maintain git's own storage or run its hooks (`gc`, `fsck`, `bundle`, `submodule`, `worktree`, hooks in `.git/hooks`) | pyn reads a git repository's `.git/` on a git home (decision #245) but does not maintain it or run its hooks. pyn's own hooks are tracked in #216. |

## On a git home

Decision #245: a repository has one home, a pyn server or git. In a git repository (a `.git` is found) pyn behaves as git,
through its own implementation of git's object store, refs and index, with no git library or binary (decision #30). A
`.pyn/pyn.toml` there is ignored: no path is exclusive, and the commands that need a pyn server refuse and name it. Going
from a git home to a pyn server is one explicit step, `pyn convert` (issue #95), which does not exist yet.

**Nothing git sees changes except what you asked for.** Hooks, notes, configuration and line endings are never touched, and
the work tree changes only where a command you ran says it does (`switch`, `checkout`, `restore`, `merge`, `stash`), never
over local changes you have not saved unless you forced it. Reading commands change nothing in `.git/`. `pyn init` writes
`info/exclude` once. Each writing command writes what git writes for the same command (objects, refs, reflog lines, the index,
`ORIG_HEAD` and the `MERGE_*` files), and the sections below list exactly what. A test asserts that the `.git` directory and
the work tree are byte-identical before and after every reading command and after every refusal, and the writing commands
are compared with what git left on disk for the same commands.

### Which home

pyn looks upward from the working directory, folder by folder, and the first (nearest) folder that makes a claim decides:

1. a `repo_tool` marker in `.pyn/local_only/repo.toml` ([the marker](workspace.md#the-repotool-marker)) wins over everything
   else in its folder: `pyn` makes it a pyn home even when a `.git` is there, and `git` makes it a git home even when it also
   looks like a pyn server workspace. A `git` marker needs a `.git` in the same folder; without one it decides nothing and the
   search goes on;
2. without a marker, a `.git` (a folder, or a `gitdir:` file as in a linked worktree or a submodule) makes it a git home, and
   a pyn server workspace (a `.pyn` folder that `pyn clone` filled in, holding `local_only/state/files.toml`) makes it a pyn
   home. Where both sit in one folder, the pyn server workspace wins.

So a clone made inside a git repository is a pyn workspace, and a git repository inside a pyn workspace is a git home. A nearer
`.git` beats a marker in a parent folder, and a nearer marker beats a `.git` above it. Any other `.pyn` folder, such as one
`pyn init` created before the marker existed or a committed `.pyn/pyn.toml`, leaves the home git. A marker that is not `git`
or `pyn` is an error naming the file.
`GIT_DIR` and `GIT_WORK_TREE` are not read.

`pyn init` in a git repository adds `.pyn/` to `.git/info/exclude` (the shared one, found through the `.git` file in a
worktree or submodule) unless a line there already hides it, creates `.pyn/local_only/`, where pyn keeps its own state
for a git home, and records `repo_tool = "git"` there. It is safe to run again.

### Ignored files

`status` honors git's ignore rules in full: every `.gitignore` from the work tree root down (a deeper file wins),
`.git/info/exclude`, and the global excludes file (`core.excludesFile`, else `$XDG_CONFIG_HOME/git/ignore`, else
`~/.config/git/ignore`). Patterns follow git's own matching: negation, folder-only patterns, `**`, character classes,
escapes and `core.ignoreCase`. A file inside an ignored folder cannot be re-included, and a tracked file stays tracked
whatever the rules say.

### Commands

| Command | On a git home |
| --- | --- |
| `status` | Also `-s`, `--porcelain` and `-b` in git's short format, and pathspecs (see [the short format](#the-short-status-format)). Header (`On branch main`, or `HEAD detached at <id>`), a `STAGED`, `UNSTAGED`, `PATH` table, and a count line; `nothing to commit, working tree clean` when clean. States are `added`, `modified`, `deleted`, `typechange`, `untracked` and `unmerged`. Staged is `HEAD` against the index, unstaged is the index against the work tree, so a path can be in both. A folder with nothing tracked in it is listed once, as `dir/`. Paths are from the repository root. Mode changes count as modified; symlinks are compared by their target. |
| `diff` | Index against the work tree; one commit against the work tree (the paths in the commit or the index); two commits, or `a..b`. A name that is both a revision and a file needs `--`. Shows `old mode` and `new mode`, symlinks and binary files as git does. `--cached` and `--staged` compare the index with `HEAD` (or with one named commit). |
| `show [<rev>]` | Commit header and message, then the diff against the first parent. A merge commit prints no diff (git prints a combined diff, which pyn does not compute). |
| `log [<options>] [<rev>...] [--] [<path>...]` | Git's output: commit blocks, newest first across all parents, with git's history simplification for a path; revision ranges, formats, filters and diff output are in [Log on a git home](#log-on-a-git-home). `--commits` is the pyn table. `--filter` fails. |
| `files`, `ls` | `files` lists the index and `ls` lists `HEAD`'s tree. Every entry is `shared`, with no revision, lock or last change. |
| `add`, `restore --staged`, `commit` | See [staging](#staging-and-committing). |
| `branch`, `switch`, `checkout`, `restore` | See [Branches, switching and the work tree](#branches-switching-and-the-work-tree). |
| `merge` | See [Merging](#merging). |
| `tag` | See [Tags](#tags). |
| `stash` | See [Stashing](#stashing). |
| `init` | See above. |
| `login`, `logout`, `register`, `password`, `key`, `token`, `user`, `org`, `team`, `repo`, `usage`, `setup`, `admin`, `whoami`, `member`, `role`, `invite` | Run as anywhere else; see [Refused commands](#refused-commands). |
| `lock`, `unlock`, `locks`, `policy`, `audit`, `verify`, `summary` | Refuse; see [Refused commands](#refused-commands). |
| `config`, `clone` | Unchanged: they do not depend on the repository's home. A clone from a pyn server made inside a git repository is its own pyn workspace; a clone from a git URL is its own git repository. |

Revisions are `HEAD`, `@`, a branch, a tag (annotated tags are followed to their commit), a full or abbreviated id of 4 or
more digits, and any of `~N`, `^` and `^N` after one. SHA-1 and SHA-256 repositories are both read. Dates print in the
offset recorded in the commit, as git does.

Differences from git, all deliberate or not yet done:

- Files are read as they are on disk: `core.autocrlf`, `.gitattributes` filters and `core.eol` are not applied, so a
  repository that depends on them can show files as modified that git does not.
- No rename or copy detection: a rename is a delete and an add.
- Submodule entries are not compared, and a submodule folder is not descended into.
- `log` and `show` print no ref decorations (`%d` in a `log` template does); commit abbreviations are 7 digits and are not
  lengthened to stay unique.
- A nested repository that the index does not track is one untracked `dir/`.
- `merge` and `stash apply` detect no renames, write diff3 conflict markers whatever `merge.conflictStyle` says, merge only when
  there is one merge base, and do not write `AUTO_MERGE`; `switch` and `checkout` do not warn about commits left behind by
  leaving a detached `HEAD`, and print their own wording where git's differs (`Fast-forward` without a diffstat, the status
  after `stash pop` in pyn's table).

### Log on a git home

`pyn log [<options>] [<rev>...] [--] [<path>...]` prints what `git log` prints. Arguments up to the first one that is not a
revision are revisions, the rest are paths; `--` ends the revisions explicitly. A name that is both a revision and a file needs
`--` (exit `128`, as git); a name that is neither fails with `ambiguous argument`. With no revision it starts at `HEAD`, and
`^<rev>` alone prints nothing, as git. Paths are taken relative to where you stand and limit the history with git's default
simplification.

| Revision | Meaning |
| --- | --- |
| `<rev>` | Everything reachable from it (`HEAD`, `@`, a branch, a tag, an abbreviated id, with `~N`, `^`, `^N`). |
| `^<rev>` | Leaves out what is reachable from it. |
| `<a>..<b>`, `..<b>`, `<a>..` | Reachable from `<b>` and not from `<a>`; a missing side is `HEAD`. |
| `<a>...<b>` | Reachable from either, but not from both. |

Formats: `--oneline` (abbreviated id and subject), `--format=<f>` and `--pretty[=<f>]` with `oneline`, `short`, `medium`
(the default), `full`, `fuller`, `raw`, `format:<template>` (a newline between commits) and `tformat:<template>` (a newline
after each; also a bare template or `--format=<template>`). Templates take `%H %h %T %t %P %p %an %ae %ad %aD %ai %aI %as %at
%ar %cn %ce %cd %cD %ci %cI %cs %ct %cr %s %b %B %d %D %n %%`; any other `%` sequence is printed as written. `%h`, `%t` and `%p`
are 7 digits. `%d` and `%D` list `HEAD -> <branch>` (or `HEAD`), then the other branches, remote-tracking branches, `tag: <name>`
and `refs/stash` that point at the commit, in git's order. `--date=` takes `default`, `iso`, `iso-strict`, `rfc`, `short`,
`relative`, `unix` and `raw` and applies to `Date:` and `%ad`/`%cd`; commit dates print in the offset recorded in the commit.

Filters apply in this order: `--author` and `--committer` (regular expressions on `Name <email>`, any of several of the same
kind), `--grep` (on the whole message; any of several, or all with `--all-match`, or none with `--invert-grep`), `-i` for all
three, `--since`/`--after` and `--until`/`--before` (on the committer date), `--merges` and `--no-merges`, then `--skip`, `-n`,
and `--reverse` last (it reverses the commits that were selected). The different kinds must all match. Regular expressions are
POSIX basic ones with GNU's `\|`, `\+` and `\?`, matched line by line; back-references are not supported. A date is a Unix
time (`1700000000` or `@1700000000`), `YYYY-MM-DD` with an optional `HH:MM[:SS]` and zone, an RFC 2822 date, `now`,
`yesterday`, or `<n> <unit> ago` (units from seconds to years, combinable). As in git a bare date takes the current time of day,
and a date without a zone is in the local timezone.

Output after each commit: `--stat`, `--shortstat`, `--name-only`, `--name-status` and `-p`/`--patch`/`-u`, with `-U<n>` (which
implies `-p`) for the context. `--stat` and `--shortstat` combine with `-p` (git prints `---` between the message and the stat
when `--stat` and `-p` are both given); the other combinations are a usage error. The diff is against the first parent, or
against the empty tree for a root commit, limited to the paths. A merge commit prints none (git needs `-m`, `-c` or `--cc` for
that). The engine is the one `pyn diff` uses, including its `--stat` layout.

Differences from git:

- With both a revision range that excludes history and a path, the choice of merge commits whose parents are on the excluded
  side can differ from git's, which decides it while the walk is still marking commits as excluded. Ranges alone, and paths
  alone, match.
- `--since` and `--until` only filter; git also stops walking at the first commit older than `--since`, which differs only for
  histories whose commit dates are not in order. A date git cannot read is a failure here (git takes `now`).
- `%d` does not hide `refs/notes/*` or other namespaces: they print by full name. Unlisted placeholders (`%C`, `%w`, `%<`, `%aN`,
  `%x`) are printed as written.
- `--format` and `--pretty` win over `--oneline` whatever their order. `--date=local`, `human` and `format:` are not taken.
- The `--stat` graph is at most 40 columns wide and names are never shortened; it has no `--stat=<width>` form.
- Passing a revision that is not a commit, a reflog selector (`HEAD@{1}`) or `:/<text>` fails with exit `128` where git
  accepts them.

### The short status format

`-s`/`--short` prints one `XY <path>` line per path, `--porcelain` (version 1) the same lines with paths always from the
repository root, and `-b`/`--branch` leads with `## <branch>...<upstream> [ahead N, behind M]`. Without `-b` there is no branch
line; with `-b` alone (no `-s` or `--porcelain`) the table is printed as before. A clean tree prints nothing.

On a git home `X` is `HEAD` against the index and `Y` the index against the work tree: `A`, `M`, `D`, `T` (type change),
space for no change, ` A` for an intent-to-add path, `??` for untracked, and for an unmerged path the pair git prints
from the stages the index holds (`UU`, `AA`, `DU`, `UD`, `UA`, `AU`, `DD`). A path staged for deletion that is back in the
work tree is two lines, `D ` and `??`. Changes come in path order, untracked paths last. `-s` shows paths relative to the
folder you stand in (`../a`, `./` for it), quoted as git does for a space, a control character, a quote, a backslash or a
non-ASCII byte (`core.quotePath`).

The branch line is `No commits yet on <branch>` before the first commit and `HEAD (no branch)` when detached. The upstream
comes from `branch.<name>.remote` and `branch.<name>.merge` mapped through the remote's fetch refspecs; `[gone]` marks a
tracking ref that does not exist. Counts are commits reachable from one side and not the other.

Differences from git: an untracked folder is listed as `dir/` even when a pathspec names a path inside it (git lists the
files under the path), and pathspecs are never globs.

On a pyn server `STAGED` becomes `X` (`added` is `A`, `modified` `M`, `deleted` `D`) and the work tree state becomes `Y`
(`modified` is `M`, `deleted locally` `D`, `untracked` is `??`). A conflicted path is `UU`, or `AA` when both sides added it
(the server does not say which side deleted a path, so `DU` and `UD` are not used). States that are about the server and
not the work tree (`behind`, `new`, locks, local commits not yet published, modes) have no place in the short format and
print nothing. `-b` prints `## <branch>...origin/<branch>` with `[ahead N]` for local commits not yet published, and just
`## <branch>` for a workspace without a local store. Pathspecs follow the same rules as `add`.

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
path is staged as the file is, which resolves it.

**`commit`.** A linear commit on the current branch:

1. It refuses, changing nothing, a detached `HEAD`, a merge, cherry-pick, revert, rebase or sequencer in progress,
   unmerged index entries, and `commit.gpgsign=true` (pyn cannot sign yet).
2. The author and committer are `GIT_AUTHOR_*` and `GIT_COMMITTER_*` (name, email, date) when set, else `author.*` and
   `committer.*`, else `user.name` and `user.email`; without a name and email it stops. The time zone is the system's.
3. With `-a`, every changed tracked file is staged first, in memory; the index is written only once the commit is built.
4. The index becomes trees (written as loose objects), and the commit has `HEAD` as its only parent. A tree equal to the parent's
   is `nothing to commit` (exit `1`). The message gets git's whitespace cleanup: no trailing blanks, no leading or trailing blank
   lines, a final newline.
5. The index is written (only with `-a`), then the branch ref is moved through its `.lock` file after checking `HEAD` did not move,
   and the reflog lines are appended (see [Reflogs](#reflogs)).

No hook runs (a warning names hooks that exist), and no `COMMIT_EDITMSG` is written. A commit made this way has the object id
git would give the same tree, parent, identity, dates and message. `commit <path>` is refused; `add` the paths and run
`commit`.

#### Commit flags on a git home

`-m` and `-F` are exclusive, as in git (exit `128`).

- `-F <file>` and `--file=<file>` read the message from a file, `-` from standard input, with the same whitespace cleanup as `-m`.
  A message that is empty after cleanup exits `1` with `Aborting commit due to empty commit message.`; an unreadable file exits `128`.
- `--allow-empty` commits a tree equal to the parent's. Without it that is `nothing to commit` (exit `1`).
- `--author=<Name <email>>` replaces the author name and email, `--date=<date>` the author date; the committer is unchanged.
  Dates are `<seconds> [<+hhmm>]`, `@<seconds>`, ISO 8601, RFC 2822, `YYYY-MM-DD HH:MM:SS` (local zone) and a bare day
  (`YYYY-MM-DD` or `YYYY/MM/DD`, taking the current time of day, as git). Relative words (`yesterday`) and month names are
  `invalid date format` (exit `128`). A bare `--author` pattern is not looked up among existing authors: it fails with exit `128`.
- `-s` and `--signoff` append `Signed-off-by: <committer name> <email>` after git's trailer rules: no blank line when the last
  paragraph is a trailer block, and nothing when the last line already is that line.
- `--amend` replaces the tip with a commit that has the tip's parents, author (name, email and date) and message, and the
  index as its tree; `-m` or `-F` replace the message, `--author` and `--date` replace the author fields, and `--no-edit` is
  accepted (pyn never opens an editor). `GIT_AUTHOR_*` is ignored, as in git. The committer and its date are new, a `gpgsig`
  header is dropped and other headers are kept. It exits `128` with no commit to amend or during a merge; an amend that
  would leave the commit equal to its parent's tree exits `1` unless `--allow-empty` is given; amending a merge commit is allowed.
  The branch and `HEAD` logs get `commit (amend): <subject>`, and `ORIG_HEAD` is not written.
- The output is `[<branch> <id>] <subject>`, an ` Author:` line when the author differs from the committer, and pyn's
  changed-file count; it has no `Date:` line.

A pyn server refuses all of these with a usage error (`129`), because its commits are immutable and `--file` there is the
content file.

**Finishing a merge.** While `MERGE_HEAD` exists `commit` makes a merge commit: the merged-in commits are further parents, and
without `-m` the message is `MERGE_MSG` with its `#` comment lines removed (what git's editor flow leaves; `git commit
--no-edit` keeps them). An empty merge is allowed, and unmerged entries are refused. `commit -a` stages a resolved file first.
`add` of an unmerged path stages the file as it is (or its removal) and clears the conflict stages. `commit` removes `MERGE_HEAD`,
`MERGE_MSG` and `MERGE_MODE`. A cherry-pick, revert or rebase in progress still stops it.

**`restore --staged`.** Replaces the index entries under the paths with `HEAD`'s (or `--source`'s), removing entries `HEAD`
lacks. The replaced entries carry no stat data, so git re-reads those files on its next status.

**`diff --cached`.** `HEAD` (or the named commit) against the index, in the same output as `diff`.

### Branches, switching and the work tree

`branch`, `switch`, `checkout` and `restore` write refs, the index and the work tree as git does, through the same lock files and
with the same reflog lines.

**`branch`.** Lists (`*` marks the current branch, `+` a branch checked out in another work tree, `(HEAD detached at <id>)` first),
`-v` adds the abbreviated commit and subject, `--list <pattern>` filters with shell patterns, `--show-current` prints the name.
`branch <name> [<start>]` creates a branch at `HEAD` or the start point (any revision) with the reflog line `branch: Created from
<start>`, and refuses an existing name or one that clashes with a folder of refs. `-d` deletes a branch merged into `HEAD`, `-D` any
branch; both refuse a branch checked out in a work tree, and delete its reflog and its `[branch "<name>"]` configuration section
(the section is rewritten as plain text, nothing else in `config` changes). `-m [<old>] <new>` renames the ref and its reflog,
moves the configuration section and, for the current branch, points `HEAD` at the new name. `-M`, `-c`, `-f`, `-a`, `-r`, `-u` and
`--contains` are not built (usage errors).

**`switch` and `checkout`.** Move `HEAD`, the index and the work tree from `HEAD`'s tree to the target's with git's two-way merge rules:

- A file that is the same in both trees keeps whatever local change it has; a file that differs is written only if the work tree
  file and the index entry still match `HEAD`. Otherwise nothing is changed and the command prints `error: Your local changes to
  the following files would be overwritten by checkout:` with the paths, and exits `1`.
- A file the target adds, where an untracked file exists, stops the command the same way (`The following untracked working tree
  files would be overwritten`). An untracked file that an ignore rule hides is overwritten, as in git. A folder holding untracked
  files is never replaced by a file, even with `-f`.
- `-f` makes the index and the work tree the target, local changes included; untracked files elsewhere stay.
- Files are written with the executable bit of the tree entry; symlinks become links (files holding the target when
  `core.symlinks=false`); `core.filemode=false` is honored when deciding what is a local change. Folders emptied by a removal are
  removed.
- Unmerged index entries, a merge, cherry-pick, revert or rebase in progress, and a sparse checkout refuse the command.
- `switch -c <new> [<start>]` and `checkout -b` create the branch first (an unborn branch just points `HEAD` at the name),
  `switch -` and `checkout -` return to the previous branch (read from the `HEAD` reflog), `--detach` and `checkout <commit>` (or a
  tag) detach `HEAD` and print git's advice about it (`advice.detachedHead=false` turns it off). `switch <commit>` is refused
  without `--detach`. A branch checked out in another work tree is refused.
- After a switch that carried local changes the paths are listed as `M\t<path>`, `A\t<path>` and so on, as git does, and
  `Switched to branch '<name>'`, `Switched to a new branch '<name>'` or `Already on '<name>'` goes to standard error.
- The `HEAD` reflog gets `checkout: moving from <old> to <new>`; a created branch gets `branch: Created from <start>`.

`checkout [<tree-ish>] -- <path>...` takes the paths from the index or the commit into the work tree (and, from a commit, into the
index); `checkout <path>` without `--` works when nothing else has that name; `--ours` and `--theirs` write a side of an unmerged
path and leave it unmerged. A path no tree or index entry has is `error: pathspec '<path>' did not match any file(s) known to
git` (`1`); an unmerged path is `error: path '<path>' is unmerged`.

**`restore`.** `restore <path>...` writes the index entries into the work tree; `--source <rev>` writes that commit's files instead
and, as in git, removes tracked files the commit lacks; `--staged --worktree` does both for index and work tree, from `HEAD` unless
`--source` is given; `--staged` alone is described under [staging](#staging-and-committing). The paths are prefixes (no globs).

### Merging

`merge <branch|tag|commit>` follows git's `ort` results for what pyn handles, with these steps:

1. It refuses a merge, rebase, cherry-pick or revert already in progress, unmerged entries, and an unknown name (`1`). `ORIG_HEAD`
   is written, as git does, once the target is known.
2. If the target is an ancestor of `HEAD` it prints `Already up to date.`. If `HEAD` is an ancestor it fast-forwards, moving the
   index and work tree with the rules above and logging `merge <name>: Fast-forward`; `--no-ff` makes a merge commit instead and
   `--ff-only` refuses when it cannot (`Not possible to fast-forward, aborting.`, `128`).
3. Otherwise there must be exactly one merge base. No base is `refusing to merge unrelated histories`; several (a criss-cross)
   is refused, because pyn does not build a virtual base yet, and nothing is changed.
4. The trees are merged path by path: a side that changed alone wins, a path both changed is merged line by line with the
   three-way merge of `pyn-merge`, a file mode changed on one side is taken, and a binary or symlink changed on both is a
   conflict. Renames are not detected, so a rename on one side and an edit on the other is a modify/delete conflict where git
   would merge cleanly; a path that is a file on one side and a folder on the other is refused before anything changes.
5. As in git's `ort` the index must equal `HEAD` and the files the merge changes must be clean (`error: Your local changes to the
   following files would be overwritten by merge:`, exit `2`).
6. A clean result becomes a commit with the two parents, `Merge branch '<name>'` (`tag`, `commit` and `remote-tracking branch` as
   git words them; ` into <branch>` unless the branch is `main` or `master`; an annotated tag's message follows) or `-m`, and the
   reflog line `merge <name>: Merge made by the 'ort' strategy.`. `--no-commit` stops before the commit with `MERGE_HEAD`,
   `MERGE_MSG` and `MERGE_MODE` written.
7. A conflict leaves the file with **diff3** markers (`<<<<<<< HEAD`, `||||||| <base abbreviation>`, `=======`,
   `>>>>>>> <name>`; git's default is the two-sided style unless `merge.conflictStyle` says diff3, and pyn always writes diff3),
   the unmerged index entries at stages 1, 2 and 3 (only the sides that exist), and `MERGE_HEAD`, `MERGE_MSG` (with git's
   `# Conflicts:` list) and `MERGE_MODE` (`no-ff` or empty). It prints `Auto-merging <path>`, `CONFLICT (content|add/add|modify/delete):
   ...` and `Automatic merge failed; fix conflicts and then commit the result.`, and exits `1`. git sees an ordinary conflicted merge:
   `git status`, `git add`, `git commit` and `git merge --abort` work on it, and `pyn add`, `pyn commit` and `pyn merge --abort`
   work on one git started.

`merge --abort` is `reset --merge`: it puts back the index and the files the merge changed (and any path you staged since) and
keeps unrelated local changes, but stops (`not uptodate`) if a path you staged has been edited again. `merge --continue` is
`commit`. `merge --preview` needs a pyn server. `--squash` and strategies are not built. `AUTO_MERGE` is not written.

### Tags

`tag` lists `refs/tags` sorted (`-l <pattern>` filters). `tag <name> [<object>]` writes a lightweight tag at `HEAD` or the object
(a tag name tags the tag object itself); `-a`, or just `-m`, writes an annotated tag: a tag object with the tagger from
`GIT_COMMITTER_*` or `user.name` and `user.email` (an error without them), the object's type, and the cleaned message, byte for
byte as git writes it. There is no editor, so `-a` needs `-m`. `-f` replaces a tag (`Updated tag '<name>' (was <id>)`), `-d` deletes
(`Deleted tag '<name>' (was <id>)`). Signing (`tag.gpgsign`) is refused. Tags get no reflog unless `core.logAllRefUpdates=always`.

### Stashing

A stash is the commit git makes: the work tree's tracked files as the tree, with `HEAD`, a commit of the index, and with `-u` a
parentless commit of the untracked (not ignored) files as its parents, on `refs/stash` with a reflog line `WIP on <branch>:
<abbreviation> <subject>` (`On <branch>: <message>` with `-m`). The reflog is the list, so pyn writes `logs/refs/stash` (always, as
git does) and keeps its chain consistent when an entry is dropped. After saving, the index and tracked files are reset to `HEAD`
(`ORIG_HEAD` and a `reset: moving to HEAD` line follow, as git's), and with `-u` the untracked files are removed; ignored files
stay. `list` prints `stash@{N}: <message>`. `apply` and `pop` merge the stash's changes onto the current index and work tree
with the same tree merge as `merge` (labels `Updated upstream`, `Stash base` and `Stashed changes`), stage files the stash adds,
leave other changes unstaged, restore untracked files that do not already exist, and print the status; `pop` then drops the entry
(`Dropped refs/stash@{N} (<id>)`). A conflict, or local changes to a file the stash touches, keeps the stash. `drop` and `clear`
remove entries. `stash push -- <path>`, `show`, `branch`, `create`, `store`, `--index`, `-k` and `-p` are not built.

git 2.43 cannot stash in a SHA-256 repository, so the SHA-256 stash is tested for consistency, not against git.

### Reflogs

pyn appends the reflog lines git would, when `core.logAllRefUpdates` would: `HEAD`, `refs/heads/*`, `refs/remotes/*` and
`refs/notes/*` get a log created (the default in a work tree, and `true`); `always` creates one for every ref; `false` creates
none; an existing log is appended to either way. The lines are `commit: <subject>` (`commit (initial)`, `commit (merge)`, `commit (amend)`),
`checkout: moving from <old> to <new>`, `branch: Created from <start>`, `Branch: renamed <old> to <new>`, `merge <name>:
Fast-forward` or `Merge made by the 'ort' strategy.`, `reset: moving to HEAD`, the stash message and, for a fetch, `fetch <arguments>: fast-forward` and its siblings (see [Fetch on a git home](#fetch-on-a-git-home)). A line names the committer
identity (with the committer date from `GIT_COMMITTER_DATE`); with no identity configured a reflog line falls back to the login name
so branching and switching still work. A moved branch is also logged in `HEAD`'s log, as git does.

### What pyn will not write

Any command that would write a work tree file stops first, and changes nothing, when git would convert what it writes: `core.autocrlf`
set to true, `core.eol=crlf` with a `text` attribute, `eol=crlf`, a `filter` other than Git LFS, `ident` or `working-tree-encoding`
(`.gitattributes` in the work tree, `.git/info/attributes` and the global attributes file are read; `-text`, `binary`, `eol=lf`,
symlinks and submodules are never converted). A Git LFS filter writes the pointer file as it is, with a warning (decision #245; see [Git LFS](#git-lfs)).
The same limit applies to `status`, which compares raw bytes (#273). Hooks are never run; one warning names the hooks that exist
for the command (`post-checkout`, `pre-merge-commit`, `commit-msg` and the like). Sparse checkouts, submodule updates, the split
index and `GIT_DIR` are not supported, and `rebase`, `cherry-pick`, `revert`, `--squash` and anything that talks to a remote other than `ls-remote`, `fetch`, `pull`, `push` and `clone` are not built yet (epic #301; see [Transports and credentials](#transports-and-credentials)).

### Git LFS

pyn has no LFS client (provisional: a client is not scheduled). `fetch`, `pull`, `clone`, `switch`/`checkout` and the other
commands that write the work tree keep pointer files byte for byte: nothing is smudged, downloaded or cleaned, and `status`
sees them as ordinary files. When `filter=lfs` applies to a path, the command prints one warning on standard error, once
per invocation (so `pull` and `clone` warn once, not once per step). `fetch` and `pull` read the `.gitattributes` files in
the tip of each ref that moved, `clone` in the commit it checks out (none for `--bare` or `-n`), and the work tree commands
read the attributes as for [What pyn will not write](#what-pyn-will-not-write). Only `.gitattributes` files in the tree are
read for `fetch` and `clone`, not `.git/info/attributes` or the global file. The warning wording is not part of the
contract. Covered by `crates/pyn-cli/tests/git_lfs.rs` (#311).

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
| `update` | `update` #310. |
| `get` | Per-path revisions, which git has no equivalent of; not scheduled. |

`pyn convert` fails with `pyn: 'convert' is not available yet` and names #95.

### Packs (library, provisional)

`pyn-git` holds the pack plumbing the git transport uses (see [Transports and credentials](#transports-and-credentials));
`pyn-gittransport` calls it; `pyn fetch` is its first command and `pyn push` sends packs it writes with `write_pack`.

- **Ingest.** `index_pack` reads a pack from any stream, checks the header, every entry's zlib data, the object count and the
  trailing checksum, resolves offset and reference deltas, and computes each object id with git's rules (SHA-1 with collision
  detection, or SHA-256). A thin pack is completed: bases found in the object database are appended to the stored pack, so the
  result is self-contained. The pack and its version 2 index are written to `objects/pack` through temporary files and renames,
  and nothing is left behind on error. Limits cover the object count, object size, delta chain depth and pack size.
- **Write.** `write_pack` writes the given objects as an undeltified version 2 pack and can produce the matching index.
  `PackWriter` takes one entry at a time; new storage forms, such as deltas, are added as entry variants.
- **Walk.** `reachable_objects` lists what `want` needs that `have` does not already imply (tags, commits newest first, then
  trees and blobs), as `git rev-list --objects --objects-edge-aggressive want ^have` does, with the boundary commits.
  A `have` this repository does not hold is ignored; any other missing object is an error. `commits_newest_first` and
  `existing` support negotiation.

## Open items

- Unknown flags on supported commands are a usage error (`129`) from the argument parser, not a message that says the git
  flag is unsupported; making those messages explicit and pointing at the nearest pyn equivalent is #222's intent and is
  tracked with #223 and #224.
- `pyn status` and `pyn commit` output shapes are not aligned with git's, and `-C <dir>` and other global git flags are
  not taken.
