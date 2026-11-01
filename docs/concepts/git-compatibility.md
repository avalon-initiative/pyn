# Git compatibility

Decision #218: where pyn has an equivalent of a git command it uses git's name, flags, argument order and output shape,
so `alias git=pyn` works for everyday flows. pyn owns what git lacks (`lock`, `unlock`, `policy`, modes, leases,
organizations and teams). This page is the compatibility table: every difference from git is deliberate and listed here.
The table describes the CLI as it is. Conformance tests against git are tracked in #224. Everything below is about a repository on a pyn server unless it says *git home*; a
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
   abbreviated, and named by hash, `HEAD`, `~N` and `^`. Git's wire protocol is spoken in process
   (see [Transports and credentials](#transports-and-credentials)), and there is no dependency on git (decision #30): git does not
   need to be installed. `pyn convert` moves a repository between the two homes.
8. **Listings are aligned tables** with the path or variable column last, not git's free-form blocks. This is a
   repository-wide difference: it keeps columns such as mode, lock and revision readable.

9. **A repository has one home** (decision #245): a pyn server or git. In a git repository pyn behaves as git, through its own
   implementation, and commands that act on the repository and only a pyn server provides refuse and say so; see [On a git home](#on-a-git-home).

Exit codes follow git: `0` success, `128` for a failure (message prefixed `fatal: `), `129` for a usage error such as
an unknown flag on a supported command (raised by the argument parser, which names the flag and the command's usage),
`1` for an unknown or git-only command. `diff --exit-code` and `-q` exit `1` when there are differences, as in git.
Messages that git writes to standard output (for example the nothing-to-commit notice and the unmerged-path listing of a
conflicted commit) go to standard output, and errors to standard error. `fetch`, `pull` and `push` outside any repository fail
with `fatal: not a git repository (or any of the parent directories): .git` (`128`). The statuses are checked against git by
`crates/pyn-cli/tests/git_exit_status.rs`, which runs the same command on twin repositories under git and pyn, for error
paths and for successful runs of the supported commands, and compares the exit status and whether standard output and
standard error carry anything, and that a line both print (paths and ids normalised) is on the same stream; a line only
one of them prints is not compared; its allow-list names the differences that remain. During a rebase, a replayed
commit that merges cleanly prints no `Auto-merging` line, as git (a merge commit recreated by `--rebase-merges` does); a
conflicted one prints it with the `CONFLICT` lines on standard output. Where git exits `128` for a bad
value of an option of `log` or `show` (a missing `--since` value), and `255` for a failed
`reflog expire` or `reflog delete`, pyn does the same.

Deliberate status differences that remain:

- **Options of `log`, `show` and `reflog`**: a long option git does not have is `fatal: unrecognized argument` (`128`), as in
  git. pyn checks the name against a list of git's long options taken from its manual (`git_log_options.rs`), so a real git
  option that is not built stays a usage error (`129`). Short options and `reflog expire` or `delete` options are not checked
  against a list: an unknown short one is `128`.
- **`status --porcelain=<version>`** takes `v1` (or `1`); an unknown version such as `v9` is `fatal: unsupported porcelain
  version` (`128`), as git, and `v2` is not built, a usage error (`129`).
- **`rev-parse` with an option it does not know** echoes the option and exits `0`, as git does; `name-rev` with no commit
  prints nothing and exits `0`; `shortlog` outside a repository reads a log from standard input, and with a terminal or a
  revision is a usage error (`129`).
- **`config --remove-section` and `--rename-section`** are not built, a usage error (`129`) where git succeeds or says `fatal: no such
  section` (`128`).
- **`pyn diff` outside a repository** compares two existing paths as `--no-index` does, as git does; anything else asks for a
  server (`128`), where git prints its `--no-index` usage (`129`).

Statuses used below:

| Status | Meaning |
| --- | --- |
| same | Name, arguments and output shape as git. |
| different | Supported, with a deliberate difference; the reason is given. |
| unsupported | Fails with a message; no plan (see [Not planned](#not-planned)) or no pyn equivalent. |
| pyn only | No git equivalent; pyn's own. |

Global options: pyn takes `--server`, `--token`, `--user` and `--repo` (also as `PYN_SERVER`, `PYN_TOKEN`, `PYN_USER`,
`PYN_REPO`) before or after the command, and has no git global flags: `-C <dir>`, `-c <key=value>`, `--git-dir` and
`--work-tree` fail with a usage error (exit `129`). `--version` and `--help` behave as in git.

## Daily set

The commands named in decision #218.

| Git | pyn | Status | Notes |
| --- | --- | --- | --- |
| `git init` | `pyn init [<dir>]` | different | Starts a pyn server-home repository, not a git one: creates `<dir>` if missing (the current folder if omitted) with `.pyn/pyn.toml` (every path `shared`, with a commented example of an exclusive rule) and `.pyn/local_only/` holding `repo.toml` with `repo_tool = "pyn"`, and prints `Initialized empty pyn repository in <path>/.pyn`. In a folder that already has a `.git` or `.pyn`, or inside a git work tree or pyn workspace, it fails (`128`) and, for a git repository, names `pyn convert`. With a server set (`--server`, `PYN_SERVER` or the user setting) and `--repo owner/name`, it asks `Create owner/name on <server>? [y/N]` on a terminal (default no); `--create-on-server` creates it without asking, with the same call as `pyn repo create` and the folder's `.pyn/pyn.toml` as the first policy, and records the server and repository in `.pyn/local_only/config.toml`; `--no-create-on-server` never asks. `--create-on-server` without a server and `--repo` is a usage error (`129`). Without a terminal nothing is created on the server (decision #562). |
| `git init [<dir>]` | `pyn init-git [<dir>]` | same | Starts a new repository: a real `.git` written by pyn (no git program) plus `.pyn/local_only/repo.toml` with `repo_tool = "git"`, and `.pyn/` hidden in `.git/info/exclude`. Creates `<dir>` if missing and prints `Initialized empty Git repository in <path>/.git/`. `-b`/`--initial-branch <name>`, `--object-format sha1\|sha256` and `-q`/`--quiet` work as in git. Divergence: with no `-b` and no `init.defaultBranch` the first branch is `main` (git's default is `master`). `--bare` makes `<dir>` the git directory (`core.bare = true`, no `.pyn/`, prints `Initialized empty Git repository in <dir>/`); `--separate-git-dir <gitdir>` puts the git directory there (its parent must exist, it must be absent or empty) and leaves a `.git` file `gitdir: <absolute path>` in the work tree, and with `--bare` is `fatal` (`128`), as in git. `--template` and `--shared` are a usage error (`129`). In a folder that already has a `.git` or `.pyn`, or inside a git work tree or pyn workspace, it fails (`128`) and names `pyn convert` (decision #562), except `--separate-git-dir` on an existing repository's work tree, which moves its git directory (see [Separate git directory](#separate-git-directory)). |
| | `pyn convert [--home git\|pyn] [-y] [--exclusive <glob>]...` | pyn only | Without `--home`, adopts an existing git repository (hides `.pyn/` in `.git/info/exclude`, creates `.pyn/local_only/`, records `repo_tool = "git"`; safe to run again); outside a git repository, or in a pyn server workspace, it fails with `fatal: ...` (status 128). `--home git` in a pyn server workspace moves it to a git home keeping its history, see [Leaving a pyn server](#leaving-a-pyn-server). `--home pyn` in a git repository moves it to a repository on a pyn server (`--server` and `--repo owner/name`) keeping its history, see [Moving to a pyn server](#moving-to-a-pyn-server); `--exclusive` (repeatable, `--home pyn` only) makes the paths matching a glob exclusive in the starter policy. |
| `git clone <repo> [<dir>]` | `pyn clone <source> [<dir>]` | same | A source that is `<server>/owner/name` or `owner/name` on the configured server is cloned from a pyn server, and prints one summary line, not progress. A git URL (`https://`, `ssh://`, `user@host:path`) is cloned into a git repository with git's behaviour, flags and output; see [Clone from a git URL](#clone-from-a-git-url), which also says how an `http(s)` address is told apart. |
| `git clone -b`, `--bare`, `--mirror`, `--separate-git-dir`, `-o`, `-n`, `--single-branch`, `--no-tags`, `-c`, `-q`, `--progress`, `--depth`, `--shallow-since`, `--shallow-exclude`, `--filter`, `--sparse` | `pyn clone ...` | git URL: supported | Git's behaviour. On a pyn server clone they are a usage error (`129`): that clone takes everything the server has, on its default branch. |
| `git clone --recurse-submodules[=<pathspec>]`, `--recursive`, `--shallow-submodules`, `-j` | `pyn clone ...` | git URL: supported | After the clone, registers and checks out the submodules `submodule.active` selects (all for a bare flag, the pathspecs given otherwise; they are written to `submodule.active` first, as git does), and theirs, with git's messages. `--shallow-submodules` clones each at depth 1. `-j` is accepted. See [Submodules](#submodules). |
| `git clone --also-filter-submodules`, `--reference`, `--template`, `--local`, `--no-hardlinks` | none | unsupported | Usage error (`129`) naming the flag: submodules are not cloned partially yet, and `--local`, `--no-local`, `--no-hardlinks` and `--shared` are not taken: a local-path source is cloned through the transfer protocol, see [Local paths](#local-paths-and-file-urls). |
| `pyn clone --revision <commit>` | | pyn only | Clone as of a commit (a hash, `HEAD~N`). Git does this with `clone` then `checkout`. |
| `pyn clone --no-local-store` | | pyn only | A pyn server clone keeps a partial local store of the history under `.pyn/local_only/store/` and commits locally by default (`--local-store` says the same); this opts out and commits straight to the server. See [the local store](workspace.md#the-local-store). A usage error for a git URL. |
| `pyn migrate` | | pyn only | Converts a workspace made before local commits; see [migrating](workspace.md#migrating-a-workspace). |
| `git status` | `pyn status` | different | Header `On branch <name>`, then a table with mode, revision, staged, state, lock and path plus a summary line, because mode and lock state have no place in git's layout. `STAGED` is `added` or `modified` for a staged path, and `STATE` then describes what changed after it was staged. Lock state is shown in this default table's lock column and by `pyn lock`/`pyn locks`, not in `-s`/`--porcelain` output. On a git home the table is `STAGED`, `UNSTAGED`, `PATH` with git's states. |
| `git status -s`, `--short`, `--porcelain[=v1]`, `-b`, `--branch`, `--renames`, `--no-renames`, `-M[<n>]`, `--find-renames[=<n>]`, `<pathspec>...` | same | same | Git's `XY <path>` lines; see [the short format](#the-short-status-format). Pathspecs are git's (see [Pathspecs](#pathspecs)) and limit every format. The rename flags need a git home. `-z`, `--porcelain=v2`, `--ignored`, `-u<mode>` and `--ahead-behind` are a usage error (`129`). `pyn status --revision` fails on a git home. |
| `pyn status --revision <commit>` | | pyn only | Compare the workspace with a past commit (`behind` / `ahead`). |
| `git add <path>...` | `pyn add <path>...` | same | Stages a path, a folder or `.`; a path is required unless `-u` or `-A` is given. On an exclusive path of a pyn server it fails unless you hold the lock (see below). |
| `git add -u`, `-A`, `-n`, `-v`, `-f` and their long forms | same | same | `-u` and `--update` stage tracked files only; `-A` and `--all` take new files too and, with no path, cover the whole repository. `-n` and `--dry-run` print `add '<path>'` and `remove '<path>'` and stage nothing. Ignored files are refused (exit `1`, as git) without `-f`. `-n` and `-v` list tracked paths first, then new ones, as git does. |
| `git rm [-r] [-f] [-n] [-q] [--cached] [--ignore-unmatch] [--] <path>...` | `pyn rm ...` | supported | Removes tracked files from the work tree and stages the removal. On a git home it is git's `rm`, see [Removing and moving](#removing-and-moving). On a pyn server it stages a deletion for the next commit, and an exclusive path needs your lock first. |
| `git mv [-f] [-n] [-k] [-v] <source>... <destination>` | `pyn mv ...` | supported | Renames a file or folder, or moves them into an existing folder. On a git home it is git's `mv`; on a pyn server it stages a deletion at the old path and an addition at the new one. See [Removing and moving](#removing-and-moving). |
| `git add -p` | `pyn add -p` (`--patch`) | git home only | Asks about each hunk with git's answers, see [Choosing hunks](#choosing-hunks). On a pyn server it is a usage error (`129`): a pyn server stages whole files. |
| `git add -N`, `--intent-to-add` | `pyn add -N` | git home only | Records a new file in the index with empty content and git's intent-to-add flag, so `status` shows ` A`, `diff` shows the file as new and `commit` leaves it out until it is added for real. A tracked path is left alone (a deletion is still staged by a path that names it, `.`, `-A` or `-u`, as git does). Combines with `-n`, `-v`, `-f`, `-u` and `-A`; not with `-p` (usage error, `129`). On a pyn server it is a usage error (`129`). |
| `git add -i`, `--interactive` | `pyn add -i` | git home only | The menu of status, update, revert, add untracked, patch, diff and help, see [Interactive add](#interactive-add). On a pyn server it is a usage error (`129`). Unmerged paths are listed and handled as git does. |
| `git add -e`, `--chmod`, `--renormalize` | none | unsupported | Usage error (`129`). A path is a file, a folder or a git pathspec (globs and `:` magic, see [Pathspecs](#pathspecs)), taken relative to where you stand; on a git home `:(attr:...)` selects by attribute, on a pyn server it is refused. |
| `git add --pathspec-from-file`, `--pathspec-file-nul` | same | same | Also on `rm`, `reset`, `restore`, `checkout` and `stash push`, see [Pathspecs](#pathspecs). |
| `git commit -m <msg>` | `pyn commit -m <msg>` | different | Commits what is staged as one unit of work. On a pyn server it is one changeset uploaded to the server, so commit and publish are one step (no `push` needed), it releases the locks it held, and the output is `<path> is now at revision N` for each file. On a git home it is a git commit on the current branch (or on a detached `HEAD`, which moves only `HEAD`) and prints `[<branch> <id>] <subject>`, with `detached HEAD` in place of the branch name when detached. `-m` repeats for more paragraphs and is taken as given. Without `-m`, `-F` or `-C` the editor opens on both homes ([lookup order](workspace.md#the-editor): `GIT_EDITOR`, `PYN_EDITOR`, the pyn `editor` setting, `core.editor` on a git home, `VISUAL`, `EDITOR`, then `vi`) on `COMMIT_EDITMSG` (`.git/COMMIT_EDITMSG` on a git home, `.pyn/local_only/COMMIT_EDITMSG` on a pyn server): an empty first line, git's "Please enter the commit message" lines, the branch and the staged changes in `#` lines; lines starting with `#` are dropped. A git home also lists the unstaged and untracked paths and opens the editor for `-e`, `--amend`, a merge or squash in progress and `--cleanup` as git does, see [The commit editor on a git home](#the-commit-editor-on-a-git-home); a pyn server opens it only when no message is given and refuses `-e`. On a git home `prepare-commit-msg` sees the template and `commit-msg` the edited message. A message that is empty after that cleanup (from the editor, `-m ""`, only blanks or `#` lines, or an empty `-F` file) aborts with `Aborting commit due to empty commit message.` and exit `1`, on both homes; a git home still takes `--allow-empty-message`. A merge in progress keeps its own message. Nothing staged exits `1` with a message and never opens the editor. |
| `git commit -a`, `-am <msg>` | `pyn commit -a`, `-am <msg>` | same | Stages every changed tracked file, deletions included, then commits; new files are not included. Both homes. |
| `git commit <path>...` | `pyn commit -m <msg> <path>...` | different | On a pyn server: one atomic commit of the named paths as they are on disk, whatever is staged (git's `--only`). Not available on a git home, which says to `add` the paths first. |
| `git commit -F <file>` (`-` is standard input) | same | git home: supported; pyn server: supported | The same message reading and cleanup on both homes, with and without the local store; `-m` and `-F` together are refused. On a pyn server `--file` stays the content file (it has no short form, so `-F` does not clash). See [Commit flags on a git home](#commit-flags-on-a-git-home). |
| `git commit --file=<file>`, `--allow-empty`, `--author=<name <email>>`, `--date=<date>`, `-s`, `--signoff`, `--amend`, `-S[<keyid>]`, `--gpg-sign[=<keyid>]`, `--no-gpg-sign`, `--allow-empty-message`, `--cleanup=<mode>`, `--reset-author`, `--trailer <trailer>`, `-C <commit>`, `--reuse-message=<commit>`, `-e`, `--edit` | same | git home: supported | Git's behaviour on a git home; on a pyn server a usage error (`129`) naming the flag, because commits there are immutable (no `--amend`) and `--file` is the content file. `--allow-empty` is refused there because the server rejects a pushed commit that changes no file. `--author` and `--date` are the exception: a pyn server workspace with the local store takes them on a commit of staged changes (author and author date only; the committer stays the signed-in account, the audit actor the pusher, and they survive a push), and refuses them without the local store, on a path commit and on a merge conclusion. `-s` waits until accounts carry emails. See [Commit flags on a git home](#commit-flags-on-a-git-home). |
| `git commit -q`, `--quiet`, `-n`, `--no-verify`, `--verify`, `-v`, `--verbose`, `--status`, `--no-status`, `--no-edit` | same | supported | Both homes. `-q` prints no summary (errors and warnings stay); `--no-verify` skips `pre-commit` and `commit-msg` on a git home (see [Client hooks](#client-hooks)) and is ignored on a pyn server; the rest are accepted and ignored. |
| `git commit -c <commit>`, `--reedit-message`, `-t <file>`, `--template`, `--fixup`, `--squash`, `--dry-run`, `--short`, `--branch`, `--long`, `--porcelain`, `-z`, `--ahead-behind`, `--untracked-files`, `-i`, `--include`, `-o`, `--only`, `-p`, `--patch`, `--interactive`, `-U`, `--unified`, `--inter-hunk-context`, `--pathspec-from-file`, `--pathspec-file-nul`, `--no-post-rewrite`, `--post-rewrite`, `--no-quiet`, `--no-verbose` and the other `--no-` forms git generates for its options | none | unsupported | Usage error (`129`). Each needs an editor, a status report, or a path selection `commit` does not have: use `add` to choose paths and `-m`/`-F` for the message. |
| `pyn commit -u`, `--untracked` | | pyn only | Stages the untracked files that are not ignored (`.pyn/ignore` on a pyn server, the `.gitignore` rules on a git home) before committing, and combines with `-a` and `-m` in any order: `pyn commit -uam "msg"` stages new, modified and deleted files. It differs from git, where `commit -u` is `--untracked-files` (a status option, still unsupported). Exclusive paths need your lock exactly as for `pyn add`; with it alone, tracked changes stay unstaged. |
| `pyn commit --file <local>` | | pyn only | Content file for a single path. In a workspace the file at the path is the default. |
| `pyn commit --base <rev>` | | pyn only | The revision the edit started from; the server rejects the commit if the branch has moved on from it on an exclusive path. |
| `git log [-n <n>] [<path>]` | `pyn log [-n <n>] [<path>]` (alias `history`) | different | Without a path, newest first, 50 by default. With a path, oldest first and all revisions (git prints newest first). It lists the workspace's branch (`On branch <name>` goes to standard error). Output is a table of per-path revisions, not commit blocks; the message column holds the subject and a body follows its row, indented. `-n` is also `--max-count`; `--limit` is an alias. On a git home it is git's output (see below). |
| `git log --author`, `--grep`, `-i`, `--since`, `--until`, `<rev>`, `<a>..<b>`, `^<rev>`, `[<rev>...] -- <path>` | same | supported | Git's output on a git home (see [Log on a git home](#log-on-a-git-home)). On a pyn server the server filters the commits (`GET /commits` parameters, see [commits](commits.md#filtering-commits)), so the output is the commit listing of `--commits` (or the given format), never a filtered page of a longer history. The differences from git: `--author` and `--grep` are a substring, always ignoring case (`-i` is accepted and changes nothing), and a pattern with `\ [ * ^ $` is a usage error; `--author` matches the name or email of the author; `--since` and `--until` compare the author date, inclusive, and take the date forms of a git home (RFC 3339, `YYYY-MM-DD`, `2 days ago`, ...); a revision is `HEAD`, a commit hash (4 digits or more) or a [local tag](#tags) (workspaces with the local store), each optionally followed by `~N`, `^`, so branch names are not revisions there; a tag of an unpublished local commit is refused with a message (the listing is the server's history), and a path with a tag's name is a path unless `--` follows the revisions; one path only, and not with diff output (`--stat`, `-p`, ...: usage error, `129`); unpublished local commits are not listed when a filter, revision or path is given. |
| `git log --committer`, `--invert-grep`, `--all-match`, `--no-merges`, `--merges`, `--skip`, `--reverse`, `<a>...<b>`, a repeated `--author` or `--grep`, more than one path | same | git home: supported | Git's output on a git home. On a pyn server a usage error (`129`) naming the option (`<a>...<b>` fails with `1`): the server has no committer, merge, count or symmetric filters. |
| `git log --stat`, `--shortstat`, `--name-only`, `--name-status`, `--summary`, `-p`, `-U<n>` | same | supported | Both homes. The diff of each commit against its first parent follows its text, as `show` prints it (a merge shows none unless a merge-diff option asks, see [Merge commits in `show` and `log`](#merge-commits-in-show-and-log)); `--stat` with `-p` separates them with `---`. On a pyn server they list commits like `pyn log --commits` (`-n` or 50, medium format unless a format is given) and take no `--filter` (usage error, `129`); a path or revision narrows the commits (see the filter row above); unpublished local commits are diffed from the local store. Exclusive paths show as in `show`. `--stat`, `--shortstat`, `--name-only` and `--name-status` combine with no other of the four (usage error, `129`), and `-p` with a name flag too. |
| `git log --oneline`, `--format=`, `--pretty=`, `--date=` | same | supported | Both homes, through one renderer (the formats and `%` placeholders of [Log on a git home](#log-on-a-git-home)). On a pyn server they list commits like `pyn log --commits` (newest first, `-n` or 50) and take no `--filter` (usage error, `129`); a path or revision narrows the commits (see the filter row above); `%d` and `%D` are empty there, as no refs are known. | |
| `git log --all`, `--first-parent`, `--decorate[=short\|full\|auto\|no]`, `--no-decorate`, `--decorate-refs[-exclude]`, `-<n>` | same | git home: supported | Git's output on a git home (see [Log on a git home](#log-on-a-git-home)). `-<n>` is `-n <n>`. On a pyn server a usage error (`129`) naming the option. |
| `git log --follow`, `-S<string>`, `-G<regex>` | same | git home: supported | Git's output on a git home (see [Log on a git home](#log-on-a-git-home)); `--follow` takes diff output, showing the renames it tracks. On a pyn server a usage error (`129`). |
| `git log -E`, `-F`, `--basic-regexp` | same | git home: supported | The pattern syntax of `--grep`, `--author` and `--committer`; the last of the switches wins. On a pyn server a usage error (`129`). |
| `git log -P` | none | unsupported | Usage error (`129`): pyn has no Perl-compatible regular expression engine (the `regex` crate has no lookaround or back-references, and a PCRE engine would be a new dependency), and `-E` covers extended expressions. |
| `git log --graph` | same | git home: supported | Git's output on a git home, in git's topological order (see [Log on a git home](#log-on-a-git-home)), with a path, diff output and commit filters; not with `--follow`, and a usage error with `--show-signature`. On a pyn server a usage error (`129`). |
| `git log --show-signature` | same | git home: supported | Prints each signed commit's verification report after its `commit` line (see [Signing](#signing)). With `--oneline`, a `--format=` template or `--graph` a usage error (`129`); on a pyn server a usage error. |
| `git log --notes` | none | unsupported | Usage error (`129`). `pyn log` is of the workspace's branch; another branch's log needs a switch (`pyn log <branch>` is not built). |
| `pyn log --commits` | | pyn only | List commits (`COMMIT`, `AUTHOR`, `WHEN`, `FILES`, `MESSAGE`) instead of revisions. |
| `pyn log --filter <glob>` | | pyn only | Only paths matching the glob. |
| `git diff [<c1> [<c2>]] [-- <path>...]` | `pyn diff [<c1> [<c2>]] [-- <path>...]` | different | Unified output as git, computed in the CLI. Commits are hashes (or abbreviations of 4 or more digits), `HEAD`, `~N`, `^`, or `a..b`; two commits work outside a workspace. With no commit it compares the work tree with the index: the staged content, or the last update for what is not staged. File modes print as in git, including `old mode` and `new mode`. On a git home an argument that is neither a revision nor an existing path (a deleted file needs `--`) fails with `fatal: ambiguous argument '<arg>'` (or `bad revision` before `--`), exit `128`, as git does; `a...b` is not supported and fails the same way. |
| `git diff -U<n>`, `--unified=<n>`, `--stat`, `--name-only`, `--name-status`, `--exit-code`, `-q` | same | same | `--name-status` prints `A`, `M`, and `D`; gitlink type changes print `T`, and on a git home a rename or copy prints `R<nn>` or `C<nn>` (see [Rename and copy detection](#rename-and-copy-detection)). On a git home `--stat` (also in `show`, `log` and `stash show`) prints a binary file as git does, `Bin <old> -> <new> bytes` (`Bin` alone for a rename with identical content, as in the merge diffstat), aligned in the graph column and left out of the insertion and deletion totals; `--numstat` prints `-` and `-`. |
| `git diff --numstat` | same | different | Both homes. Prints added and deleted line counts per path, or `-` and `-` for a binary file. It combines with none of `--stat`, `--shortstat`, `--name-only`, `--name-status` or `--check` (usage error, `129`); git can combine numeric and other stats or give another format precedence. |
| `git diff --cached`, `--staged` | `pyn diff --cached`, `--staged` | different | The staged content against the last commit (the last update on a pyn server). On a git home it takes one optional commit to compare with, as git; on a pyn server it takes none. |
| `git diff --shortstat`, `-w` (`--ignore-all-space`), `--check` | same | same | Both homes. `--shortstat` prints only the totals line. `-w` compares lines with all whitespace removed, prints context lines as they are in the new file, and leaves out a file that differs only in whitespace (so `--exit-code` is `0` when nothing else changed); `--name-only` and `--name-status` still list it, as git 2.43 does (newer git versions differ on this edge; pyn lists the file). `--check` prints `path:line: trailing whitespace.` (or `space before tab in indent.`, `indent with spaces.`, `tab in indent.`, or several) and the added line, `path:line: leftover conflict marker`, then `path:line: new blank line at EOF.`, for the rules of `core.whitespace` (git's default without it), and exits `2` if it printed anything; `-w` does not affect it (newer git versions differ here too). `--check` and `--shortstat` combine with no other format (`--stat`, `--name-only`, `--name-status`): a usage error (`129`), where git prints both stats or refuses with `128`. |
| `git diff -b`, `--ignore-space-change`, `--ignore-space-at-eol`, `--ignore-cr-at-eol`, `--ignore-blank-lines` | same | git home: supported | Git's output with the lines compared as git compares them; a file whose changes all fall away prints nothing (a change to blank lines only still counts as a difference for `--exit-code`). `--stat`, `--numstat`, `--shortstat`, `--name-only` and `--name-status` follow, and so do `show`, `log -p` and `stash show`. `--check` ignores them, as git does. `--raw`, `--name-only`, `--name-status` and `--check` still list a file that differs only in ignored whitespace (with zero ids in `--raw`), as git 2.43 does; newer git leaves it out and prints real ids. The line diff is a port of git's xdiff (the one `blame` uses), so ambiguous changes land where git puts them. |
| `git diff -W` (`--function-context`), `--inter-hunk-context=<n>`, `--minimal`, `--diff-algorithm=default\|myers\|minimal`, `--indent-heuristic`, `--no-indent-heuristic` | same | git home: supported | A hunk grows to the whole function (a line starting with a letter, `_` or `$` begins one), hunks within the distance merge, and the algorithm flags change how the edit script is found. `diff.indentHeuristic` and `diff.algorithm` are read; `patience`, `histogram` and `--anchored` are a usage error (`129`). |
| `git diff --raw`, `-z`, `--full-index`, `--abbrev[=<n>]`, `--no-abbrev`, `--src-prefix`, `--dst-prefix`, `--no-prefix`, `-a` (`--text`), `-R`, `--diff-filter=<kinds>`, `-s` (`--no-patch`), `-p` | same | git home: supported | Git's output. `--raw` shows an id of zeros for a work tree file; `-z` ends the records of `--raw`, `--name-only`, `--name-status` and `--numstat` with NUL; `--raw` ids are shortened to seven digits unless `--abbrev=<n>` (at least 4) says otherwise, and `index` lines follow the [abbreviation policy](#log-on-a-git-home); `-R` swaps the sides and the `a/` and `b/` prefixes; `--diff-filter` takes upper case letters to keep and lower case to leave out (`*` and `log` are a usage error); `diff.noprefix` is read (not `diff.mnemonicPrefix`). `-p` joins `--stat`, `--shortstat`, `--numstat`, `--raw` and `--dirstat` (`--patch-with-stat` and `--patch-with-raw` too), a stat then a blank line then the patch; with `--name-only`, `--name-status` or `--check` it is a usage error (`129`), as is `-s` with another format. `-u`, `--binary` and `--irreversible-delete` are a usage error (`129`). |
| `git diff --word-diff[=plain\|color\|porcelain\|none]`, `--word-diff-regex=<regex>`, `--color-words[=<regex>]` | same | git home: supported | Git's word diff of each run of changed lines; words are runs of non-space characters, or the matches of an extended regular expression (`diff.wordRegex` too; matches never span a line). Output of `--word-diff=color` is colored whatever `color.ui` says. In `diff`, `show`, `log -p` and `stash show`. |
| `git diff --color[=always\|never\|auto]`, `--no-color`, `--ws-error-highlight=<kinds>` | same | git home: supported | Git's colors for the headers, hunk headers, lines, whitespace errors, `--stat` and `--check`, and for the `commit` line of `show` and `log` (decorations and `--graph` are a usage error with color, `129`). `color.diff`, `color.ui` (`auto` is off when standard output is not a terminal), `color.diff.<slot>` (`meta`, `frag`, `func`, `old`, `new`, `context`, `whitespace`, `commit`), `core.whitespace` and `diff.wsErrorHighlight` are read; `--color-moved` is a usage error (`129`). |
| `git diff --no-index [<path> <path>]` | same | supported | Compares two files, a file with the same-named file in a directory, or two directories recursively, in or out of a repository; with two existing paths and no repository `--no-index` is implied. Exits `1` when they differ. A path of `/dev/null` is an empty side. File and directory under one name, standard input (`-`), `-C` and `-B` are a usage error (`129`). The output is git's, including the names in `--stat` (`{d1 => d2}/f`) and `--raw` (zero ids for changed files); git 2.43 prefixes the current folder to the names `--summary` lists when run in a subfolder of a repository; pyn does not, as newer git. |
| `git diff --stat=<width>[,<name-width>[,<count>]]`, `--stat-width`, `--stat-name-width`, `--stat-graph-width`, `--stat-count`, `--compact-summary`, `--dirstat[=<params>]`, `--dirstat-by-file`, `--cumulative` | same | git home: supported | Git's diffstat layout (the terminal width is `COLUMNS`, else 80; `diff.statGraphWidth` is read), `...` for files left out by the count, `(new)`, `(gone)`, `(mode +x)` and the like with `--compact-summary` (which implies `--stat`), and the share of the change per folder for `--dirstat` (`changes`, `lines` and `files` measures, `cumulative`, a cut-off). `--dirstat` by changes counts bytes by git's line chunks: the last chunk of a file with no final newline is counted by git 2.55 and not by 2.43. Also in `show`, `log` and `stash show` (not `log --patch-with-stat`). A `--stat` option does not combine with `--numstat`, `--shortstat`, `--name-only`, `--name-status`, `--check` or `--raw` (usage error, `129`). |
| `git diff -M[<n>]`, `--find-renames[=<n>]`, `-C[<n>]`, `--find-copies[=<n>]`, `--find-copies-harder`, `-B[<n>][/<m>]`, `--break-rewrites[=<n>][/<m>]`, `--no-renames`, `-l<n>`, `--summary` | same | git home: supported | Rename, copy and rewrite detection and the `--summary` lines, as git does them; also in `show`, `log` and `stash show`. See [Rename and copy detection](#rename-and-copy-detection). `-B` is a usage error (`129`) together with a combined diff, `--no-index`, color, `--word-diff`, whitespace options, `--check` and `--dirstat`. |
| `git show [<commit>...] [-- <path>...]` | `pyn show [<commit>...] [-- <path>...]` | different | Commit header (`commit`, `Author:`, `Date:`), message, then the commit's diff. The newest commit if none is named. The author line carries the email. On a git home the output is git's, with a merge commit shown as a combined diff (see [Merge commits in `show` and `log`](#merge-commits-in-show-and-log)): it takes several commits, and paths (after `--`, or existing ones) limit each diff. A commit that changes none of the paths still prints its header, as git 2.55 does (git 2.43 and 2.56 leave it out; the tests accept both). A pyn server takes one commit and no paths (usage error, `129`). |
| `git show`, `git log` with `-m`, `-c`, `--cc`, `--dd`, `--first-parent`, `--diff-merges=<mode>`, `--remerge-diff`, `--no-diff-merges`, and `log.diffMerges` | same | git home: supported | A merge's combined (`-c`), dense combined (`--cc`, the default of `show`), per-parent (`-m`), first-parent and remerge diffs, byte for byte as git prints them. `--combined-all-paths` is not built. See [Merge commits in `show` and `log`](#merge-commits-in-show-and-log). A pyn server refuses them (`129`). |
| `git show -U<n>`, `--stat`, `--shortstat`, `-w`, `--check`, `--name-only`, `--name-status` | same | same | The diff flags behave as on `diff` (whitespace modes, `-W`, `--raw`, `--word-diff`, `--color`, `--stat` widths, `--dirstat`, `-R`, ...), and `--stat` is laid out as git lays it out (long names cut with `...`, graph scaled to `COLUMNS` or 80); `--check` exits `2` when it reports a problem; `--stat` with `-p` puts `---` between the message and the stat. |
| `git show --numstat` | same | different | The numeric rows and format-combination limits are the same as `diff --numstat`. |
| `git show <rev>:<path>` | `pyn get <path> [--rev N] [-o <file>]` | git home: supported; pyn server: different | On a git home `show <rev>:<path>` (and `:<path>`, `<rev>:./<path>`) prints a blob's bytes or a tree as `tree <name>`, a blank line and its entry names (folders end in `/`), byte for byte as git, in the order given, also mixed with commit and tag arguments (a commit shown once is not shown again, as in git); a name that resolves to nothing or a submodule entry falls through to the commit path (`128`), and so does `<rev>:<path>` with a pathspec after `--` (git shows the blob). On a pyn server `show` takes commits only (`HEAD:foo` fails with `fatal: ... is not a commit`, `128`); file content at a revision is `pyn get`, to stdout or a file. `--rev` is a per-path revision, not a commit. |
| `git show --oneline`, `-s` (`--no-patch`), `--format=`, `--pretty=`, `--date=` | same | supported | Both homes, with the formats of `log`. `-s` prints the commit only; `--oneline` and the formats print their line, then the diff (blank line between for a template or a named format, none for `--oneline`). Without them a pyn server prints its own header (see above). |
| `git show --show-signature` | same | git home: supported | As for `log`: the report follows each signed commit's `commit` line. A tag's own signature is not checked, as in git 2.43. A usage error on a pyn server. |
| `git show <tag>` | `pyn show <tag>` | different | On a pyn server with the local store, an annotated tag prints `tag <name>`, `Tagger:`, `Date:` and its message, then the commit; a lightweight tag, or a tag with `~N`/`^` steps, shows the commit. A git home prints the same, then the commit, or for a tag of a tree or blob the tree listing or the content (`Tagger:` and `Date:` follow the format as in git: `oneline` has neither, `fuller` has `TaggerDate:`, the other formats have no date; `--date` is honoured). `show --show-signature` on a tag is refused. |
| `git show <object>` | `pyn show <object>` | git home: supported; pyn server: unsupported | On a git home a blob or tree id prints like `show <rev>:<path>`. On a pyn server: `fatal: ... is not a commit`. |
| `git restore --source=<rev> <path>` | `pyn restore --source <rev> <path>` (or `pyn restore <path> <rev>`) | different | pyn's restore appends a new revision on the server: it needs the lock, the `restore` permission and a typed confirmation (`--yes` skips it); `-m` sets the message. The revision is required (git defaults to the index or HEAD) and `-s` is the short form of `--source`. Git's restore only edits the working tree. |
| `git restore --staged <path>...` | `pyn restore --staged <path>...` (`-S`) | same | Puts the staged entries under the paths back to the last commit and leaves the files alone. On a git home `--source <commit>` takes the entries from that commit, as git; on a pyn server there is no commit to take them from and it is a usage error. |
| `git restore [-s <rev>] [-S] [-W] <path>...` (the work tree) | `pyn restore ...` on a git home | git home only | Takes the paths from the index (or `--source`) into the work tree, and with `--staged --worktree` into both. See [Branches, switching and the work tree](#branches-switching-and-the-work-tree). On a pyn server `--worktree` is a usage error (`129`): a restore there makes an older revision the new head. |
| `git restore --ours\|--theirs [-W] <path>...` | `pyn restore --ours\|--theirs <path>...` | git home only | Writes one side of an unmerged path into the work tree and leaves the path unmerged. On a pyn server it is a usage error (`129`). See [`restore`](#branches-switching-and-the-work-tree). |
| `git restore --merge <path>...` | `pyn restore --merge <path>...` | git home only | Recreates the conflicted merge of an unmerged path in the work tree, with diff3 markers. `--merge` has no `-m` short form (that is `--message`), and `--conflict=<style>` is not accepted. |
| `git checkout -p [<tree-ish>] [--] [<path>...]`, `git reset -p [<tree-ish>] [--] [<path>...]` | `pyn checkout -p`, `pyn reset -p` | git home only | The prompt of `restore -p`, see [Choosing hunks](#choosing-hunks): `checkout -p` discards from the work tree (index to work tree) or, with a tree-ish, from the index and the work tree (`Discard` for `HEAD`, `Apply` for anything else); `reset -p` unstages (`Unstage`, or `Apply ... to index` for a tree-ish other than `HEAD`) and works on an unborn branch. `reset -p` with `--soft`, `--mixed`, `--hard`, `--keep` or `--merge` is `fatal` (`128`); `checkout -p` with `-f`, `--detach`, `-b` or `-B` is `fatal` (`128`) with git's wording, `--ours` and `--theirs` are ignored (`fatal` with a tree-ish, as git), and `--yes` is a usage error (`129`). On a pyn server both are usage errors. |
| `git restore -p` | `pyn restore -p` (`--patch`) | git home only | Asks about each hunk of the work tree, the index (`--staged`) or both, from the index, `HEAD` or `--source`, see [Choosing hunks](#choosing-hunks). On a pyn server it is a usage error (`129`). |
| `git reset [--soft\|--mixed\|--hard\|--keep\|--merge] [-q] [<rev>]`, `git reset [<rev>] [--] <path>...` | `pyn reset ...` | git home: supported; pyn server: different | On a git home it is git's reset, see [Reset](#reset). On a pyn server `pyn reset [--soft\|--mixed\|--hard] [<commit>]` only drops unpublished local commits ([workspace](workspace.md#the-local-store)); `--keep`, `--merge`, `-q`, paths and a second revision are a usage error there. |
| `git branch`, `git switch`, `git checkout` | `pyn branch`, `pyn switch`, `pyn checkout` | supported | See [branching and merging](#branching-and-merging). On a git home they work on `.git` as git does, see [Branches, switching and the work tree](#branches-switching-and-the-work-tree). |
| `git merge` | `pyn merge` | supported | See [branching and merging](#branching-and-merging). On a git home it merges as git does, see [Merging](#merging). |
| `git fetch` | `pyn fetch` on a git home | git home only | See [Fetch on a git home](#fetch-on-a-git-home). On a pyn server it fetches into the local store (a workspace cloned with `--no-local-store` runs `pyn update`), see [Remotes on a pyn server](#remotes-on-a-pyn-server). |
| `git pull` | `pyn pull` on a git home | git home only | Fetch, then merge through `pyn merge`; see [Pull on a git home](#pull-on-a-git-home). On a pyn server it fetches into the local store and merges (a workspace cloned with `--no-local-store` runs `pyn update`), see [Remotes on a pyn server](#remotes-on-a-pyn-server). |
| `git push` | `pyn push` on a git home | git home only | See [Push on a git home](#push-on-a-git-home). On a pyn server it publishes the local commits of the current branch to the branch it follows, or of the branches named; `-u`, `--delete <branch>` and `origin :<branch>` work (also without the store), a branch with no upstream is refused as with `push.default=simple`, and a branch the server lacks is created by the push (a workspace cloned with `--no-local-store` prints that there is nothing to push, because a commit goes straight to the server); see [Remotes on a pyn server](#remotes-on-a-pyn-server) and [local branches](workspace.md#local-branches). |
| `git remote` | `pyn remote` | git home; pyn server partly | On a git home it lists and edits the remotes in `.git/config`, see [Remotes](#remotes). On a pyn server `pyn remote`, `pyn remote -v` and `pyn remote get-url origin` show the one remote, `origin`: the server and repository set by `--server` and `--repo` or `pyn config set server` and `repo`; `add`, `remove`, `rename` and `set-url` say that a server home has exactly one home and exit `1`. |
| `git tag [-a] [-s] [-u <keyid>] [--no-sign] [-m <msg>] [-f] [<name> [<commit>]]`, `-d`, `-l` | `pyn tag ...` | supported | See [Tags](#tags). On a pyn server the tags are local-only refs in the local store (workspaces with `local_store` on); they are never pushed, and `-s`, `-u` are refused (#447). |
| `git stash [push] [-p] [-u] [-k] [-m <msg>] [-- <paths>]`, `list`, `pop`, `apply` (`--index`), `drop`, `clear`, `show`, `branch`, `create`, `store` | `pyn stash ...` on a git home | git home only | See [Stashing](#stashing). On a pyn server it prints the git-command message. |
| `git clean [-n] [-f] [-d] [-x\|-X] [-q] [-i] [-e <pattern>] [--] [<path>...]` | `pyn clean ...` on a git home | git home only | See [Clean](#clean). Without `-f`, `-n` or `-i` it refuses, as `clean.requireForce` does; `-i` is git's menu; pathspecs are git's, see [Pathspecs](#pathspecs). |

`pyn update [<path>...]` is the pre-branch stand-in for `fetch` plus `pull`: it fast-forwards files without local
changes and never overwrites an edit, reporting skipped files. `--revision <commit>` moves the workspace to an older or
newer commit. It is pyn only; on a pyn server `pyn fetch` and `pyn pull` run it on a pyn server. A git home has `pyn pull` instead, and `pyn update` is refused there.

## History and inspection

Commands people reach for that are not in the daily set.

| Git | pyn | Status | Notes |
| --- | --- | --- | --- |
| `git ls-files` | `pyn files` | different | Table of every file with mode, head revision and lock holder. No flags. On a git home `pyn files` lists the index, every file `shared`, with no revision or lock, and `pyn ls-files` is git's command; see [Inspection commands](#inspection-commands). |
| `git ls-tree [<rev>] [<path>]` | `pyn ls [<path>]` | different | One folder with each entry's mode, last change and lock. Head only; no revision argument. On a git home `pyn ls` lists `HEAD`'s tree, with no last change, and `pyn ls-tree` is git's command; see [Inspection commands](#inspection-commands). |
| `git verify-commit [-v] [--raw] <commit>...`, `git verify-tag [-v] [--raw] <tag>...` | same | git home: supported | Check the signature with the configured program and exit `1` when any is missing or not good (see [Signing](#signing)); `verify-tag --format` is not built. They need a git repository. |
| `git cat-file`, `git show <rev>:<path>` | `pyn get <path>` | different | See `show` above. On a git home `pyn cat-file` is git's command; see [Inspection commands](#inspection-commands). |
| `git rev-parse`, `git show-ref`, `git shortlog`, `git describe`, `git name-rev` | the same names on a git home | git home only | See [Inspection commands](#inspection-commands). On a pyn server they fail with `fatal: not a git repository (or any of the parent directories): .git` (`128`). |
| `git config --list`, `--get <key>`, `<key> <value>` | `pyn config --list`, `--get`, `<key> <value>` | git home: supported | Git's flag forms, on git's own config files, with git's output and exit statuses. See [Config flag forms](#config-flag-forms). |
| `pyn config list`, `get`, `set`, `init` | the same | pyn only | The subcommands read and write pyn's own settings (`server`, `repo`, `user`), a different store from git config. `--global` and `--local` are accepted where they apply (`set` takes both, the default being `--local`, the workspace or git repository). `pyn config init` prompts for the user settings. |
| `git config --remove-section`, `--rename-section`, `-e`, `--blob`, `--type`, `--bool`, `--get-regexp`, `--show-origin`, `-z` and value patterns | none | unsupported | Usage error (`129`). |
| `git fsck` | `pyn verify [<path>]` | different | Checks every revision's hash and its link to the one before, and the audit log's chain. Silent and exit 0 when intact, as git; otherwise a `PROBLEM`, `WHAT`, `PATH` table and exit 1. `-v` / `--verbose` also prints what was checked. See [integrity](integrity.md). On a git home `pyn fsck` is git's command; see [Storage maintenance](#storage-maintenance). |
| `git reflog [show\|list\|exists\|expire\|delete]` | `pyn reflog ...` on a git home | git home only | See [Reflogs](#reflogs). On a pyn server it prints the git-command message; the closest analogue there is `pyn audit`, an append-only log of lock, revision and access events, filtered with `--path`, `--actor`, `--action`, `--before`, `--limit` and permission-gated (`view_audit`). |
| `git count-objects`, `git fsck`, `git prune`, `git repack`, `git gc`, `git pack-refs` | the same names on a git home | git home only | See [Storage maintenance](#storage-maintenance). On a pyn server they fail with `fatal: not a git repository (or any of the parent directories): .git` (`128`). |
| `git grep [<options>] [-e] <pattern> [<rev>...] [[--] <path>...]` | `pyn grep ...` on a git home | git home only | See [Grep](#grep). Searches the tracked files of the work tree, the index (`--cached`) or the commits named; `-n -i -w -l -c -v -q -h -H -a -I -E -G -F`, `-e` (repeatable), `--and`, `--or`, `--not` and parentheses, pathspecs after the revisions or `--`. On a pyn server or local-store workspace it says it needs a git repository (`1`). |
| `git bisect start [--term-(bad\|new)=<term> --term-(good\|old)=<term>] [--no-checkout] [--first-parent] [<bad> [<good>...]] [--] [<pathspec>...]`, `good`\|`old`, `bad`\|`new`, `skip`, `next`, `reset`, `run`, `log`, `replay`, `terms` | `pyn bisect ...` on a git home | git home only | See [Bisect](#bisect). Git's search, output, exit statuses and `.git` state, so git and pyn can continue each other's bisection; `visualize` and `view` are not built (`129`). On a pyn server or local-store workspace it fails with `fatal: not a git repository (or any of the parent directories): .git` (`128`). |
| `git blame [-L <range>] [-w] [<options>] [<rev>] [--] <file>`, `git annotate` | `pyn blame ...`, `pyn annotate ...` on a git home | git home only | See [Blame](#blame). Git's lines byte for byte: the default, `-e`, `-s`, `-f`, `-n`, `-l`, `-t`, `-b`, `--root`, `--abbrev`, `--date`, `-p`, `--line-porcelain`, `-L` ranges, `--first-parent`, `<rev>`, `<a>..<b>` and `^<rev>`. `-M`, `-C` and `--incremental` are usage errors (`129`). On a pyn server or local-store workspace it says it needs a git repository (`1`). |
| `git submodule add\|status\|init\|deinit\|update\|set-branch\|set-url\|summary\|foreach\|sync\|absorbgitdirs` | `pyn submodule ...` on a git home | git home only | See [Submodules](#submodules). On a pyn server it prints `pyn: 'submodule' manages a git repository's nested repositories, and this is not one` (`1`). `status` and `diff` also inspect recorded gitlinks and checked-out submodules without changing them; see [On a git home](#on-a-git-home). |
| `git worktree add\|list\|lock\|unlock\|move\|prune\|remove\|repair` | `pyn worktree ...` on a git home | git home only | See [Linked work trees](#linked-work-trees). On a pyn server it prints `pyn: 'worktree' manages a git repository's work trees, and this is not one` (`1`). |
| `git format-patch`, `git apply`, `git am` | the same names on a git home | git home only | See [Patches](#patches). On a pyn server `format-patch` and `am` fail with `fatal: not a git repository (or any of the parent directories): .git` (`128`), as does `apply`, which git would run outside a repository too. |
| `git archive`, `git notes` | none | unsupported | `pyn: '<cmd>' is not a pyn command` (`1`). Only the commands in the git-only list print the "git command pyn does not support yet" message, so these read as unknown until that list is widened (#223). |

## Branching and merging

Branches and merge exist on a pyn server ([branches](branches.md), [merging](merging.md)); remotes on a pyn server are the one server and repository (see [Remotes](#remotes)). The model is
decision #233 (commit graph, per-commit trees, branch heads); policy per branch is [branches](branches.md#policy-and-modes); exclusive paths form a tree of
revisions with a pointer per branch (decision #299).

| Git | pyn | Status | Notes |
| --- | --- | --- | --- |
| `git branch [<name> [<start>]]`, `-d`, `-D`, `-m`, `-l`, `-v`, `--show-current` | `pyn branch ...` | supported | Without the local store `-d` refuses a branch that is not on the default branch (git: not merged into `HEAD`). With it (the default for a clone) branches are the store's own, as git's: `-d` refuses one that is in neither its upstream nor `HEAD`, `-a` and `-r` list the remote-tracking branches `fetch` stored, `-u <upstream>` and `--unset-upstream` work, and nothing reaches the server (see [local branches](workspace.md#local-branches)). Without the store `-a` and `-r` list the server as the remote `origin` (see [Remotes on a pyn server](#remotes-on-a-pyn-server)); `-M`, `-c`, `-C`, `-f` and patterns (and `-u` and `--unset-upstream` without the store) are not built on a pyn server (usage errors); a git home has them (see below). The workspace's branch cannot be deleted. |
| `git switch <branch>`, `-c <new> [<start>]`, `-f` | `pyn switch ...` | supported | Local changes carry over where the target branch has the same file, and stop the switch (nothing changed, the paths listed) where it does not; `-f` discards them. `-` works on a pyn server, `--detach` with the local store ([detached HEAD](branches.md#detached-head)); `-C` is not built; a git home has `-`, `--detach`, `-C` and `--orphan`, and `-t`/`--track`/`--no-track` (see below). Prints `Switched to branch '<name>'` on standard error, as git. On a git home a name that is no branch, tag or commit but maps through one remote's fetch refspecs to an existing remote-tracking branch creates a local branch there that tracks it (see below). With the local store a name that is only a server branch makes a local branch following it (`branch '<name>' set up to track 'origin/<name>'.`, fetched first if the store lacks it), and a switch leaves unpublished commits on their branch ([local branches](workspace.md#local-branches)). |
| `git checkout <branch>`, `-b <new> [<start>]`, `-f`, `[<tree-ish>] -- <path>...` | `pyn checkout ...` | supported | Reserved for git's meaning. `-- <path>` restores from the revision the workspace has (git: the index; a staged path must be unstaged first), `<branch\|commit> -- <path>` from that branch or commit. On a pyn server a path that is `exclusive` and has a local edit (locked by you or not) is not discarded silently: the files are named and the discard needs the path typed back or `--yes`, and a run without a terminal refuses (`128`, nothing changed); `shared` paths discard silently as in git. With the local store `checkout <commit>` and `--detach` detach HEAD ([detached HEAD](branches.md#detached-head)); without it there is no detached HEAD and `checkout <commit>` is refused with what to use instead. Taking a lock is `pyn lock`. |
| `git merge <branch\|commit>`, `-m`, `--no-ff`, `--ff-only`, `--no-commit`, `--abort`, `--continue`, `--stat`, `-n`/`--no-stat`, `-e`/`--edit`, `--no-edit`, `--squash`, `-s`, `-X`, `-S`, `--signoff`, `--log`, `-F`, `--cleanup`, `--into-name`, `--allow-unrelated-histories`, `--autostash`, `--verify-signatures`, `--quit` | `pyn merge ...` | supported | Shared files merge three-way with diff3 conflict markers; a binary conflicts as a whole file. Exclusive files take the pointer that descends from the other, and conflict where both branches changed one or a path became exclusive while a branch edited it (a person chooses a side, taking the lock). A conflicted merge leaves the workspace as git does and is concluded with `pyn add` and `pyn commit` (no `-m` needed), or given up with `--abort`. Differences: `--preview` (pyn's own) reports what would merge and conflict and changes nothing; a merge needs a clean index and no local edits to the files it touches; `--no-verify` and every option of a git home (`--squash`, `-s`, `-X`, `-S`, `--no-gpg-sign`, `--signoff`, `--log`, `-F`, `--cleanup`, `--into-name`, `--allow-unrelated-histories`, `--autostash`, `--verify-signatures`, `--quit`, `--ff`, `--commit`, `-q`, `-v`, `--progress`) are usage errors (they need a git repository; see [Merging](#merging), [Merge options](#merge-options) and [Signing](#signing)). `git checkout --ours` and `--theirs` are `pyn checkout --ours\|--theirs -- <path>`. `pyn status` marks unmerged paths in its `STATE` column. Recursive merge of criss-cross histories as git does, with the base of a conflict shown as `merged common ancestors`. |
| `git cherry-pick <commit>...`, ranges, `-n`, `-x`, `-s`, `-S[<keyid>]`, `--no-gpg-sign`, `-m <n>`, `--continue`, `--skip`, `--abort`, `--quit` | `pyn cherry-pick ...` | supported | Each commit's change becomes a new commit, with its author and message. On a git home see [Cherry-pick](#cherry-pick). On a pyn server workspace with local commits the commits, ranges and `-x` work, and `-n`, `-s`, `-S`, `--no-gpg-sign`, `-m` and `--quit` are refused; see [cherry-pick in commits](commits.md#cherry-pick). A pyn server workspace without local commits refuses the command. |
| `git revert <commit>...`, ranges, `-n`, `-s`, `-S[<keyid>]`, `--no-gpg-sign`, `-m <n>`, `--no-edit`, `--continue`, `--skip`, `--abort`, `--quit` | `pyn revert ...` | supported | Each commit's change is undone by a new commit (`Revert "<subject>"`). On a git home see [Revert](#revert). On a pyn server workspace with local commits the commits and ranges work, and `-n`, `-s`, `-S`, `--no-gpg-sign`, `-m` and `--quit` are refused; see [revert in commits](commits.md#revert). A pyn server workspace without local commits refuses the command. |
| `git rebase [--onto <newbase>] [<upstream> [<branch>]]`, `--continue`, `--skip`, `--abort`, `--quit`, `--autostash`, `--no-ff`, `--no-verify`, `--verify`, `-S[<keyid>]`, `--no-gpg-sign` | `pyn rebase ...` | supported (git home) | Replays the commits after the upstream one by one onto the new base, with the same ids as git. See [Rebase](#rebase), including `--rebase-merges`, `--root`, `--update-refs` and the fork point. A pyn server workspace with a local store rebases only its unpublished local commits, interactively too (`edit`, `exec`, `break`, `--exec`, `--edit-todo`, `--autostash`, `--no-ff`, `--quit` and `<branch>` work), and refuses a range with a published one, `--rebase-merges`, `--root`, `--fork-point` and `--update-refs` ([commits](commits.md#rebase)); without a local store it refuses the command. |

## Remotes

| Git | pyn | Status | Notes |
| --- | --- | --- | --- |
| `git remote [-v]`, `add`, `remove`, `rename`, `set-url`, `get-url` | `pyn remote ...` | git home: supported | Configuration only, no network. Output, exit statuses and the resulting `.git/config`, refs and reflogs match git. See [Remotes on a git home](#remotes-on-a-git-home). On a pyn server: `remote`, `remote -v` and `get-url`, see [Remotes on a pyn server](#remotes-on-a-pyn-server). |
| `git ls-remote [--heads] [--tags] [--refs] [--symref] [--exit-code] [<repository> [<pattern>...]]` | `pyn ls-remote ...` | same | Works on a git home and anywhere with a URL. Output is git's: `<id>`, a tab and the name, with `<tag>^{}` for a peeled tag and `ref: <target>`, a tab and `HEAD` for `--symref`. `<repository>` is a remote name (default `origin`, which prints `From <url>` on standard error as git does) or a URL; see [Transports and credentials](#transports-and-credentials). `--get-url`, `--sort`, `-o`, `-q` and `--upload-pack` are usage errors (`129`). |
| `git remote add -f` | `pyn remote add -f ...` | git home: supported | Adds the remote, then runs `pyn fetch <name>`. If the fetch fails the remote stays defined, as in git. |
| `git remote show [-n] [<name>...]`, `prune [-n] <name>...`, `update [-p] [<group>\|<remote>...]`, `set-head <name> (-a \| -d \| <branch>)`, `set-branches [--add] <name> <branch>...` | `pyn remote ...` | git home: supported | Ask the remote over ssh or smart HTTP, with git's output and exit statuses; see [Querying remotes on a git home](#querying-remotes-on-a-git-home). Other options are usage errors (`129`). |
| `git fetch [<options>] [<remote>\|<url>] [<refspec>...]` | `pyn fetch ...` | git home: supported | Git's behaviour, flags, output and exit statuses; see [Fetch on a git home](#fetch-on-a-git-home). |
| `git pull [<remote> <branch>]` | `pyn pull` | git home: supported | See [Pull on a git home](#pull-on-a-git-home). Pyn server: runs `update`. |
| `git push [<options>] [<repository> [<refspec>...]]` | `pyn push ...` | git home: supported | Git's behaviour, flags, output and exit statuses; see [Push on a git home](#push-on-a-git-home). Pyn server: says there is nothing to publish, because `pyn commit` already writes to the server; local commits and a real `push` there are decision #300, built in #313. |
| `git clone` | `pyn clone` | same | See the daily set and [Clone from a git URL](#clone-from-a-git-url). |
| `git bundle create [--version=<n>] <file> [<rev-list-args>]`, `verify`, `list-heads`, `unbundle` | `pyn bundle ...` | same | See [Bundles](#bundles). Works on a git home; `list-heads` works anywhere. |

### Remotes on a pyn server

A pyn server home has exactly one remote, `origin`: the server and repository it is set to use.

| Command | Behaviour |
| --- | --- |
| `pyn remote -v` | `origin`, a tab and `<server>/<owner>/<name> (fetch)`, then the same with `(push)`. |
| `pyn remote get-url [--push] [--all] origin` | The same URL `remote -v` shows, one line, exit `0`. Another name is `error: No such remote '<name>'` (`2`). |
| `pyn remote add`, `remove`, `rename`, `set-url` | Refused with a message that a server home has exactly one home (one server, one repository), exit `1`. |
| `pyn fetch`, `pyn pull` | Fetch into the local store and `pull` then fast-forwards, makes a merge commit when the histories diverged as git does, or with `--rebase` replays local commits ([commits](commits.md#fetch-pull-and-push-with-a-local-store)). With any argument or option they are usage errors (`129`), except `pull --rebase`, `--no-rebase` and, to conclude a pull that stopped on conflicts, `--abort`, `--continue` and `--skip` ([conflicts in a pull](commits.md#conflicts-in-a-pull)). A workspace cloned with `--no-local-store` runs `pyn update` instead and takes no arguments. |
| `pyn push` | Publishes the local commits, merge commits included, and exits `1` on a refusal. Arguments are usage errors. A workspace cloned with `--no-local-store` prints that there is nothing to push, because commits go straight to the server; exits `0`. |
| `pyn branch -r`, `-a` | `-r` lists the server's branches as `origin/<branch>`; `-a` lists the local ones, then `remotes/origin/<branch>`. `-v` works with both. Only listing is allowed with them. |

### Remotes on a git home

`pyn remote` reads and edits `remote.<name>.*` and `branch.<name>.*` in the repository's `.git/config` and the refs under
`refs/remotes/`. The subcommands in this table never contact a remote; those that do are under [Querying remotes on a git home](#querying-remotes-on-a-git-home).

| Command | Does |
| --- | --- |
| `pyn remote` | The remote names, sorted, one per line. |
| `pyn remote -v` | `<name>`, a tab and `<url> (fetch)` (the first URL), then one `<url> (push)` line for each push URL (`pushurl`, else each `url`). A remote with no URL prints `<name>` and a tab. |
| `pyn remote add <name> <url>` | Writes `url` and the default `fetch = +refs/heads/*:refs/remotes/<name>/*`. `--mirror=fetch` writes `fetch = +refs/*:refs/*` instead, `--mirror=push` writes `mirror = true` and no `fetch`, and a bare `--mirror` writes both and warns that it is deprecated, as git does; any other value is `error: unknown mirror argument` (`129`). An invalid name is `fatal: '<name>' is not a valid remote name` (`128`), an existing one `error: remote <name> already exists.` (`3`). `-t <branch>` (repeatable) writes one `fetch = +refs/heads/<branch>:refs/remotes/<name>/<branch>` per branch instead of the default, or `+refs/<branch>:refs/<branch>` with `--mirror=fetch`; with `--mirror=push` it is `fatal: specifying branches to track makes sense only with fetch mirrors` (`128`). `-m <master>` points `refs/remotes/<name>/HEAD` at `refs/remotes/<name>/<master>` (logged when that branch exists) and is `fatal: specifying a master branch makes no sense with --mirror` (`128`) with any `--mirror`. `-f` then fetches the remote (see [Fetch on a git home](#fetch-on-a-git-home)). |
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
| `pyn remote set-head <name> (-a\|--auto \| -d\|--delete \| <branch>)` | Points `refs/remotes/<name>/HEAD` at `refs/remotes/<name>/<branch>`, at the branch the remote's `HEAD` names (`-a`, printing `<name>/HEAD set to <branch>`; `error: Cannot determine remote HEAD` or `error: Multiple remote HEAD branches...`, `1`, when it cannot tell), or deletes it. A branch with no tracking ref is `error: Not a valid ref: refs/remotes/<name>/<branch>` (`1`). Any other combination is a usage error. The wording of the `-a` message is that of git 2.43. |
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
| `--auto-gc`, `--no-auto-gc` | After fetching, run `gc --auto` unless `--no-auto-gc` or `--dry-run` is given, as git does; see [Auto gc](#auto-gc). |
| `--write-commit-graph`, `--no-write-commit-graph` | Accepted and ignored. |
| `--recurse-submodules[=yes\|on-demand\|no]`, `--no-recurse-submodules` | After the fetch, fetches in the checked-out, active submodules; see [Submodules](#submodules). Without a flag `fetch.recurseSubmodules`, then `submodule.recurse`, then `on-demand`. |
| `--depth`, `--deepen`, `--shallow-since`, `--shallow-exclude`, `--unshallow` | Git's shallow fetch, see [Shallow repositories](#shallow-repositories). |
| `--update-shallow` | Git's meaning, see [Shallow repositories](#shallow-repositories). |
| `--filter=<spec>`, `--no-filter`, `--refetch` | Git's partial fetch, see [Partial clones](#partial-clones). |
| `--atomic`, `--set-upstream` | Usage error (`129`): not built. Any other unknown flag is a usage error as everywhere. |

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
least 10. On a git home, ids use `core.abbrev` as a minimum width and lengthen for uniqueness, including objects in alternates.
On a git home, unset or `auto` chooses the default from the packed object count; `no`, `off` and `false` show full ids.

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
there is no per-object "Receiving objects" progress,
only what the server sends;
`fetch.output`, `fetch.showForcedUpdates`, `fetch.writeCommitGraph`, `--refmap`, `--negotiation-tip`, `--server-option`, `--stdin` and `-4`/`-6` are not read or taken; `--all` runs
the remotes one at a time whatever `-j` or `fetch.parallel` say, so `FETCH_HEAD` lists them in configuration order; the stale-ref hint after a ref could
not be written names `pyn remote prune`.

### Shallow repositories

`pyn clone --depth <n>`, `--shallow-since <date>` and `--shallow-exclude <rev>` (repeatable), and `pyn fetch --depth <n>`,
`--deepen <n>`, `--shallow-since`, `--shallow-exclude` and `--unshallow`, take git's meaning and its `.git/shallow` file (one
boundary commit per line, removed when the history is complete). A shallow flag on a clone implies `--single-branch` unless
`--no-single-branch` is given; `--tags` does not widen a single-branch clone. A local-path source ignores the flags with git's
`warning: --depth is ignored in local clones; use file:// instead.`, and a `file://` URL honours them.

- `--depth <n>` keeps `n` commits from each tip; `--deepen <n>` moves the current boundary `n` commits further (it does nothing on
  a complete repository); `--unshallow` fetches the rest; `--shallow-since` keeps commits newer than the date (the same date forms
  as `log --since`); `--shallow-exclude` keeps commits not reachable from the revision. A bad or conflicting combination is
  `fatal:` (`128`) with git's wording.
- Every fetch from a shallow repository tells the server which commits are boundaries; the server answers with the boundary changes
  and pyn rewrites `.git/shallow` (a boundary commit that gained parents is dropped, new ones are added). A server that cannot do
  shallow requests is refused cleanly.
- A boundary commit has no parents for every command: `log`, `show`, `describe`, `blame`, `merge` (a merge whose base lies beyond
  the boundary is `refusing to merge unrelated histories`), `rebase`, `bisect`, fetch negotiation and the objects a push sends. A
  push from a shallow clone works when the remote holds the boundary's ancestry. `pyn rev-parse --is-shallow-repository` says which.
- Cloning from a shallow repository inherits its boundary (a fetch does with `--update-shallow`), over a path, `file://`, ssh and HTTP.
- Wire: protocol v2 `shallow`, `deepen`, `deepen-relative`, `deepen-since`, `deepen-not` and the `shallow-info` section, and the v0/v1
  `shallow` and `deepen*` lines. The in-process upload-pack for local remotes answers all of them, `--shallow-since` and
  `--shallow-exclude` together included (a depth does not combine with either, as in git).
- Fetching from a shallow repository: the roots it announces that this repository lacks the parents of are recorded only
  with `--update-shallow` (a clone always does); without it, a ref whose history reaches such a root is rejected with
  `warning: rejected <ref> because shallow roots are not allowed to be updated` (exit status `0`, not in `FETCH_HEAD`), as
  git does. A root whose history is complete here is ignored. `--shallow-submodules` is a `clone` option and is supported
  there.
- A failed connectivity check after a shallow fetch puts the previous `.git/shallow` back.

### Partial clones

`pyn clone --filter=<spec>` and `pyn fetch --filter=<spec>` take git's object filters and its promisor configuration, so a
repository either tool made is a partial clone for both. Supported specs: `blob:none`, `blob:limit=<n>[k|m|g]` (blobs of at
least `n` bytes are left out), `tree:<depth>` (trees and blobs at that depth from the root tree or deeper; `tree:0` leaves out
every tree and blob until asked for) and `combine:<a>+<b>` (also what several `--filter` options make). `sparse:<...>` and
`object:type=<...>` are refused with `... is not built`; any other spec is `fatal: invalid filter-spec '<spec>'` (`128`).

- A partial clone records `remote.<name>.promisor = true` and `remote.<name>.partialclonefilter = <spec>` and sets
  `core.repositoryformatversion = 1`. The spec is the default for later fetches from that remote (`--no-filter` fetches
  everything, `--filter` overrides it for one fetch and, like git, does not replace the stored default); `fetch --filter` on a
  complete clone turns it into a partial one. `--refetch` asks again for every object as a fresh clone would, applying the
  filter in effect, without negotiating. Packs received from a promisor remote get a `.promisor` file beside them.
- Objects the clone lacks are fetched from the promisor remote when a command reads them: `checkout`, `switch`,
  `merge`, `diff`, `show`, `log -p`, `blame`, `grep`, `cat-file` and `ls-tree -l` need no flag. Checkout and `switch` ask for all the
  blobs of the files they write in one request; other commands ask object by object (a missing tree brings its subtrees). The
  request is a protocol v2 `fetch` with `want <id>` and `filter blob:none`, so the server must allow filters
  (`uploadpack.allowFilter`), and an unreachable remote fails the command that needed the object. `fetch` and `merge` do
  not download trees or blobs only to check for LFS pointers, so no LFS warning is printed in a partial clone.
- `fsck`, `prune`, `repack` and `gc` treat an object a promisor pack names, or that an object in one names, as present rather
  than missing, and never download. `repack -a` keeps the objects of promisor packs in a pack with a `.promisor` file. A fetch's
  connectivity check follows commits and as many trees as the filter keeps.
- A server that does not advertise `filter` gets no filter: `warning: filtering not recognized by server, ignoring`, a full
  fetch, and the promisor settings are still recorded, as git does. A local-path source ignores `--filter` like `--depth`
  (`warning: --filter is ignored in local clones; use file:// instead.`) and also records the settings; `file://`, ssh and HTTP
  honour it. Combined with `--depth` and the other clone flags it behaves as in git.
- Wire: protocol v2 `fetch` `filter <spec>` (and the v0/v1 `filter` capability and line). The in-process upload-pack for local
  remotes advertises `filter` when `uploadpack.allowFilter` is true and answers `blob:none`, `blob:limit`, `tree:<depth>` and
  `combine:`; objects named in a `want` are always sent. Partial fetches ask for packs without thin deltas.

`crates/pyn-cli/tests/git_partial.rs` compares clones, fetches and the commands above with real git on twin clones (config, refs, the
objects held and the objects `git rev-list --objects --missing=print` reports missing, `.promisor` files, `git fsck`), over ssh,
`file://`, local paths and HTTP, and runs pyn against partial clones git made.

Not done: `sparse:<oid>` and `object:type=` filters, `rev-list --missing=...` (pyn has no `rev-list`), a filter of its own for a
second promisor remote (those are tried in name order), writing `extensions.partialClone` (it is read), `clone
--also-filter-submodules`, and bundles with a `@filter=` capability.

### Pull on a git home

`pyn pull [<options>] [<repository> [<refspec>...]]` is `pyn fetch <repository> <refspec>...` (same remote choice, same output,
`FETCH_HEAD` written) followed by `pyn merge` of the fetched merge candidates into the current branch, as `git pull` with the
merge mode. Git's own prose is not copied everywhere: where a message names a command it names `pyn`.

1. `--rebase`, `-r`, `pull.rebase` or `branch.<name>.rebase` set to `true` rebases instead of merging, decided after the fetch.
   A dirty work tree or index is `error: cannot pull with rebase: You have unstaged changes.` (or `Your index contains
   uncommitted changes.`, `128`) before anything is fetched, unless `--autostash` or `rebase.autoStash` is set. A candidate the
   branch already contains prints `Current branch <b> is up to date.` and exits `0`; a branch that can fast-forward (or is
   unborn) fast-forwards exactly as without rebase; a branch with local commits the candidate lacks has them replayed onto the
   candidate as [`pyn rebase`](#rebase) does, and a conflict or a stop is concluded with `pyn rebase --continue`, `--skip` or
   `--abort` (a git home has no `pull --continue`, as git has none). The value is `true` (also `yes`, `on`, `1`), `false` (also
   `no`, `off`, `0`, empty), `merges` or `m` (the replay recreates merge commits, as `pyn rebase --rebase-merges` does) or
   `interactive` or `i` (the editor opens on the instruction sheet, as `pyn rebase -i` does, whenever commits need replaying); the values are case sensitive, `-r<value>` is `--rebase=<value>`, and `preserve`
   or any other value is `error: invalid value for '--rebase': '<v>'` (`129`) or, from `pull.rebase` or `branch.<name>.rebase`,
   `fatal: invalid value for '<key>': '<v>'` (`128`, before the fetch). The reflog action is `pull` with the arguments as typed
   (`pull --rebase=merges (pick): ...`), and a fast-forward inside the replay is logged as `rebase: fast-forward`, as in git. The
   rebase reads `rebase.rebaseMerges`, `rebase.updateRefs`, `rebase.autoSquash` (with `interactive`) and `rebase.autoStash`
   as `pyn rebase` does. The commits to replay start at the fork point of the remote-tracking ref (as `merge-base --fork-point`
   finds it in the ref's reflog) when the pull names the branch's own remote and upstream, or the tracking ref of the first
   refspec argument; otherwise at the fetched tip. `--no-rebase`, `--rebase=false`, `pull.rebase=false` and
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
| `--ff`, `--no-ff`, `--ff-only`, `--commit`, `--no-commit`, `--stat`, `-n`, `--no-stat` | As for `pyn merge`; `--no-commit` stops before the commit with `MERGE_HEAD` set, and the diffstat follows `--stat`, `-n` and `merge.stat`. |
| `--edit`, `--no-edit` | Passed to the merge, see [Merge message editor](#merge-message-editor); there is no short `-e` (git has none for `pull`). Ignored when the pull rebases or fast-forwards. |
| `--no-rebase`, `--rebase`, `-r` | `--no-rebase` merges; `--rebase` fast-forwards or replays as above. |
| `--autostash`, `--no-autostash` | Stash before the merge or the rebase and apply after, as for `pyn merge` and `pyn rebase`; the last of the two wins over `merge.autoStash` and `rebase.autoStash`. |
| `--squash`, `--no-squash`, `--signoff`, `--no-signoff`, `--log[=<n>]`, `--no-log`, `--cleanup=<mode>`, `--no-cleanup`, `--verify-signatures`, `--no-verify-signatures`, `--verify`, `--no-verify`, `--allow-unrelated-histories`, `--no-allow-unrelated-histories` | Passed to the merge exactly as for `pyn merge`, with the `merge.*` settings it reads (`merge.log`, `merge.verifySignatures`, `merge.autoStash`), see [Merge options](#merge-options). The `--log` heading is `'<branch>' of <url>`, as git writes it. A bad `--cleanup` mode is `fatal` (`128`) before the fetch. A pull that rebases or fast-forwards ignores the message options, and `--squash` with `--rebase` behaves as in git. `-F` and `-m` are not pull options (usage error, `129`, as in git). |
| `-s`, `--strategy`, `-X`, `--strategy-option` | Passed to the merge as for `pyn merge`, see [Strategies and options](#strategies-and-options). A strategy name git does not define is `Could not find merge strategy '<name>'.` (`1`); more than one `-s`, and `-X` options pyn cannot honor, are usage errors (`129`). With a pull that rebases they go to the rebase as for `pyn rebase`, see [Strategies in a replay](#strategies-in-a-replay). |
| `-S[<keyid>]`, `--gpg-sign[=<keyid>]`, `--no-gpg-sign` | Passed on: the merge commit of a merge pull, or each commit a rebasing pull makes, is signed as for `pyn merge` and `pyn rebase`, see [Signing](#signing). |
| `--recurse-submodules[=yes\|on-demand\|no]`, `--no-recurse-submodules` | Fetches in submodules as `fetch` does; with `yes` (or `submodule.recurse`) the checked-out submodules are then updated to the merged result. |
| `--no-strategy`, `--all` | Usage error (`129`): not built (`--all`: use `pyn fetch --all`; `--no-strategy` because git keeps the strategies named before it). |

Config read: `pull.ff`, `pull.rebase`, `branch.<name>.rebase`, `branch.<name>.merge`, `branch.<name>.remote`, `merge.ff`, plus what
`pyn fetch` reads, plus `merge.stat` and `pull.twohead` (the default strategy of a merge, as for `pyn merge`). `pull.octopus` and `branch.<name>.mergeOptions` are not read. A merge pull also reads `merge.log`, `merge.verifySignatures`, `merge.autoStash` and the other settings of [Merge options](#merge-options). The message of an annotated tag
is not appended to a tag merge's message. Pulling on a pyn server is #310 and still prints the git-command message.

### Clone from a git URL

`pyn clone [<options>] <url> [<dir>]` makes a new git repository from a git remote over the system `ssh` or smart HTTP, with no
`git` program (see [Transports and credentials](#transports-and-credentials)). It prints `Cloning into '<dir>'...` (or
`Cloning into bare repository '<dir>'...`) on standard error, the server's progress only with `--progress` or on a terminal, and
nothing but errors with `-q`. A failure is `fatal: ...` with exit `128`; an unsupported flag is a usage error (`129`).

**Which sources are git.** `ssh://`, `git+ssh://`, `ssh+git://`, scp-like `[user@]host:path`, and any other URL scheme or local path
(served in process, see [Local paths](#local-paths-and-file-urls)) are always git; a relative name such as `repo` is a local path only when that folder is a git repository, else it is read as `owner/name`. A bare `owner/name` is a pyn server
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
branch. `--mirror` is `--bare` that mirrors every ref: it fetches `+refs/*:refs/*` (notes and any other namespace too) and writes
`remote.<name>.fetch = +refs/*:refs/*` and `remote.<name>.mirror = true`, so a later `pyn fetch` brings the remote's new refs and
`pyn push` sends all of them. With `--depth` (or another shallow flag) a mirror fetches only the cloned branch, as git does, and `--depth` composes with `--bare`, `--mirror` and `--separate-git-dir` (the `shallow` file lives in the git directory). `--separate-git-dir <gitdir>` puts the git directory there (absent or empty, its parent existing) and
leaves a `.git` file `gitdir: <absolute path>` in the work tree; with `--bare` or `--mirror` it is `fatal` (`128`). `-o <name>` names the remote. `-c <key>=<value>` is written to the new repository's config before the fetch, so transport
settings such as `http.extraHeader` or `url.<base>.insteadOf` apply to it. `-v` and `-j` are accepted and change nothing.

**Empty and unusual remotes.** An empty remote gives `warning: You appear to have cloned an empty repository.` (also with `-q`), an
unborn `HEAD` on the branch the remote's `HEAD` names (or, when the server cannot say, `init.defaultBranch`, else `master`) with
`branch.<name>.*` set, and no checkout. A remote whose `HEAD` names a branch it does not have, while other
branches exist, keeps that unborn `HEAD`, fetches the rest and warns `remote HEAD refers to nonexistent ref, unable to checkout`.

**Failure.** If the clone fails before the fetch is complete (an unreachable host, a failed sign-in, a bad `-b`), what it created is
removed: the new directory, or only `.git` (or the contents of a bare target) when the directory already existed and was empty. A
failure while checking out leaves the repository in place, as git does.

Differences from git, all deliberate or not yet done: no hooks, templates or `description` file are created; there is no "Updating files" progress; `--also-filter-submodules`,
`--reference`, `--reference-if-able`, `--dissociate`, `--template`, `--local`, `--no-local`,
`--no-hardlinks`, `--shared`, `--upload-pack`, `--server-option`, `--bundle-uri` and `--ref-format` are usage errors. `--sparse`
is supported; see [Sparse checkout](#sparse-checkout).

### Bundles

`pyn bundle` reads and writes git's bundle files, v2 and v3, with pyn's own pack writer and reader (no `git` program).

- `create [--version=2|3] [-q] <file> [<rev-list-args>...]` writes the commits and refs the arguments select; `<file>` of `-` writes to standard output.
  Arguments are ref names, `--all` (refs, then `HEAD`), `--branches`, `--tags`, `--remotes` (each with an optional `=<glob>`), `^<rev>`, `--not` and `<a>..<b>`. A positive
  argument that is a ref (`main`, `v1`, `HEAD`) is recorded under its full name; the excluded commits that are parents of included ones become
  prerequisites (`-<id> <subject>`). A bundle with no ref or no object is refused (`Refusing to create empty bundle.`, `128`). The default version is 2, and 3 for a SHA-256 repository, whose
  header carries `@object-format=sha256`; version 2 with SHA-256 or a version other than 2 and 3 is `fatal` (`128`). The pack holds whole objects, never deltas.
  `-<n>`, `--max-count`, date limits and `<a>...<b>` are not built (`1` for an option, `128` for a symmetric range).
- `verify <file>` checks the header, the prerequisites against this repository and the pack's checksum, then prints git's report on standard output
  and `<file> is okay` on standard error. A repository lacking prerequisites prints `error: Repository lacks these prerequisite commits:` and exits `1`; outside a repository it is `fatal: Need a repository to verify a bundle.` (`128`).
- `list-heads <file> [<ref>...]` prints `<id> <name>` per ref, in the bundle's order, and needs no repository.
- `unbundle <file> [<ref>...]` stores the bundle's objects (a thin pack is completed from this repository) and prints the refs; it changes no ref.

A bundle file is also a remote: `pyn clone <file>`, `pyn fetch <file> <refspec>...`, `pyn pull`, `pyn remote add` with a bundle path, and a `file://` URL to a `.bundle`.
pyn serves the bundle's refs and its pack through the same upload-pack exchange a local repository does. A bundle with prerequisites the repository lacks fails a
fetch with the same error and exit status `1` (a clone fails with `128` and leaves nothing behind). Clone and fetch from a SHA-256 bundle adopt its object format as for any remote.
A bundle carries no `HEAD` symref, so a clone guesses the branch as it does for a dumb remote. Not built: bundles with a `@filter=` capability, shallow requests against a bundle, pushing to one, and `--bundle-uri`.

### Push on a git home

`pyn push [<options>] [<repository> [<refspec>...]]` sends commits and refs to a git remote over the system `ssh` or smart HTTP,
with no `git` program (see [Transports and credentials](#transports-and-credentials)). It prints git's table on standard error
and exits `0` when every ref was pushed, `1` when a ref was rejected or failed, `128` on a fatal error and `129` on a usage
error.

**Which remote.** `<repository>` is a remote name or a URL. Without one: `branch.<name>.pushRemote`, then `remote.pushDefault`,
then `branch.<name>.remote`, else `origin`; with none configured it is `fatal: No configured push destination.` (`128`). The
URLs are the remote's `pushurl`, else its `url`; every one is pushed to in turn. `url.<base>.pushInsteadOf` and `insteadOf` are
applied. A remote with `remote.<name>.mirror = true` is pushed as `--mirror` is.

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
| `--progress`, `--no-progress`, `--verify`, `--no-verify`, `--thin`, `--no-thin`, `-4`, `-6`, `--recurse-submodules=no` | `--no-verify` skips the `pre-push` hook on a git home (see [Client hooks](#client-hooks)); `--verify` is the default. The others are accepted and ignored. The server's own messages (`remote: ...`, including its hooks' output) are always shown. |
| `--mirror` | Sends every ref under `refs/` (branches, tags, notes, remote-tracking refs, anything else) as `+refs/*:refs/*` and deletes the remote refs this repository does not have. Not with refspecs, `--all`, `--tags` or `--delete` (`fatal`, `128`). A remote with `remote.<name>.mirror = true` (`remote add --mirror=push`) does this on a plain `pyn push <name>` and refuses refspecs. |
| `--prune` | Also deletes the remote refs that a glob refspec (`refs/heads/*:refs/heads/*`, or what `--all` and `--tags` stand for) maps from a local ref that does not exist; refs outside the refspecs' destinations stay. Plain refspecs have nothing to prune, and `--mirror` already prunes. It is evaluated before the push, so `-n` shows the deletions. |
| `--signed`, `-o`, `--push-option`, `--recurse-submodules` (other than `no`), `--receive-pack`, `--exec`, `--repo`, `--force-if-includes` | Usage error (`129`) naming the flag. `--force-if-includes` is refused because ignoring it would push less safely than asked. |

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
   each ` <flag> <summary> <from> -> <to> (<reason>)` in git's columns. The summary column is `2 * abbrev + 3` wide.
   On a git home, ids use `core.abbrev`'s minimum width, extended for uniqueness across local and alternate stores;
   unset or `auto` derives the default from the packed object count, and `no`, `off` and `false` show full ids.
   Then `-u`, then the tracking refs, then `error: failed to push some refs to '<url>'` and one
   `hint:` block (naming `pyn pull`) for the first kind of rejection, unless `advice.pushUpdateRejected` or the specific
   `advice.push...` setting is false; or `Everything up-to-date` when nothing needed sending.
4. For a push to a named remote it moves each remote-tracking ref the remote's fetch refspecs map the pushed ref to (and deletes
   it for a delete), for every ref that was pushed or already up to date, with the reflog line `update by push` (following
   `core.logAllRefUpdates`, as fetch does).

Differences from git, all deliberate or not yet done: no per-object "Enumerating objects" or "Writing objects" progress, only what the
server sends; `push.autoSetupRemote`, `push.negotiate`, `push.gpgSign`, `push.recurseSubmodules`,
`push.useForceIfIncludes` and `remote.<name>.receivepack` are not read; `--dry-run` with `-u` prints `Would set upstream of ...`
instead of setting anything.
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
credentials, a local path and a `file://` URL, each for SHA-1 and SHA-256 repositories. The per-command suites (`git_clone.rs`, `git_fetch.rs`, `git_pull.rs`, `git_push.rs`) go deeper on one command and also run against a local path and a `file://` URL, so
those are compared with real git without a server or an ssh shim; `git_local_remote.rs` covers what only a local repository can do.

Oracle tests assert semantics, not git's exact output (decision #718): the exit status class (success, usage error 129, other
failure), the facts a result must carry, and the resulting files and refs, using containment and structured checks. Whole-output
equality with git is kept only where the bytes are the contract (plumbing output other tools parse, file formats, on-disk state).
Git releases reword messages, change abbreviation widths and move failure statuses between 1 and 128, so the suite passes on
git 2.55 and 2.56 without branching on the version. The comparators are in `crates/pyn-cli/tests/common/oracle.rs`.

### URLs

| Form | Transport |
| --- | --- |
| `https://host/path`, `http://host/path` | Smart HTTP |
| `ssh://[user@]host[:port]/path`, `git+ssh://...`, `ssh+git://...` | System `ssh` |
| `[user@]host:path` (scp-like; `[::1]:path` for IPv6) | System `ssh` |
| `/path`, `./path`, `file:///path` | Served in process, see [Local paths and `file://` URLs](#local-paths-and-file-urls) |
| `git://host/path` | Not supported: unauthenticated and unencrypted |

`url.<base>.insteadOf` and `pushInsteadOf` (longest matching prefix wins; `pushInsteadOf` first when pushing) are applied when
connecting, so `remote -v` still shows what is configured. A password or token in a URL is never printed: messages show the URL
without its user information, and error text from the network layer is redacted the same way.

### Local paths and `file://` URLs

A remote that is a directory on this machine is served by pyn itself, in process: the same upload-pack (protocol v2 `ls-refs` and
`fetch`) and receive-pack (`report-status`, `delete-refs`, `atomic`, `side-band-64k`) exchanges a network remote speaks, answered
from the repository's objects and refs. No `git` program, no child process and no hidden command are involved, so the only
path ever opened is the one the remote names. It must be a git directory or a work tree root (`<path>.git` is tried too), and its
object store must be readable, else `fatal: '<path>' does not appear to be a git repository` (`128`). `file://` takes an empty host
or `localhost`; the path is percent-decoded. `clone` records a relative path as an absolute one, as git does. A relative URL on a remote of a git home (`../remote.git`) is resolved from the work tree root wherever `pyn` runs, for `push`, `fetch`, `pull` and `ls-remote`, as git does.

On a push the served repository decides like `git receive-pack`, with the same refusals and wording: updating or deleting the checked-out
branch of a non-bare repository is refused (`receive.denyCurrentBranch`: `refuse` by default, `ignore`, `warn`), deleting the current branch
of any repository is refused (`receive.denyDeleteCurrent`), and `receive.denyDeletes` and `receive.denyNonFastForwards` are honoured. A new
tip must be connected to objects the repository holds. Configuration is the user's global files under the repository's own.

Refused cleanly, because they need machinery pyn does not run: a repository with an executable `pre-receive`, `update`, `post-receive`,
`post-update`, `push-to-checkout` or `reference-transaction` hook, `receive.fsckObjects` or `transfer.fsckObjects`, any `hideRefs`
setting, and `receive.denyCurrentBranch=updateInstead`. Not done: the served repository gets no reflog entries from a push, a branch
checked out in a linked worktree is not protected, packs are sent without deltas, and the objects of a rejected push stay in the
repository unreferenced (there is no quarantine).

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
| `pyn repo` (`create`, `list`, `explore`, `visibility`, `usage`, `delete`) | Repositories live on the server; `pyn init-git` starts a local git-home repository (see `git init` above). |
| `pyn setup`, `pyn admin`, `pyn usage` | First-run setup, server administration and per-owner usage and limits. |

## Not planned

Fail with the git-command message today (the unsupported list in `crates/pyn-cli/src/unsupported.rs`); no issue
schedules them.

| Git | Reason |
| --- | --- |
| Hooks in `.git/hooks` beyond the ones built | A git home's client hooks run as git runs them (see [Client hooks](#client-hooks)); the ones not built are warned about. pyn's own hooks are tracked in #216. Maintaining a git home's storage and bundles are built; see [Storage maintenance](#storage-maintenance) and [Bundles](#bundles). |

## On a git home

Decision #245: a repository has one home, a pyn server or git. In a git repository (a `.git` is found) pyn behaves as git,
through its own implementation of git's object store, refs and index, with no git library or binary (decision #30). A
`.pyn/pyn.toml` there is ignored: no path is exclusive, and the commands that need a pyn server refuse and name it. Going
from a git home to a pyn server is one explicit step, `pyn convert --home pyn`: see [Moving to a pyn server](#moving-to-a-pyn-server). The other direction is [Leaving a pyn server](#leaving-a-pyn-server).

**Nothing git sees changes except what you asked for.** Hooks, notes, configuration and line endings are never touched, and
the work tree changes only where a command you ran says it does (`switch`, `checkout`, `restore`, `merge`, `stash`), never
over local changes you have not saved unless you forced it. Reading commands change nothing in `.git/`. `pyn convert` writes
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
a pre-marker `pyn init` created or a committed `.pyn/pyn.toml`, leaves the home git. A marker that is not `git`
or `pyn` is an error naming the file.
`GIT_DIR` and `GIT_WORK_TREE` are not read.

`pyn convert` in a git repository adds `.pyn/` to `.git/info/exclude` (the shared one, found through the `.git` file in a
worktree or submodule) unless a line there already hides it, creates `.pyn/local_only/`, where pyn keeps its own state
for a git home, and records `repo_tool = "git"` there. It is safe to run again.

### Leaving a pyn server

`pyn convert --home git` (decision #574) turns a pyn server workspace into a git home and keeps the history. It lists what is lost
and asks `Continue? [y/N]`; `-y` or `--yes` skips the question, and without a terminal and without `--yes` it fails (`128`).

- **Kept:** every branch, and every commit with its author, committer, times, parents, message and signature, the trees and the
  content of every file. The server's SHA-256 ids are kept, so the result is a SHA-256 repository (`extensions.objectformat`) and
  signatures stay valid; a SHA-1 git home is rehashed on the way to a server, see [Moving to a pyn server](#moving-to-a-pyn-server). Local commits the server lacks, and the local tags,
  come along from the local store; a branch that has diverged from the server's is refused (`pull` or `rebase` first).
- **Lost:** locks (a lock held on the server stays there and no longer applies), revision numbers, the shared and exclusive
  modes (`.pyn/pyn.toml` stays as local state that a git home ignores), changes staged with `pyn add`, and the record of which
  revision each file is at. Files that were read-only because of the exclusive mode become writable.
- **How:** the commits and trees come through the fetch route (all branches), each file by its id, verified by hash. The new
  `.git` is built in `.git.pyn-convert` and renamed into place only when complete, so an error leaves the workspace as it was.
  The index is written from `HEAD`, so unmodified files are clean and edited ones show as modified. The command then records
  `repo_tool = "git"`, hides `.pyn/` in `.git/info/exclude`, and removes `.pyn/local_only/state/` and `store/` (the marker alone
  decides the home; the settings in `config.toml` stay). It refuses when a `.git` exists or a merge is in progress.

### Moving to a pyn server

`pyn convert --home pyn --server <url> --repo <owner/name>` (decision #562, issue #572) moves a git home repository to a
repository on a pyn server and keeps the history. It shows what it will do and asks `Continue? [y/N]`; `-y` or `--yes` skips
the question, and without a terminal and without `--yes` it fails (`128`) with nothing changed. `--server` can come from
`PYN_SERVER` or the user config; `--repo` is required (`129` without it).

- **The repository on the server:** created first (with the server's default settings and no policy) when it does not exist and
  the server lets the caller create it; an existing repository must be empty, and one with history is refused, because the
  command never merges histories.
- **Kept:** every local branch with its commits (author, committer, times, parents, message), trees and file contents, and the
  files on disk as they are. A SHA-256 git repository keeps every id. The server stores SHA-256, so a SHA-1 repository is
  rehashed: every commit, tree and file gets a new id, the parent links and trees follow, and all other bytes of a commit are
  kept, but a signature on a commit no longer verifies (it signed the old bytes). Commit ids therefore change for SHA-1 sources
  and the confirmation says so. The server records only messages that end with a newline, so a message without one (every
  GitHub squash or web merge) gets a final newline appended, which changes that commit's id even in a SHA-256 repository; an
  empty message is refused.
- **Branch names:** the server's default branch is `main`. The git branch called `main` becomes it; a repository with no
  `main` has its checked-out branch pushed as `main` (the message says so), and the other branches keep their names. A
  detached `HEAD` is refused.
- **Policy:** after the history, the starter policy `.pyn/pyn.toml` (`default = "shared"`, plus the `--exclusive <glob>`
  paths) is committed to `main` and written into the folder, which makes the matching existing files exclusive and read-only
  until locked. A repository whose history already tracks `.pyn/pyn.toml` keeps it, and `--exclusive` is then a usage error. An
  untracked `.pyn/pyn.toml` in the folder is sent instead of the starter policy. The history is sent before the policy, so
  nothing in it needs a lock; a server whose default rules make a path of the history exclusive refuses the move for that reason.
- **How:** everything the server would refuse is checked first, and a refusal lists the commits and changes nothing, on the server
  or in the folder: a submodule entry, a merge of more than two parents, an empty commit, a commit that changes more than
  10000 files, a tree over 100000 entries, a commit the server cannot store byte-for-byte (not in git's canonical form, or not
  UTF-8, or empty), more than one root commit, a shallow clone, and an operation in progress (merge, rebase, cherry-pick, revert, bisect).
  Then every file is uploaded, and each branch is pushed in batches inside the push limits. A commit merged into a branch whose
  branch is gone is pushed through a temporary branch, deleted at the end.
- **The folder:** only after every branch is on the server does the command write `.pyn/local_only/config.toml` (server and
  repository), the revision each file is at, the branch the workspace is on, and `repo_tool = "pyn"`, last, which is what turns
  the folder into a pyn home. If the folder cannot be set up it stays a git home. If the server refuses midway, the repository
  stays on the server partly filled; delete it or convert into another name. If nothing reached a repository this run created
  (a refusal while uploading or pushing the first branch), it is deleted again.
- **`.git` is kept**, untouched, and ignored by pyn (a line in `.pyn/local_only/ignore`). Remove or move it when it is no longer
  needed; `pyn convert --home git` refuses while one exists.
- **Not carried over:** tags (the server has none; they stay in `.git`), stashes, the index (staged changes), hooks, remotes and
  other git configuration. Files tracked through Git LFS arrive as their pointer files, with a warning. Files whose content on disk
  differs from the committed content (line-ending or filter conversions) show as modified in `pyn status`.

### Ignored files

`status` honors git's ignore rules in full: every `.gitignore` from the work tree root down (a deeper file wins),
`.git/info/exclude`, and the global excludes file (`core.excludesFile`, else `$XDG_CONFIG_HOME/git/ignore`, else
`~/.config/git/ignore`). Patterns follow git's own matching: negation, folder-only patterns, `**`, character classes,
escapes and `core.ignoreCase`. A file inside an ignored folder cannot be re-included, and a tracked file stays tracked
whatever the rules say.

### Attributes and line endings

`pyn-gitwork` has one attributes engine, shared by every command that needs it. It reads the global attributes file
(`core.attributesFile`, else `$XDG_CONFIG_HOME/git/attributes`, else `~/.config/git/attributes`), every `.gitattributes` from the
work tree root down to the path's folder (a deeper file wins, and a later line wins within a file) and `.git/info/attributes`
(which wins over all of them). Patterns follow git: a pattern without a slash matches names at any depth below its file,
others are anchored to it, `**` and character classes work, `core.ignoreCase` folds case, negative (`!`) and folder-only
(`dir/`) patterns are ignored, and a symbolic link named `.gitattributes` is not read. Attribute states are set, unset
(`-name`), reset (`!name`) and valued (`name=value`); `[attr]` macros are defined in the top-level `.gitattributes`,
`info/attributes` and the global file only, and the built-in `binary` macro is `-diff -merge -text`. The system-wide
attributes file is not read (its place depends on how git was built).

Conversion follows git's `convert.c`. `text`, `text=auto`, `eol`, the legacy `crlf`, `core.autocrlf` (`true`, `input`),
`core.eol` and `core.safecrlf` (`true`, `warn` by default, `false`) decide what `add` stores and what a checkout writes:
CRLF becomes LF on the way in, LF becomes CRLF on the way out when the effective end of line is CRLF, `auto` leaves binary
files (a NUL, a lone CR or mostly non-printable bytes) and files that already mix endings alone, and an index blob that
already holds carriage returns is not converted when an auto path is added again. `ident` expands `$Id$` to
`$Id: <blob id> $` on checkout and collapses it on `add`. With `core.safecrlf` set to `true`, `add` of a file whose line
endings would not survive a round trip fails with `fatal: LF would be replaced by CRLF in <path>` (or the CRLF form) and
exit `128`; with `warn` it prints `warning: in the working copy of '<path>', ... will be replaced by ... the next time Git
touches it`. Warnings are only printed when something is written to the object store.

These apply to `add`, `commit -a`, `stash`, `status`, `diff`, `ls-files -m`, `add -p`, `rm` and `mv` (through the index
comparison), and to every command that writes work tree files (`switch`, `checkout`, `restore`, `reset --hard`, `merge`,
`cherry-pick`, `revert`, `rebase`, `stash apply`, `clone`, `pull`), including conflict files. A `.gitattributes` that a checkout
brings in applies to the files written after it, and to itself. `status` and `diff` compare the converted work tree file with the
index, so a repository that relies on conversions shows no phantom changes. `-diff` makes a file binary in `diff` and `diff` forces text.

Not built: `working-tree-encoding`, `-X renormalize` and `add --renormalize`,
`export-ignore`/`export-subst`, the `merge` and `whitespace` attributes, `diff=<driver>` and `textconv`, and `.gitattributes` read
from a tree rather than the work tree (bare repositories, `--source`). `core.autocrlf` and the attributes are not applied to
`diff --no-index`. Covered by `crates/pyn-cli/tests/git_attributes.rs`, which compares blob ids, work tree bytes and `status`
and `diff` output with git (#273).

#### Filter drivers

`filter=<name>` runs the commands of `filter.<name>.clean`, `smudge`, `process` and `required` where git applies them:
`clean` when content goes into the repository (`add`, `commit -a`, `stash`, and in `status` and `diff` to compare the work
tree file with the index), `smudge` when a command writes a work tree file (`switch`, `checkout`, `restore`, `reset --hard`,
`merge`, `stash apply`, `clone` and the rest of the list above). The order is git's: `clean` sees the raw file and its result
then goes through the line-ending and `ident` conversion, while `smudge` sees the already converted content. A driver exists
only when some `filter.<name>.*` key is set; an attribute naming an unconfigured driver changes nothing. A command is run by
`sh -c` in the work tree root with the content on standard input; `%f` in it is the quoted path and `%%` is `%`. When
`process` is set it is used and `clean` and `smudge` are ignored. The long-running protocol is git's: one process per
command line and per pyn invocation, the `git-filter-client`/`git-filter-server` version 2 handshake, pkt-lines of up to
65516 bytes, `command=` and `pathname=` per file, and a final `status=` (`success`, `error`, or `abort`, after which the
process is not asked again). A failing command, a non-zero exit, `status=error` or a missing command for the direction leaves
the content unfiltered with `error: external filter ... failed`, unless `filter.<name>.required` is true, which makes it
`fatal: <path>: clean filter '<name>' failed` (or `smudge`) with exit `128`; for a required driver with no command it is fatal
too, as in git.

Not built: the `delay` capability (pyn does not advertise it, so a filter never delays a file), the `ref`, `treeish` and `blob`
metadata that git sends with `smudge` requests, filters in `apply` and the patch-mode commands that write the work tree
(`checkout -p`, `restore -p`), which stop for such a path (see [What pyn will not write](#what-pyn-will-not-write)),
`-X renormalize` through a filter, and `restore` rewrites every matching path rather than only the changed ones, which a
`smudge` that does not invert `clean` makes visible. Covered by `crates/pyn-cli/tests/git_filters.rs`, which compares blob
ids, work tree bytes, `status`, `diff` and exit status class with git (#628).

### Pathspecs

One matcher serves every command that takes paths (`add`, `rm`, `restore`, `checkout`, `reset`, `stash push`, `clean`,
`status`, `diff`, `log`, `show`, `shortlog`, `bisect start`, `grep`, `ls-files`, `ls-tree`), as git's pathspec matching does,
and it is the one a pyn server or local-store workspace uses for `add`, `rm`, `status`, `diff` and `restore --staged`.

- **Plain paths.** A file, or a folder with everything under it. A trailing slash names a folder only (`a.txt/` matches no
  file `a.txt`). `.` and `..` are resolved against where you stand, and a path outside the repository is
  `fatal: <arg>: '<path>' is outside repository at '<root>'` (`128`), as is an empty string
  (`empty string is not a valid pathspec. please use . instead if you meant to match all paths`). An absolute path inside
  the work tree is taken from the root.
- **Wildcards.** `*`, `?`, `[...]` and a backslash escape. In a plain pathspec a `*` also matches `/` (`src/*.rs` takes
  `src/deep/a.rs`), and a name that exists literally still matches itself. The text up to the first wildcard, and the folder
  you stand in, are never read as patterns.
- **Magic.** `:(top)` (`:/`), `:(literal)`, `:(glob)` (a `*` stops at `/`, and `**` crosses folders as a whole path segment),
  `:(icase)` (ASCII letters; inside a bracket class git keeps the members as written), `:(exclude)` (`:!`, `:^`), combined
  as `:(top,exclude)`. Only excludes means everything else (the working folder alone for `clean`, `grep`, `ls-files` and
  `rm`). A bad magic is `Invalid pathspec magic '<name>' in '<arg>'`, `Missing ')' at the end of pathspec magic in '<arg>'`,
  `Unimplemented pathspec magic '<c>' in '<arg>'`, or `<arg>: 'literal' and 'glob' are incompatible` (`128`).
  `ls-tree` refuses the magic git refuses there (`<arg>: pathspec magic not supported by this command: 'attr'`), and so does `add` on a pyn server.
- **Settings.** `GIT_LITERAL_PATHSPECS`, `GIT_GLOB_PATHSPECS`, `GIT_NOGLOB_PATHSPECS` and `GIT_ICASE_PATHSPECS` are read as
  git reads them (`bad boolean environment value`, and the `incompatible` pair messages, are `128`). On a pyn server they
  are not read.
- **Reporting.** A pathspec that selects nothing is git's per-command error (`pathspec '<arg>' did not match any files` for
  `add` and `rm`, `error: pathspec '<arg>' did not match any file(s) known to git` for `restore`, `checkout`, `stash` and
  `ls-files --error-unmatch`); an exclude is never reported. Like git, a command that reports unmatched pathspecs stops
  matching a pathspec that has named a path exactly (`a*c` taken for the file `a*c` leaves out `abc`).
- **Attributes.** `:(attr:...)` selects by the attributes of the [attributes engine](#attributes-and-line-endings): `name` (set),
  `-name` (unset), `!name` (unspecified) and `name=value`, several separated by spaces (all must hold). Like git, a value is
  letters, digits, `,`, `-` and `_` (a backslash quotes any character), and only one `attr:` is allowed. The attributes are
  those of the work tree, also for `log` and `show` on history. An untracked folder that fails the requirement is not entered
  (`status`, `clean`, `ls-files -o`), but an ignored one is, as in git. Errors use git's words (`attr spec must not be empty`,
  `invalid attribute name <name>`, `cannot use '<c>' for value matching`, `Only one 'attr:' specification is allowed.`).
  `ls-tree` refuses it. `add` and `stash push` read it as git 2.55 does; git 2.43 refuses it there, and pyn follows 2.55.
  `builtin_objectmode` is not built.
- **From a file.** `--pathspec-from-file=<file>` (`-` for standard input) with `--pathspec-file-nul` on `add`, `rm`, `reset`,
  `restore`, `checkout` and `stash push`: one pathspec per line (a line may be C-quoted, and may end in CR), or NUL-separated
  and unquoted. Pathspec arguments with it, `--pathspec-file-nul` without it, and `--patch` / `--interactive` with it are
  `fatal` (`128`) with git's words; a blank line is an empty pathspec. Paths in the file are relative to where you stand.
- **Symbolic links.** `add` refuses a pathspec with a symbolic link on the way (`pathspec '<arg>' is beyond a symbolic link`, `128`).

Not built: `prefix:N` magic (git aborts on it), for `clean` a `:(glob)` pattern that names folders (it ends in a slash:
usage error, `129`), `commit --pathspec-from-file`, and the `GIT_*_PATHSPECS` settings on a pyn server. On a pyn server `log`
sends a literal path to the server, `update`, `lock` and `restore <path> <revision>` take exact paths, and `mv` and
`commit <paths>` stay literal: they need a server contract change to take patterns.

Names in `diff --name-only`, `--name-status`, `--raw`, `--stat`, `--summary` and `log` / `show` with them are quoted as git
quotes them (`core.quotePath`), and left as they are with `-z`.

### Commands

| Command | On a git home |
| --- | --- |
| `status` | Also `-s`, `--porcelain` and `-b` in git's short format, and pathspecs (see [the short format](#the-short-status-format)). Header (`On branch main`, or `HEAD detached at <id>`), a `STAGED`, `UNSTAGED`, `PATH` table, and a count line; `nothing to commit, working tree clean` when clean. States are `added`, `modified`, `deleted`, `typechange`, `untracked` and `unmerged`. Staged is `HEAD` against the index, unstaged is the index against the work tree, so a path can be in both. A folder with nothing tracked in it is listed once, as `dir/`. Paths are from the repository root. Mode changes count as modified; symlinks are compared by their target. |
| `diff` | Index against the work tree; one commit against the work tree (the paths in the commit or the index); two commits, or `a..b`. A name that is both a revision and a file needs `--`. Shows `old mode` and `new mode`, symlinks and binary files as git does. `--cached` and `--staged` compare the index with `HEAD` (or with one named commit). |
| `show [<rev>]` | Commit header and message, then the diff against the first parent; a merge commit prints git's dense combined diff (see [Merge commits in `show` and `log`](#merge-commits-in-show-and-log)). |
| `log [<options>] [<rev>...] [--] [<path>...]` | Git's output: commit blocks, newest first across all parents, with git's history simplification for a path; revision ranges, formats, filters and diff output are in [Log on a git home](#log-on-a-git-home). `--commits` is the pyn table. `--filter` fails. |
| `grep [<options>] [-e] <pattern> [<rev>...] [[--] <path>...]` | git's output over the work tree, the index (`--cached`) or revisions; see [Grep](#grep). |
| `bisect <subcommand>` | git's binary search, output and state; see [Bisect](#bisect). |
| `blame`, `annotate` | git's output for the line history of a file, following renames and merges; see [Blame](#blame). |
| `files`, `ls` | `files` lists the index and `ls` lists `HEAD`'s tree. Every entry is `shared`, with no revision, lock or last change. |
| `add`, `restore --staged`, `commit` | See [staging](#staging-and-committing). |
| `rm`, `mv` | See [Removing and moving](#removing-and-moving). |
| `branch`, `switch`, `checkout`, `restore` | See [Branches, switching and the work tree](#branches-switching-and-the-work-tree). |
| `reset` | See [Reset](#reset). |
| `merge` | See [Merging](#merging). |
| `rebase` | See [Rebase](#rebase). |
| `tag` | See [Tags](#tags). |
| `stash` | See [Stashing](#stashing). |
| `clean` | See [Clean](#clean). |
| `submodule` | See [Submodules](#submodules). |
| `reflog` | See [Reflogs](#reflogs). |
| `rev-parse`, `ls-files`, `ls-tree`, `cat-file`, `show-ref`, `shortlog`, `describe`, `name-rev` | See [Inspection commands](#inspection-commands). |
| `verify-commit`, `verify-tag` | See [Signing](#signing). |
| `sparse-checkout` | See [Sparse checkout](#sparse-checkout). |
| `format-patch`, `apply`, `am` | See [Patches](#patches). |
| `init` | See above. `pyn init` starts a pyn repository; `pyn init-git` starts a git one. |
| `login`, `logout`, `register`, `password`, `key`, `token`, `user`, `org`, `team`, `repo`, `usage`, `setup`, `admin`, `whoami`, `member`, `role`, `invite` | Run as anywhere else; see [Refused commands](#refused-commands). |
| `lock`, `unlock`, `locks`, `policy`, `audit`, `verify`, `summary` | Refuse; see [Refused commands](#refused-commands). |
| `config`, `clone` | Unchanged: they do not depend on the repository's home. A clone from a pyn server made inside a git repository is its own pyn workspace; a clone from a git URL is its own git repository. |

Revisions are `HEAD`, `@`, a branch, a tag (annotated tags are followed to their commit), a full or abbreviated id of 4 or
more digits, and any of `~N`, `^` and `^N` after one. Reflog forms (`HEAD@{2}`, `@{-1}`, `@{upstream}`, `@{push}`) are
described in [Reflogs](#reflogs). SHA-1 and SHA-256 repositories are both read. Dates print in the
offset recorded in the commit, as git does.

On a git home, recorded submodules are inspected read-only for `status` and `diff`: a different submodule `HEAD`,
tracked edits (including staged edits), and untracked content are distinguished. Short status uses `M`, `m` and `?`
respectively; porcelain and the default table report these as modified. Dirt in recorded nested submodules keeps
its tracked or untracked category at the enclosing submodule. An uninitialized directory is clean;
a missing checkout is deleted. An initialized checkout with an unborn `HEAD` is inspected for staged and untracked
content; its diff uses the recorded gitlink ID. A submodule's files are not listed as superproject files.

On a git home, gitlink diffs use full-hex `Subproject commit <id>` lines, with `-dirty` for visible worktree dirt.
`--cached` compares recorded commits only. `submodule.<name>.ignore` in git config overrides the setting in the
worktree's `.gitmodules` (falling back to the index, then `HEAD`, when that file is absent), then
`diff.ignoreSubmodules`, then the command default (status includes untracked
content; diff ignores untracked-only content, including in nested submodules). `none`, `untracked`, `dirty` and `all`
select what is ignored; staged gitlinks remain visible in status even under `all`. `status` and `diff` take `--ignore-submodules[=<when>]` (a bare flag is `all`), which beats every configured setting and, under `all`, also hides staged gitlinks, as in git; a bad value is `fatal: bad --ignore-submodules argument: <value>` (`128`). Creating and updating submodules is `pyn submodule`, see [Submodules](#submodules). `diff --submodule` is not built.
With no worktree `.gitmodules`, an unmerged index entry for that file disables the recorded configuration fallback.

Differences from git, all deliberate or not yet done:

- Line endings and `ident` are converted as git does (see [Attributes and line endings](#attributes-and-line-endings));
  `working-tree-encoding` is not applied.
- `log` and `show` print no color on a terminal.
- A nested repository that the index does not track is one untracked `dir/`.
- `merge` and `stash apply` write diff3 conflict markers whatever `merge.conflictStyle` says and `stash apply` merges only when
  there is one merge base; `stash apply` does not write `AUTO_MERGE`; `switch` and `checkout` print their own wording where git's differs (`Fast-forward` without a diffstat, the status
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
%ar %cn %ce %cd %cD %ci %cI %cs %ct %cr %s %b %B %d %D %n %%`, and the mailmap forms `%aN %aE %al %aL %cN %cE %cl %cL` (see [Mailmap](#mailmap)); `%gd %gD %gs %gn %gN %ge %gE` are empty, as in git without `-g`; any other `%` sequence is printed as written. `%h`, `%t` and `%p`
use the git home's abbreviation policy described below. `%d` and `%D` list `HEAD -> <branch>` (or `HEAD`), then the other
branches, remote-tracking branches, `tag: <name>` and `refs/stash` that point at the commit, in git's order. `--date=` takes `default`, `iso`, `iso-strict`, `rfc`, `short`,
`relative`, `unix` and `raw` and applies to `Date:` and `%ad`/`%cd`; commit dates print in the offset recorded in the commit. `show` takes the same formats and `--date=`.

On a git home, abbreviated ids (including patch headers) lengthen until unique across loose objects, packs and
recursive alternates. The minimum defaults to at least seven, estimated from packed object counts;
`core.abbrev=auto` or unset selects it. Numeric `core.abbrev` accepts decimal, octal, hexadecimal and binary
`k`/`m`/`g` suffixes. Values below four, malformed or missing values and integer overflow fail;
valid integers above the hash width (`999`, `1k`, `64` for SHA-1) are accepted and render full ids; git 2.43 rejects them,
and pyn deliberately does not. A bad value is reported only by commands that read it;
`remote` (without a fetch), `prune` and `prune-packed` never do, so they succeed where git does.
`no`, `off`, `false` (case-insensitive) or an empty value selects full ids. Duplicate objects do not extend
prefixes; null patch ids also lengthen for uniqueness. The `index` line of a combined diff and the `(from <parent>)` label of
`-m --oneline` follow the same policy; a combined diff prints whole ids with `--full-index` and, as git does, ignores `--abbrev=<n>` and `--no-abbrev`. Server-home conventions are unchanged.

Filters apply in this order: `--author` and `--committer` (regular expressions on `Name <email>`, any of several of the same
kind), `--grep` (on the whole message; any of several, or all with `--all-match`, or none with `--invert-grep`), `-i` for all
three, `--since`/`--after` and `--until`/`--before` (on the committer date), `--merges` and `--no-merges`, then `--skip`, `-n`,
and `--reverse` last (it reverses the commits that were selected). The different kinds must all match. Regular expressions are
POSIX basic ones with GNU's `\|`, `\+` and `\?`, matched line by line; back-references are not supported. A date is a Unix
time (`1700000000` or `@1700000000`), `YYYY-MM-DD` with an optional `HH:MM[:SS]` and zone, an RFC 2822 date, `now`,
`yesterday`, or `<n> <unit> ago` (units from seconds to years, combinable). As in git a bare date takes the current time of day,
and a date without a zone is in the local timezone.

Output after each commit: `--stat[=<widths>]`, `--shortstat`, `--raw`, `--dirstat`, `--name-only`, `--name-status` and `-p`/`--patch`/`-u`, with `-U<n>` (which
implies `-p`) for the context. `--stat` and `--shortstat` combine with `-p` (git prints `---` between the message and the stat
when `--stat` and `-p` are both given); the other combinations are a usage error. The diff is against the first parent, or
against the empty tree for a root commit, limited to the paths. A merge commit prints none unless `-m`, `-c`, `--cc`,
`--first-parent` or `--diff-merges` asks for one (see [Merge commits in `show` and `log`](#merge-commits-in-show-and-log)). The
engine is the one `pyn diff` uses, including its `--stat` layout and the options of `diff` that shape a patch
(`-b`, `-w`, `-W`, `--word-diff`, `--color`, `--no-prefix`, `-R`, `--full-index`, ...); `--diff-filter`, which also selects commits, is a usage error (`129`).

`-<n>` is the same as `-n <n>`. `--all` starts from `HEAD` and every ref (`refs/`, including tags, remote-tracking branches and the
stash), together with any revisions named. `--first-parent` follows only the first parent of each commit, with git's
simplification when a path is given, and shows a merge's diff against its first parent. `--decorate[=short|full|auto|no]` (and git's boolean spellings;
`--no-decorate`, the last of the two wins) names the refs at a commit after its id in `--oneline`, `oneline`, `short`, `medium`,
`full`, `fuller` and `raw`; `%d` and `%D` always do, in the `full` form (`refs/heads/main`) under `--decorate=full`. Without a flag
`log.decorate` decides (`short`, `full`, `auto`, `no`, git's booleans; a bare key is true, an unknown value is off) and
otherwise `auto`: decorate when standard output is a terminal, so captured output has none. `auto` also decides for `--decorate=auto`.
`--pretty=raw` is decorated only when a `--decorate` flag asks, as in git. `show` takes the same flags and config. A pyn server
refuses them (`129`). `--decorate-refs=<pattern>` names only the refs that match, `--decorate-refs-exclude=<pattern>` and
`log.excludeDecoration` (repeatable; ignored when `--decorate-refs` is given) leave refs out; they also filter `%d` and `%D`. A
pattern without `refs/` gets it prepended (`tags/*` is `refs/tags/*`; `HEAD` stays), a glob uses `wildmatch` and any other pattern
also matches the refs below it (`refs/heads/feature` matches `refs/heads/feature/x`). `HEAD -> <branch>` needs both to pass.
`--source` is not built.

`--follow -- <path>` needs exactly one path and walks the whole history, dropping the commits that do not touch the tracked
path; when a commit adds it, git's rename detection (identical content, then at least half similar, see `pyn diff`) names the
old path and the walk goes on with that (`-M<n>` sets the similarity it needs and `-l` the limit; renames are tracked even with `--no-renames` or `diff.renames=false`, as in git). Merges are dropped (unless `--first-parent`). With diff output (`-p`, `--stat`,
`--name-only`, `--raw`, ...) each commit shows only the followed path, as a rename when the commit moved it. `-S`/`-G`, copy detection (`-C`) and `-B` with `--follow` are usage errors (`129`). `-S<string>` keeps the commits that change the number of occurrences of the
string in a file, `-G<regex>` those with an added or removed line matching an extended regular expression (never binary
files); `-i` makes both ignore case. Both look at each file after rename detection, drop merges (unless `--first-parent`) and
print only the matching files with a diff option. `-n` counts the commits that were kept, while `--skip` counts the ones the
walk selected (as git). `-S` and `-G` together fail (`128`).

`--graph` draws git's graph without colors in front of every line of every format, and orders the commits topologically (no commit
before all of its children, as `--topo-order`), as git does. It works with revisions, `--all`, `--first-parent`, `--decorate`,
`--skip`, `-n`, paths, diff output (every diff line carries the graph), `--author`, `--committer`, `--grep`, `--merges`,
`--no-merges`, `--since`, `--until`, `-S` and `-G`. With a path, a commit's parents are rewritten past the commits that changed
nothing (and `-p` diffs against the rewritten parent), and a parent is drawn only when the walk would show it, so the filters
leave no dangling lines; `-S` and `-G` leave a `...` row where a commit they drop was drawn. It fails (`128`, as git does)
with `--reverse`; `--follow` and the merge diff options (`-m`, `-c`, `--cc`, `--remerge-diff`, ...) are usage errors (`129`).

`-E` and `-F` change how `--grep`, `--author` and `--committer` read their patterns: extended regular expressions (the
operators `( ) | + ? { }` without backslashes) or fixed strings; `--basic-regexp` restores the default, and the last of
the three wins. `-G` is always extended. Back-references fail in both regular expression dialects.

On a git home, `log` and `show` accept `--abbrev[=<n>]` and `--no-abbrev`. A numeric width overrides
configuration and clamps to the valid range; a bare flag retains the configured default. Repeating `--abbrev`
uses its last numeric or bare occurrence. The revision parser accepts a signed decimal prefix (`12junk` selects
twelve, `-1` full width); `no` and `0` select four. `--no-abbrev` selects full commit ids while patch index ids
keep the configured/default width; an explicit numeric width applies to both.

Differences from git:

- With both a revision range that excludes history and a path, the choice of merge commits whose parents are on the excluded
  side can differ from git's, which decides it while the walk is still marking commits as excluded. Ranges alone, and paths
  alone, match.
- `--since` and `--until` only filter; git also stops walking at the first commit older than `--since`, which differs only for
  histories whose commit dates are not in order. A date git cannot read is a failure here (git takes `now`).
- `%d` does not hide `refs/notes/*` or other namespaces: they print by full name. Unlisted placeholders (`%C`, `%w`, `%<`,
  `%x`) are printed as written.
- `--format` and `--pretty` win over `--oneline` whatever their order. `--date=local`, `human` and `format:` are not taken.
- `log --stat` takes the width forms of `diff --stat`; the graph is scaled to the terminal width as in `diff`.
- Passing a revision that is not a commit, a dated reflog selector (`HEAD@{yesterday}`) or `:/<text>` fails with exit `128`
  where git accepts them.

### Merge commits in `show` and `log`

A merge commit's diff follows git's merge-diff options. The last one given wins:

| Option | Merge shows |
| --- | --- |
| `--cc`, `--diff-merges=cc` (also `dense-combined`) | A dense combined diff, `diff --cc <path>` with `@@@` hunks: the paths that differ from every parent, leaving out the hunks where the result took one parent's text. The default of `show`. `--cc` also implies `-p` in `log`. |
| `-c`, `--diff-merges=c` (also `combined`) | The same without the omissions (`diff --combined <path>`). `-c` implies `-p` in `log`. |
| `-m`, `--diff-merges=m` (`on`, `separate`) | One ordinary diff per parent, each under its own copy of the header, `commit <id> (from <parent>)` (`<abbrev> (from <abbrev>) <subject>` with `--oneline`; none with a `--format` template). `-m` and `on` use `log.diffMerges` (`separate` when unset). `-m` alone prints no diff in `log`. |
| `--first-parent`, `--diff-merges=first-parent` (`1`, `--dd`) | A diff against the first parent only. `--first-parent` chooses this unless one of the options above is given, in `show` and in `log -p`. |
| `--remerge-diff`, `--diff-merges=remerge` (`r`) | The merge's result against a fresh merge of its two parents, conflict markers included, so only what the committer changed while resolving shows. `--remerge-diff` implies `-p` in `log`. See below. |
| `--no-diff-merges`, `--diff-merges=off` (`none`) | Nothing: the default of `log`. |

`log` shows a merge's diff only when asked, and `--diff-merges=<mode>` alone shows merges (and no other commit) with the patch;
`-c`, `--cc` and `--dd` show every commit's diff. `show` prints a merge's header, then a blank line, then the diff, even when
the combined diff is empty (a clean merge) and with `--oneline`; `-s` and an empty `--format=` have no blank line. The combined layout is a port of git's:
two-column `+ `, `- `, ` +`, `++` prefixes, `index a,b..c`, `mode a,b..c` and `new file mode` lines, `Binary files differ`
for a binary file, one hunk comment of at most 40 bytes after the `@@@` line, and octopus merges with a column per parent.
Paths are the ones every parent differs from, after the same rename detection as `pyn diff` (`-M`, `-C`, `--no-renames`
and `-l` apply per parent); `-U<n>` sets the context. `--stat`, `--shortstat`, `--numstat` and `--summary` describe the
result against the first parent, and `--name-only` and `--name-status` list the combined paths (`--name-status` with a
letter per parent, `MM`); with a patch, a blank line separates them from it. `--stat -p` in `log` has no `---`.

A remerge diff merges the two parents again with the tree merge of `pyn merge` (markers labelled `<abbrev> (<subject>)`, git's
two-sided style unless `merge.conflictStyle` is `diff3`; `zdiff3` is a usage error) and diffs the result against the merge commit as
any other diff; each conflicted path gets `remerge CONFLICT (...)` (and `remerge warning:` for a binary file) lines after its
`diff --git` line, and a path whose resolution equals the fresh merge still gets the header and those lines. An octopus merge prints
`diff: warning: Skipping remerge-diff for octopus merges.` in place of its diff. Differences: renames are not followed when merging
(so a merge git resolves through a rename shows a conflict), a merge with several merge bases is a usage error (`129`) where git
builds a virtual base, and the blobs of the fresh merge are written to the object store (unreferenced).

Not built, each a usage error (`129`): `-w` and `--check` with a combined diff; a combined diff of a submodule; a merge of more than 62 parents; `-c`, `-m`
and the other merge options with `log --graph`, `--follow`, `-S` or `-G`; `log -m`, `--diff-merges=separate` or
`--diff-merges=first-parent` with a path, which also turns history simplification off. `--combined-all-paths` is an unknown
option. An invalid `--diff-merges` value fails (`128`) as in git. `-m` and `--first-parent` diffs are `pyn diff`'s per parent,
so they carry its alignment, which is the xdiff port's and matches git's.

A commit message is printed with tabs expanded to multiples of eight columns in the `medium`, `full` and `fuller` formats (the text is
expanded before the four-space indent, as git does); `--expand-tabs[=<n>]` sets the width for any format and `--no-expand-tabs`
(or `--expand-tabs=0`) keeps tabs. `--abbrev-commit` abbreviates the id on the `commit` line (and in `--pretty=oneline`);
`log.abbrevCommit` is not read.

### Rename and copy detection

On a git home `diff`, `show`, `log` and `stash show` pair a deleted file with an added one the way git's diffcore does, so
their output matches git's byte for byte: `similarity index N%` with `rename from` and `rename to` (or `copy from` and `copy to`)
in a patch, `{old => new}` names in `--stat` and `--numstat`, ` rename old => new (N%)` in `--summary`, and `R<nnn>` or `C<nnn>`
in `--name-status`. A rename with no content change prints only its header. A mode change prints `old mode` and `new mode`
first. The order of the steps is git's:

1. Files with identical content, preferring a source with the same name, then a source no rename has used yet.
2. Sources and added files whose name (after the last `/`) is unique on each side, when they are at least 75% alike (halfway
   between the threshold and 100%).
3. Every remaining pair, scored as git scores it (lines up to 64 bytes hashed into spans, with the share of the larger file's
   bytes the two have in common), best first; a source is used once. Files that differ in size by more than the threshold, and
   anything but regular files, are not compared.

The threshold is 50%; `-M<n>` or `--find-renames=<n>` sets it (`-M90%`, `-M9`, `-M.5`; a score of 0 is the default), and the
last of `-M`, `-C` and `--no-renames` wins. Detection is on by default (`diff.renames`), `diff.renames=copies` also finds copies,
and `false` turns it off. `-C` adds copies from files the change modified; `-C -C` and `--find-copies-harder` (and a bare `-C`
while `diff.renames=copies`) take every unmodified file in the compared trees as a source too, so a path filter also limits the
sources. A deleted source copied to several places is a rename for the last destination in path order and a copy for the others.
`-l<n>` and `diff.renameLimit` (default 1000) skip the exhaustive step when destinations times sources exceed the square of the
limit, with git's warning; under `-C -C` the unmodified sources are dropped first (git's "only found copies from modified paths").
A path filter limits the files before pairing, so a rename across it shows as a deletion or an addition. `log -S` and `-G` pair
with the detection the command line asks for before they filter. Paths with a quote, backslash, control or (under
`core.quotePath`) non-ASCII byte are quoted in `diff --git`, `---`, `+++`, `rename` and `copy` lines as in git, and a name with a
space is followed by a tab on its `---` and `+++` lines.

`-B[<n>][/<m>]` (`--break-rewrites`) looks for files that are mostly rewritten: a modified regular file of at least 400 bytes
whose removed plus added bytes are at least `n` (default 50%) of its size is split into a deletion and an addition, which
rename detection may pair with other files (a rewritten file can be where another file went, or the source of a rename). A
split pair nothing claimed is joined again; when at least `m` (default 60%) of the old content is gone it is a complete rewrite,
shown as `M<nnn>` (`--name-status`, `--raw`), ` rewrite <path> (N%)` (`--summary`), `dissimilarity index N%` and one hunk
removing every old line and adding every new one (`--patch`), with all lines counted in `--stat` and `--numstat`, and selected
by `--diff-filter=B`. Not built, each a usage error (`129`): `-B` with a combined diff, `--no-index`, color, `--word-diff`,
whitespace options, `--check` and `--dirstat`.

Git 2.43 does not count a file's final span when it has no newline (or 64 bytes) to end it, so a similarity score can differ from
git's by a few percent there; pyn follows git 2.55.

`status` reads `status.renames` (else `diff.renames`) and shows a staged rename as `R  old -> new` (`C` for a copy), and a rename
between the index and the work tree, which needs an intent-to-add file, as ` R`. `--renames` turns detection on (as renames, even
over `status.renames=copies`), `--no-renames` off, the last of the two wins, and `-M[<n>]` or `--find-renames[=<n>]` turns it on
when it is off, keeping `copies`, and sets the threshold (text after the score is ignored; 0 is the default). `status -C` is a
usage error, as in git.

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

A pathspec decides how an untracked folder shows, as in git: `dir/` when the pathspec takes the folder whole and a
selected file lies in it, the files or folders inside when it names only part of it. Pathspecs are git's, see
[Pathspecs](#pathspecs).

On a pyn server `STAGED` becomes `X` (`added` is `A`, `modified` `M`, `deleted` `D`) and the work tree state becomes `Y`
(`modified` is `M`, `deleted locally` `D`, `untracked` is `??`). A conflicted path is `UU`, or `AA` when both sides added it
(the server does not say which side deleted a path, so `DU` and `UD` are not used). States that are about the server and
not the work tree (`behind`, `new`, locks, local commits not yet published, modes) have no place in the short format and
print nothing. Lock state is shown in the default table's lock column and by `pyn lock`/`pyn locks`.
`-b` prints `## <branch>...origin/<branch>` with `[ahead N]` for local commits not yet published, and just
`## <branch>` for a workspace without a local store. Pathspecs follow the same rules as `add`.

### Staging and committing

`add`, `restore --staged`, `commit` and `diff --cached` work on git's own index, so git sees the same state and the two tools
can be mixed freely.

**Index.** `.git/index` of the work tree (a linked worktree has its own) is read and written through `pyn-git`. Versions 2, 3
and 4 are read and the version found is the version written (a new index is version 2); the entry stat data, the stage
bits, assume-valid, skip-worktree and intent-to-add flags are kept. The optional cache extensions (`TREE`, `UNTR`, `REUC`,
`FSMN` and the like) are not kept: git rebuilds them, at the cost of one slower first command. A split or sparse index (`index.sparse`) is
refused; the skip-worktree flag on ordinary entries is kept and used (see [Sparse checkout](#sparse-checkout)). The file is written through `index.lock` and a rename, as git does, and pyn first checks that nobody changed the index
while it was working; an existing `index.lock` stops the command.

**`add`.** Writes the blob of each file as a loose object and a stage 0 entry with the file's stat data, mode (`100644`,
`100755`, or `120000` for a symlink, whose target is the blob) and id. A tracked file that was deleted is staged as a removal.
`core.filemode=false` keeps the index's mode. Ignore rules are `.gitignore` in every folder, `.git/info/exclude` and the global
excludes file; a file or folder named on the command line that they hide is refused with git's message and exit status `1`
(the other paths are still staged) unless `-f` is given, and a hidden file met while walking a folder is skipped silently. A
nested git repository is skipped with a warning. An existing submodule is staged as a gitlink (`160000`) pointing to its
current `HEAD`; its files are not staged in the superproject. `.pyn/local_only` is never added. An unmerged
path is staged as the file is, which resolves it.

**`commit`.** A linear commit on the current branch:

1. It refuses, changing nothing, a detached `HEAD`, a merge, cherry-pick, revert, rebase or sequencer in progress,
   unmerged index entries.
2. The author and committer are `GIT_AUTHOR_*` and `GIT_COMMITTER_*` (name, email, date) when set, else `author.*` and
   `committer.*`, else `user.name` and `user.email`; without a name and email it stops. The time zone is the system's.
3. With `-a`, every changed tracked file is staged first, in memory; the index is written only once the commit is built.
4. The index becomes trees (written as loose objects), and the commit has `HEAD` as its only parent. A tree equal to the parent's
   is `nothing to commit` (exit `1`). The message gets git's whitespace cleanup: no trailing blanks, no leading or trailing blank
   lines, a final newline.
5. The index is written (only with `-a`), then the branch ref is moved through its `.lock` file after checking `HEAD` did not move,
   and the reflog lines are appended (see [Reflogs](#reflogs)).

Client hooks run as in [Client hooks](#client-hooks); `COMMIT_EDITMSG` is written only when a message hook runs. A commit made this way has the object id
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
  A pyn server workspace with the local store parses both the same way; only the author changes.
- `-s` and `--signoff` append `Signed-off-by: <committer name> <email>` after git's trailer rules: no blank line when the last
  paragraph is a trailer block, and nothing when the last line already is that line.
- `--amend` replaces the tip with a commit that has the tip's parents, author (name, email and date) and message, and the
  index as its tree; `-m` or `-F` replace the message, `--author` and `--date` replace the author fields. Without `-m`, `-F`
  or `--no-edit` the editor opens on the old message ([below](#the-commit-editor-on-a-git-home)). `GIT_AUTHOR_*` is ignored, as in git. The committer and its date are new, a `gpgsig`
  header is dropped and other headers are kept. It exits `128` with no commit to amend or during a merge; an amend that
  would leave the commit equal to its parent's tree exits `1` unless `--allow-empty` is given; amending a merge commit is allowed.
  The branch and `HEAD` logs get `commit (amend): <subject>`, and `ORIG_HEAD` is not written.
- `-S`, `--gpg-sign[=<keyid>]` (the key id attached or after `=`) and `--no-gpg-sign` (the last of the two wins) sign the commit;
  `commit.gpgsign=true` signs by default. See [Signing](#signing).
- `--allow-empty-message` accepts a message that is empty after cleanup, and stores it as zero bytes. `--cleanup=<mode>` is
  `strip` (also drops `#` lines), `whitespace` (the default without the editor, as are `default` and `scissors`) or `verbatim` (the
  message exactly as given, `-m` paragraphs joined by a blank line); it applies to `-m`, `-F`, `-C` and the editor (see
  [below](#the-commit-editor-on-a-git-home)), and an unknown mode is an error (`128`). `core.commentChar` is not read. A comment-only message is not empty under `whitespace`.
- `--reset-author` takes the author from now (and `GIT_AUTHOR_*`) instead of the commit's, and needs `--amend` or `-C` (exit `128`);
  it cannot be combined with `--author`.
- `--trailer <Token: value>` (or `Token=value`) adds a trailer after any `-s` line, joining the last paragraph when it is a trailer
  block, and is skipped when the last line is the same trailer. Only that default `interpret-trailers` behaviour is built: `trailer.*`
  configuration is not read, and an argument without a valid token is added as `<text>:`.
- `-C <commit>`, `--reuse-message=<commit>` takes the message and the author (name, email and date) from that commit; the committer
  is new. It combines with `-s`, `--trailer`, `--cleanup`, `--author`, `--date`, `--reset-author` and `--amend`, not with `-m` or `-F` (exit `128`),
  and an unknown commit exits `128`.
- `-q` prints nothing on success; `-n`/`--no-verify`, `--verify`, `-v`, `--status` and `--no-status` are accepted and ignored.
- The output is `[<branch> <id>] <subject>`, an ` Author:` line when the author differs from the committer, and pyn's
  changed-file count; it has no `Date:` line.

A pyn server refuses all of these except `-q`, `-n`, `--no-verify`, `--verify`, `-v`, `--status`, `--no-status`, `--no-edit` and `-F` with a
usage error (`129`), because its commits are immutable and `--file` there is the content file.

#### The commit editor on a git home

The editor opens when no `-m`, `-F` or `-C` gave a message, and also with `-e`/`--edit`; `--no-edit` (the last of the two wins)
keeps it closed. Its file is `COMMIT_EDITMSG` and starts with the message so far, then git's help lines and the `#` status lines:

| Message so far | Source |
| --- | --- |
| `-m`, `-F`, `-C` (only with `-e`) | The message, tidied unless `--cleanup=verbatim`. |
| `--amend` | The tip's message; the status lines add `Date:` (the author date) and, when the author is not the committer, `Author:`. |
| A merge in progress | `MERGE_MSG` with its `# Conflicts:` list, then git's "It looks like you may be committing a merge" note. |
| A pending `SQUASH_MSG` | The squashed log, no note. |
| None | Empty. |

`-s` and `--trailer` are added to that message before the editor opens (above a trailing `# Conflicts:` block), and a message left
with only `Signed-off-by:` lines is empty. The status lines list the staged, unstaged (`modified`, `deleted`, `typechange`) and
untracked paths (a folder holding nothing tracked is one `dir/`), and `Initial commit` or the merge note where git has them.
`--no-edit` with no message source is `Aborting commit due to empty commit message.` (exit `1`).

`--cleanup` decides what the editor leaves, and the help lines:

| Mode | Result |
| --- | --- |
| `default`, unset | `strip`. |
| `strip` | Drops `#` lines and tidies blanks. |
| `whitespace` | Tidies blanks only; the `#` lines stay, and the help says so. |
| `verbatim` | Keeps every byte. |
| `scissors` | Cuts from the scissors line down, then tidies; the template has the scissors line in place of the help. |

Without the editor `default` and `scissors` are `whitespace`. What stays different: the status paths are relative to the work
tree root, not the current folder; `status.showUntrackedFiles`, `status.displayCommentPrefix` and `core.commentChar` are not read;
`commit-msg` sees the message after cleanup, where git shows it the raw editor file; and a comment-only `-m` with no `--cleanup` is
still refused (`Aborting commit due to empty commit message.`), deliberately, where git commits it under some modes.

Not built, on either home: `-c`/`--reedit-message`, `-t`/`--template`, `--fixup`, `--squash`, `--dry-run`, `--short`,
`--branch`, `--long`, `--porcelain`, `-z`, `--ahead-behind`, `-u`/`--untracked-files`, `-i`/`--include`, `-o`/`--only`, `-p`/`--patch`,
`--interactive`, `-U`/`--unified`, `--inter-hunk-context`, `--pathspec-from-file`, `--pathspec-file-nul`, `--no-post-rewrite` and
`--post-rewrite`.

**Finishing a merge.** While `MERGE_HEAD` exists `commit` makes a merge commit: the merged-in commits are further parents, and
without `-m` the message is `MERGE_MSG` with its `#` comment lines removed (what git's editor flow leaves; `git commit
--no-edit` keeps them, so the `# Conflicts:` list stays in the commit and the id matches git's; git 2.43 and 2.55 agree on this, and `merge --continue` strips them). An empty merge is allowed, and unmerged entries are refused. `commit -a` stages a resolved file first.
`add` of an unmerged path stages the file as it is (or its removal) and clears the conflict stages. `commit` removes `MERGE_HEAD`,
`MERGE_MSG` and `MERGE_MODE`. A cherry-pick, revert or rebase in progress still stops it.

**`restore --staged`.** Replaces the index entries under the paths with `HEAD`'s (or `--source`'s), removing entries `HEAD`
lacks. The replaced entries carry no stat data, so git re-reads those files on its next status.

**`diff --cached`.** `HEAD` (or the named commit) against the index, in the same output as `diff`.

### Removing and moving

`rm` and `mv` change the index and the work tree together, as git does, and check everything before writing anything: a
refused run leaves both untouched. Paths are git pathspecs (see [Pathspecs](#pathspecs)); `mv` takes literal files and folders relative to where you stand.

**`rm`.** Takes every index entry under the paths, unmerged ones included. A path that matches nothing is `pathspec '<path>' did
not match any files` (`128`) unless `--ignore-unmatch` is given, which makes the command succeed when nothing matched at all; a
folder without `-r` is `not removing '<path>' recursively without -r` (`128`). Unless `-f` is given, a path is refused with
exit `1` when its entry differs from `HEAD` and from the file (`staged content different from both`), when only the entry
differs from `HEAD`, or when only the file differs from the entry (`local modifications`; a changed mode counts); `--cached`
only refuses the first case, an unmerged path is never refused, and a file already gone from the work tree is never refused.
An entry added with `intent-to-add` counts as staged. Without `--cached` the file and the folders it leaves empty are
removed, except the folder you stand in; a path that is now a folder is `Is a directory` (`128`) and a submodule is refused
unless `--cached`. `-n` prints `rm '<path>'` for each path and changes nothing, `-q` prints nothing.

**`mv`.** `mv <source> <destination>` renames; with several sources, or a destination that is an existing folder, each source
goes into the folder under its own name. A folder is renamed whole, untracked files in it included, and its tracked files
keep their index entries (ids, modes and stage 0) under the new names; the content in the index is not re-read, so staged and
unstaged changes stay as they were. Refused with `<reason>, source=<source>, destination=<destination>` (`128`): `bad source`,
`not under version control`, `conflicted` (an unmerged path), `destination exists` (also for a source moved onto itself, and
for a tracked destination missing from the work tree, unless `-f`, which still cannot replace a folder), `cannot move directory
over file`, `can not move directory into itself`, `source directory is empty` (no tracked file in it), `destination directory
does not exist` and `multiple sources for the same target`. `-k` skips such a source and carries on; `-n` and `-v` print `Renaming
<source> to <destination>` (`-n` renames nothing). A folder emptied by the move is left in place, as git leaves it.

**On a pyn server.** There is no index of committed content to rewrite, so `rm` stages the deletion of the file (or forgets a
file that was only staged) and `mv` renames the file and stages a deletion at the old path plus the file's current content at
the new one; moving a file back to its old name leaves nothing staged. The checks above run against the staged content and the
last synced revision. A path with a merge conflict is refused (`rm`) or `conflicted` (`mv`). An exclusive path, at the old path
and at a destination it replaces, needs its lock first and is refused before anything changes, with the wording and exit status
of `add` (`<path> is exclusive and you do not hold its lock`, `128`); a new exclusive destination is checked at `commit`, as for
`add`. `commit -a` keeps a deletion staged with `rm --cached`. Covered against the real git by
`crates/pyn-cli/tests/git_rm_mv.rs` and on a pyn server by `crates/pyn-cli/tests/rm_mv_server.rs`.

### Branches, switching and the work tree

`branch`, `switch`, `checkout` and `restore` write refs, the index and the work tree as git does, through the same lock files and
with the same reflog lines. `switch` and `checkout` take `--recurse-submodules` for the checked-out submodules, see [Submodules](#submodules).

**`branch`.** Lists (`*` marks the current branch, `+` a branch checked out in another work tree, `(HEAD detached at <id>)` first),
`-v` adds the abbreviated commit and subject, `--list <pattern>` filters with shell patterns, `--show-current` prints the name.
`branch <name> [<start>]` creates a branch at `HEAD` or the start point (any revision) with the reflog line `branch: Created from
<start>`, and refuses an existing name or one that clashes with a folder of refs. `-d` deletes a branch merged into `HEAD`, `-D` any
branch; both refuse a branch checked out in a work tree, and delete its reflog and its `[branch "<name>"]` configuration section
(the section is rewritten as plain text, nothing else in `config` changes). `-m [<old>] <new>` renames the ref and its reflog,
moves the configuration section and, for the current branch, points `HEAD` at the new name (a branch checked out in another work tree
is refused; git renames it and updates that work tree). `-M` (or `-m -f`) also replaces an existing branch, except one checked out in a work tree
(`cannot force update the branch '<name>' used by worktree at '<path>'`, `128`); the replaced branch's reflog is replaced and its
`[branch]` section is left as it was, as in git. `-c` and `-C` copy the ref, its reflog and its `[branch "<name>"]` section (the copy sits right
after the original), log `Branch: copied <old> to <new>`, and `-C` replaces an existing branch like `-M`. `-f` on a create resets an
existing branch (`branch: Reset to <start>`, nothing when it is already there); `-f -d` is `-D`.

`-r` and `-a` list the remote-tracking branches under `refs/remotes/` (`-r` as `origin/main`, `-a` as the local branches then `remotes/origin/main`,
and the detached `HEAD` line only with `-a`); a symbolic ref prints as `origin/HEAD -> origin/main`, and with `-v` the arrow is aligned
with the other names. Patterns (`--list <pattern>`) match the name without `remotes/`; a bare name with `-a` or `-r` is `fatal: the -a, and
-r, options to 'git branch' do not take a branch name.` (`128`). `-r -d` and `--contains` are not built.

`-v` adds `[ahead 1, behind 2]` (or `[gone]`) for a branch with an upstream, and `-vv` the upstream's name before it (`[origin/main: ahead 1]`, `[origin/main]` when level) and, for a branch checked out in another work tree, that tree's path in parentheses.

`-u <upstream> [<branch>]` (`--set-upstream-to`) writes `branch.<name>.remote` and `merge` and prints `branch '<name>' set up to track '<upstream>'.`
on standard output. A local branch as upstream is remote `.` with `merge = refs/heads/<name>`; a remote-tracking branch needs a
`remote.<name>.fetch` refspec that maps it, else `cannot set up tracking information; starting point '<ref>' is not a branch`. Naming the
branch itself only warns (`not setting branch '<name>' as its own upstream`), an unknown upstream is `fatal: the requested upstream branch
'<name>' does not exist` with git's hint (off with `advice.setUpstreamFailure`), and an unknown branch `branch '<name>' does not exist`
(all `128`). `--unset-upstream [<branch>]` removes both settings, or is `fatal: branch '<name>' has no upstream information` (`128`).
Covered against the real git (2.43 and 2.56) by `crates/pyn-cli/tests/git_branch_flags.rs`.

**Tracking a created branch.** `branch <name> [<start>]`, `checkout -b`/`-B`, `switch -c`/`-C` and the guessed branch above set up the
upstream as git does, writing `branch.<name>.remote` and `merge` (and `rebase`) and printing `branch '<name>' set up to track '<upstream>'.`
on standard output. The start point must resolve to a branch: a local one (remote `.`, `merge = refs/heads/<name>`) or a remote-tracking
one that a `remote.<name>.fetch` refspec maps (the first matching refspec of each remote; two remotes mapping the same ref is `fatal`,
`128`, `not tracking: ambiguous information`). `HEAD` and symbolic refs such as `origin/HEAD` count as the branch they point at; a
detached `HEAD`, a tag, a commit or a name that is not a ref set up nothing.

- `-t`, `--track` and `--track=direct` track the start point whatever `branch.autoSetupMerge` says, and a start that is not a branch,
  or a remote-tracking ref no refspec maps, is `fatal` (`128`, `starting point '<x>' is not a branch`). `--track=inherit` copies the
  start branch's `remote` and `merge` values instead (a warning and no tracking when it has none). `--no-track` sets nothing. The
  last of these on the command line wins. `checkout` and `switch` accept them with `-b`/`-c`/`-B`/`-C`; without one,
  `checkout -t origin/topic` and `switch -t origin/topic` create `topic` (the part after the remote's name) from it, and
  `--no-track origin/topic` does the same without tracking. A name with no `/` (`-t topic`), `--detach`, paths and more than one
  argument are `fatal` (`128`), as in git.
- `branch.autoSetupMerge`: `true` (the default) tracks a remote-tracking start only; `false` tracks only when asked; `always` also
  tracks a local branch (a tag or detached `HEAD` stay untracked); `inherit` copies the start branch's upstream; `simple` tracks a
  remote-tracking start only when the new branch is named as the remote branch. Any other value is `fatal` (`128`) before anything
  is created, as is a value of `branch.autoSetupRebase` other than `never` (the default), `local`, `remote` or `always`.
- `branch.autoSetupRebase` adds `branch.<name>.rebase = true` for a tracked local branch (`local`), a remote one (`remote`) or any (`always`),
  and prints `by rebasing`. A setting already in the section is replaced in place; an existing `rebase` stays unless set.
- A start point written `@{upstream}`, `@{u}`, `@{push}` or `@{-N}` (after a branch name for the first two) is the ref it names, so
  `branch n @{u}` tracks that remote-tracking branch as `branch n origin/main` does, and `@{-1}` (a local branch) tracks only with
  `--track`; one with no target is `fatal` (`128`). `-t`/`--no-track` with `-d`, `-D`, `-m`, `-c`, `-u`, `--unset-upstream`, `--show-current` or a listing is ignored, as in git, while
  `checkout -t -- <path>` is `fatal` (`128`). `-q`/`--quiet` on `branch`, `checkout` and `switch` drops the progress lines (`Switched to ...`, `Deleted branch`, `branch '<x>' set up to track ...`, `Updated N paths from ...`, the list of local changes, the detached `HEAD` advice) and keeps errors.
- Not built: on the pyn server and the local
  store the tracking flags are usage errors, and the pyn server refuses `-q`; other `@{...}` forms (`@{yesterday}`) are refused. Differences: git has already created the branch when it stops on an ambiguous mapping, and when a
  `checkout -b` or `switch -c` fails after creating the branch (local changes in the way) or a `post-checkout` hook fails, git leaves the
  `branch.<name>.*` settings written; pyn writes them after the switch, so neither leaves them behind.
Covered against the real git by `crates/pyn-cli/tests/git_branch_track.rs`.
 
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
- Unmerged index entries, and a merge, cherry-pick, revert or rebase in progress, refuse the command.
- `switch -C <branch> [<start>]` (`--force-create`) creates the branch or resets an existing one to the start point (default `HEAD`), logs
  `branch: Reset to <start>` (on `HEAD` alone when the branch is already there and current), and prints `Switched to and reset branch`,
  `Reset branch` (already on it) or `Switched to a new branch`. `switch --orphan <new>` points `HEAD` at an unborn branch and empties the index and the
  tracked files of the work tree (untracked files stay; local changes stop it unless `-f`), logs nothing in the `HEAD` reflog and prints
  `Switched to a new branch '<new>'`; it refuses a start point, an existing name, and being combined with `-c`, `-C` or `--detach`.
  `checkout -B <new> [<start>]` does what `switch -C` does, with checkout's wording for a bad start point (`'<x>' is not a commit and a branch '<new>' cannot be created from it`) and `Cannot update paths and switch to branch` for extra arguments; `-b` with `-B` is `fatal` (`128`). `checkout --orphan` is not built. On a pyn server `-C`, `-B` and `--orphan` are refused.
- `checkout <name>` and `switch <name>` guess a branch (`--guess`, `--no-guess`, `checkout.guess`, default on): when `<name>` is no local branch and
  names no revision, and exactly one remote maps `refs/heads/<name>` onto an existing remote-tracking ref, the branch is created at it with
  `branch.<name>.remote` and `merge` set and `branch '<name>' set up to track '<remote>/<name>'.` printed. Several remotes matching is refused
  (`128`, nothing changed) unless `checkout.defaultRemote` names one of them. `checkout` does not guess with `--detach`, with more than one argument or
  when a file of that name exists without `--` (that is `fatal`, `128`, as git). The guessed branch follows `branch.autoSetupMerge` and
  `branch.autoSetupRebase` as any created branch does (see below); the hint text for several matching remotes is not built.
- `switch -c <new> [<start>]` and `checkout -b` create the branch first (an unborn branch just points `HEAD` at the name),
  `switch -` and `checkout -` (also `@{-N}`) return to the previous branch (read from the `HEAD` reflog), `--detach` and `checkout <commit>` (or a
  tag) detach `HEAD`; `checkout --detach` alone detaches at the current commit. Entering from a branch prints git's advice
  (`advice.detachedHead=false` turns it off), except with `--detach`, and `HEAD is now at <id> <subject>`. `checkout -` returns to
  a detached position (the reflog holds its full id), but `switch -` and `switch <commit>` onto one are refused as in git
  (`a branch is expected, got commit`), as is `switch @{-N}`. Leaving a detached `HEAD` prints `Previous HEAD position was <id>
  <subject>`, or, when commits would be reachable from no ref any more, git's `Warning: you are leaving N commit(s) behind` with
  the newest four listed (` ... and M more.`) and the `git branch <new-branch-name> <id>` hint (off with the advice setting).
  `status` shows `HEAD detached at <id>`, `log` follows `HEAD`, and `commit` (and `--amend`) works on a detached `HEAD`,
  logging `commit: <subject>` in `HEAD`'s reflog only. Staying on the same commit while detached writes no reflog line. A branch checked out in another work tree is refused.
- After a switch that carried local changes the paths are listed as `M\t<path>`, `A\t<path>` and so on, as git does, and
  `Switched to branch '<name>'`, `Switched to a new branch '<name>'` or `Already on '<name>'` goes to standard error.
- The `HEAD` reflog gets `checkout: moving from <old> to <new>`; a created branch gets `branch: Created from <start>`.

`checkout [<tree-ish>] -- <path>...` takes the paths from the index or the commit into the work tree (and, from a commit, into the
index); `checkout <path>` without `--` works when nothing else has that name; `--ours` and `--theirs` write a side of an unmerged
path and leave it unmerged. A path no tree or index entry has is `error: pathspec '<path>' did not match any file(s) known to
git` (`1`); an unmerged path is `error: path '<path>' is unmerged`.

**`restore`.** `restore <path>...` writes the index entries into the work tree; `--source <rev>` writes that commit's files instead
and, as in git, removes tracked files the commit lacks; `--staged --worktree` does both for index and work tree, from `HEAD` unless
`--source` is given; `--staged` alone is described under [staging](#staging-and-committing). The paths are git pathspecs, see [Pathspecs](#pathspecs).

`restore --ours` and `--theirs` write that stage of each unmerged path under the paths into the work tree (the later of the two
wins), and `restore --merge` merges stages 2 and 3 over stage 1 again and writes the result with `ours`, `base` and `theirs` as the
marker labels, always in diff3 style. Neither touches the index, so the paths stay unmerged. A side the path lacks removes the work
tree file, as `restore` does for any path the source lacks (`checkout --ours|--theirs` reports `error: path '<path>' does not have
our version` or `their version` and exits `1` instead). A path that is not unmerged is restored from the index, local edits
included. `--merge` on an unmerged path without both sides prints `error: path '<path>' does not have all necessary versions`
(`1`, nothing written); a binary conflict writes our version with `warning: Cannot merge binary files: <path> (ours vs. theirs)`.
The result takes our file mode, and a link is written as git does (a link whose target is the merged text). The usual pathspec
errors apply (`error: pathspec '<path>' did not match any file(s) known to git`, `1`, nothing written). The flag checks are git's,
in this order, each `128`: `--merge` with `--ours` or `--theirs` (`git checkout: --ours/--theirs, --force and --merge are
incompatible when checking out of the index.`), then any of them with `--staged` (also with `--worktree`), then with `--source`
(`'--merge', '--ours', or '--theirs' cannot be used when checking out of a tree`).

Limits: git recreates the conflict of a path already resolved with `add` from its resolve-undo record; pyn does not keep that
extension, so `restore --merge` of a resolved path restores the staged content instead. `restore` with no path is a usage error
(`129`), not git's `128`.

### Reset

`reset` moves the current branch (or a detached `HEAD`) and resets the index and work tree by mode, through the same lock files,
`ORIG_HEAD` and reflog lines as git. The default is `--mixed HEAD`; the last mode flag given wins.

| Mode | Branch | Index | Work tree |
| --- | --- | --- | --- |
| `--soft` | moves | kept | kept; refused while a merge is in progress (`Cannot do a soft reset in the middle of a merge.`, `128`) |
| `--mixed` | moves | becomes the target's tree | kept |
| `--hard` | moves | becomes the target's tree | becomes the target's tree, local changes and untracked files in the way included; other untracked files stay |
| `--keep` | moves | becomes the target's tree | files that differ between `HEAD` and the target are rewritten; stops when one of them has staged or unstaged changes, or an untracked file is in the way, or the index has unmerged paths |
| `--merge` | moves | becomes the target's tree | files that differ between the index and the target are rewritten; stops when one of them has unstaged changes or an untracked file is in the way; unmerged paths are put back to the target |

A refused `--keep` or `--merge` (and any failure before the index is written) prints `error: Entry '<path>' ...` and `fatal: Could
not reset index file to revision '<rev>'.`, exits `128` and changes nothing, `ORIG_HEAD` included. A successful reset without
paths writes `ORIG_HEAD` (the old `HEAD`), appends `reset: moving to <rev>` (the revision as typed) to the branch's log, when the
branch moved, and to `HEAD`'s, and removes the merge state (`MERGE_HEAD`, `MERGE_MSG`, `MERGE_MODE`, `AUTO_MERGE`, `MERGE_RR`,
`SQUASH_MSG`, `CHERRY_PICK_HEAD`, `REVERT_HEAD`), so a merge in progress ends with `--mixed`, `--hard` and `--merge`. A detached
`HEAD` that does not move gets no line. `--mixed` lists what is left unstaged (`Unstaged changes after reset:` then `M\t<path>`), `--hard`
prints `HEAD is now at <abbreviation> <subject>`, and `-q` silences both.

`reset [<rev>] -- <path>...` replaces only the index entries under the paths with the revision's (default `HEAD`) and touches no
ref, no work tree file and no merge state; it is the same as `restore --staged --source <rev>`, but a path that matches nothing is
not an error. `reset <path>` works without `--` when the name is not a revision; a name that is both is `ambiguous argument`
(`128`). Paths are literal files and folders relative to where you stand. Any mode but `--mixed` with paths is `Cannot do <mode>
reset with paths.` (`128`). On an unborn branch `HEAD` is the empty tree: `reset`, `--mixed`, `--hard` and `--merge` empty the index
(and `--hard` and `--merge` remove the files), `--soft` does nothing, no ref or `ORIG_HEAD` is written, and `--keep` fails.

`ORIG_HEAD`, `MERGE_HEAD` and `CHERRY_PICK_HEAD` are accepted wherever a revision is, and so are the reflog revisions
(`HEAD@{1}`, `@{-1}`; see [Reflogs](#reflogs)). `--soft`, `--mixed`, `--hard`, `--keep` and `--merge` keep the skip-worktree flags (see [Sparse checkout](#sparse-checkout)). A running rebase or cherry-pick's own
state folder (`rebase-merge/`, `sequencer/`) is not removed. `--hard --recurse-submodules` also moves the checked-out submodules, see [Submodules](#submodules). Covered against the real git by `crates/pyn-cli/tests/git_reset.rs`.

### Merging

`merge <branch|tag|commit>` follows git's `ort` results for what pyn handles, with these steps:

1. It refuses unmerged entries (`error: Merging is not possible because you have unmerged files.`, git's hints and `fatal: Exiting because of an unresolved conflict.`, `128`), a merge or cherry-pick not yet concluded (`fatal: You have not concluded your merge (MERGE_HEAD exists).`, `128`), a rebase or revert already in progress, and an unknown name (`1`). `ORIG_HEAD`
   is written, as git does, once the target is known.
2. If the target is an ancestor of `HEAD` it prints `Already up to date.`. If `HEAD` is an ancestor it fast-forwards, moving the
   index and work tree with the rules above and logging `merge <name>: Fast-forward`; `--no-ff` makes a merge commit instead and
   `--ff-only` refuses when it cannot (`Not possible to fast-forward, aborting.`, `128`).
3. Otherwise there must be a merge base. No base is `refusing to merge unrelated histories`. Several (a criss-cross) are
   merged into one virtual base as `ort` does: the oldest first by committer time, each merged with the next against their
   own merge bases, the result never committed. A conflict inside it stays as markers (labelled `Temporary merge branch 1`
   and `2`, two characters wider per level), the base of a conflict in the final merge is shown as `merged common
   ancestors`, and `-X ours` and `-X theirs` apply to the final merge only. Refused with nothing changed: `-s resolve` and
   `-s subtree` (or `-X subtree`) with several bases, a file meeting a folder while the bases are merged, bases that
   conflict among themselves when two of them share a committer time (git's order is then not known), and bases nested
   more than sixteen deep.
4. The trees are merged path by path: a side that changed alone wins, a path both changed is merged line by line with the
   three-way merge of `pyn-merge`, a file mode changed on one side is taken, and a binary or symlink changed on both is a
   conflict. Renames are followed as `ort` does (see [Renames in a merge](#renames-in-a-merge)). A path that is a file on one side and a folder on the other is resolved as `ort` does: a file
   the other side left alone or deleted gives way to the folder without a conflict; otherwise the file is moved to
   `<path>~HEAD` or `<path>~<name>` (`/` in the name becomes `_`, and `_0`, `_1`... is added while the name is taken) and
   stays unmerged at its own stage, with `CONFLICT (file/directory)` (and the modify/delete conflict when the file was
   edited where the other side replaced it with a folder). A submodule meeting a folder is refused. This holds for
   `ort`, `recursive` and `subtree`, and for `rebase`, `cherry-pick`, `revert`, `am --3way` and `stash apply`; remerge diffs
   still refuse such a path and several merge bases. Those replays have one base (the commit's parent, the patch's
   rebuilt base or the stash's base), so only a recreated merge in `rebase --rebase-merges` can have several, and it merges
   them into a virtual base as above.
5. As in git's `ort` the index must equal `HEAD`: a difference stops the merge with `error: Your local changes to the following
   files would be overwritten by merge:` and the differing paths on one line, two spaces in, then `Merge with strategy ort failed.`
   (exit `2`). Then the files the merge changes must be clean in the work tree: a modified one, an untracked file in the way, or
   a folder holding untracked files where a file arrives each get git's own message (`Your local changes ... would be overwritten
   by merge:`, `The following untracked working tree files would be overwritten by merge:`, `Updating the following directories
   would lose untracked files in them:`), then `Aborting` and `Merge with strategy ort failed.` (exit `2`). A fast-forward that is
   blocked prints `Updating <old>..<new>` first, the same messages and `Aborting`, and exits `1`. An untracked file that blocks
   a path below it is reported by its own name, and a tracked path that is also blocked hides the untracked report below it, as
   in git.
6. After a fast-forward or a merge commit it prints git's diffstat: one ` <path> | <n> +++---` line per changed file scaled
   to the terminal width (`COLUMNS`, else 80; `diff.statGraphWidth` caps the graph), ` N files changed, M insertions(+), K
   deletions(-)`, then `create mode`, `delete mode`, `mode change` and `rename <old> => <new> (NN%)` lines, with a binary file as
   `Bin <old> -> <new> bytes`. Renames are found as git finds them, by identical content and then by content at least half the
   same, whatever `diff.renames` says (git's merge forces it). `--stat` turns the diffstat on and `-n`/`--no-stat` off (the last
   one wins); `merge.stat=false` turns it off unless `--stat` is given. `--no-commit` and a conflict print none. The width is
   not read from the terminal, only from `COLUMNS` (else 80); `merge.stat=compact` and `--compact-summary` are not built, and
   `pull --rebase` prints no diffstat.
7. A clean result becomes a commit with the two parents, `Merge branch '<name>'` (`tag`, `commit` and `remote-tracking branch` as
   git words them; ` into <branch>` unless the branch is `main` or `master`; an annotated tag's message follows) or `-m`, and the
   reflog line `merge <name>: Merge made by the 'ort' strategy.` (the strategy is the one `-s` names). `--no-commit` stops before
   the commit with `MERGE_HEAD`, `MERGE_MSG` and `MERGE_MODE` written.
8. A conflict leaves the file with **diff3** markers (`<<<<<<< HEAD`, `||||||| <base abbreviation>`, `=======`,
   `>>>>>>> <name>`; git's default is the two-sided style unless `merge.conflictStyle` says diff3, and pyn always writes diff3),
   the unmerged index entries at stages 1, 2 and 3 (only the sides that exist), and `MERGE_HEAD`, `MERGE_MSG` (with git's
   `# Conflicts:` list), `MERGE_MODE` (`no-ff` or empty) and `AUTO_MERGE` (see below). It prints `Auto-merging <path>`, `CONFLICT (content|add/add|modify/delete):
   ...` and `Automatic merge failed; fix conflicts and then commit the result.`, and exits `1`. git sees an ordinary conflicted merge:
   `git status`, `git add`, `git commit` and `git merge --abort` work on it, and `pyn add`, `pyn commit` and `pyn merge --abort`
   work on one git started.

`merge --abort` is `reset --merge`: it puts back the index and the files the merge changed (and any path you staged since) and
keeps unrelated local changes, but stops (`not uptodate`) if a path you staged has been edited again. `merge --continue` is
`commit`. `merge --preview` needs a pyn server.

`merge --squash` merges as usual but records nothing: no commit, no `MERGE_HEAD`, no `HEAD` or reflog change. The index and work tree take the result, `ORIG_HEAD` is written, and `SQUASH_MSG` holds `Squashed commit of the following:` and each commit brought in, as `log` prints it. It prints `Squash commit -- not updating HEAD` (and `Fast-forward` when the branch could fast-forward, `Already up to date. (nothing to squash)` when there is nothing to bring). A conflict stops with `SQUASH_MSG` and a `MERGE_MSG` listing the conflicts, and is exit `1`; `--no-ff` with `--squash` is `fatal` (`128`); `-m` is ignored. `pyn commit` then starts from `SQUASH_MSG` (and `MERGE_MSG`) when there is no `-m`, and removes both; `reset --hard` removes them too. Squashing into an unborn branch is `fatal`.

`AUTO_MERGE` is written, as git does, when a merge stops with conflicts or at `--no-commit`: a pseudo-ref (a file in the git
directory holding a tree id) for the tree of the merge as it stands in the work tree, every path included, with a conflicted
text path holding its conflict markers, a binary or kept side its version, and a modify/delete conflict the surviving version.
It is removed when the merge is concluded, aborted, or reset away. Git 2.55 writes it for `ort`, `recursive` and `subtree`; git
before 2.50 only for `ort`, and pyn follows 2.55. `resolve` and `ours` never write it.

#### Strategies and options

`-s <strategy>` (`--strategy`) and `-X <option>` (`--strategy-option`, repeatable) are read by `merge` and `pull`. Only one `-s`
is accepted: trying several in turn is a usage error (`129`).

| Strategy | What pyn does |
| --- | --- |
| `ort` (the default), `recursive` | The tree merge above; `recursive` is an alias, named as given in the messages. |
| `ours` | A merge commit with the two parents and `HEAD`'s tree, whatever the other side holds: it needs no merge base, never fast-forwards (not even with `--ff-only`), ignores `-X`, and touches neither the work tree nor the index, which must equal `HEAD` (`Merge with strategy ours failed.`, `2`). `--no-commit` stops with the merge state written. |
| `subtree` | The tree merge with the other side (and the base) shifted to line up with `HEAD`'s tree, found the way git finds it by comparing trees two folders deep. It never fast-forwards. |
| `resolve` | Without `--no-commit`, first git's plain in-index merge: it prints `Trying really trivial in-index merge...`, and when every path is taken whole from one side (no deletion, no content merge) makes the commit with `In-index merge` and prints `Wonderful.`, otherwise prints `Nope.`. Then the tree merge, named `resolve`; a conflict leaves the same state, with conflict markers labelled as `ort` labels them. `-X` is an error (`Merge with strategy resolve failed.`, `2`), as in git. The index must equal `HEAD` first (`2`). |
| `octopus` | With one head git's octopus only ever takes the in-index merge or a fast-forward: any other merge is `Merge with strategy octopus failed.` (`2`). Merging several heads at once is not built. |

An unknown `-s` name ends the command before anything changes, as git does. Options are read when a real merge needs them, so
an option git does not define (`fatal: unknown strategy option: -X<name>`, `128`, after `ORIG_HEAD` is written) does not stop a
fast-forward or an up-to-date merge. The options of the tree strategies (`ort`, `recursive`, `subtree`):

| Option | Meaning |
| --- | --- |
| `ours`, `theirs` | A conflicting hunk of a file both sides changed takes that side's lines; the rest of the file still merges. A binary file, a link and an add/add take that side whole. Modify/delete and mode conflicts still conflict. The last of the two wins. |
| `ignore-space-change`, `ignore-all-space`, `ignore-space-at-eol`, `ignore-cr-at-eol` | Lines that differ only in that whitespace count as equal when finding changes. A side whose only changes are such differences changed nothing, so the other side's file is the result, as in git. |
| `subtree[=<path>]` | Shifts as `-s subtree` does; with a path, only that folder is tried, in either direction. |
| `diff-algorithm=myers\|default\|minimal`, `no-renormalize` | Accepted; pyn has one diff. |
| `no-renames`, `find-renames[=<n>]`, `rename-threshold=<n>` | Turn rename detection off or on, and set its similarity threshold (`50%` by default, written as for `diff -M`); see [Renames in a merge](#renames-in-a-merge). |
| `renormalize` | Accepted when no path being merged has a conversion (line endings, `ident`, a filter or `working-tree-encoding`), since then it changes nothing; otherwise the merge is refused with nothing changed (`128`) because pyn does not renormalize the sides (#273). |
| `patience`, `histogram`, `diff-algorithm=patience\|histogram` | Usage error (`129`): only the Myers diff is built. |

`pull.twohead` names the strategy when no `-s` is given (git reads it for `merge` too); several names in it are refused like several `-s`.
Not built: several `-s` in one command, `-X` for `resolve`, `pull.octopus`, `merge.<name>.mergeoptions` and `branch.<name>.mergeOptions`.
#### Renames in a merge

`merge` (with `ort`, `recursive` and `subtree`) finds the files each side moved since the merge base with the same detection
as `pyn diff -M` (identical content, then at least the threshold similar; no copies), and resolves them as `ort` does:

- An edit on one side follows a rename on the other: the result is at the new path, merged line by line against the old
  path's base, with `Auto-merging` for the new path and conflict markers labelled `HEAD:<path>` and `<name>:<path>`
  (and `<abbreviation>:<path>` for the base) when the two paths differ. A mode change follows too.
- A rename against a delete is `CONFLICT (rename/delete)` at the new path with the base and the renaming side's version
  unmerged (and `CONFLICT (modify/delete)` too when the renamed file was edited).
- A file renamed to two places is `CONFLICT (rename/rename)`: the old path unmerged at its base, each new path at its own
  side's stage, both holding the merged content. Renamed to the same place by both sides it merges there.
- Two different files renamed to one path are an add/add conflict, and a path added where the other side renamed to (with
  the old path untouched) is too, as without renames.
- A file added to a folder the other side renamed (every file of the folder gone, most of them to one new folder) moves
  with the folder. `merge.directoryRenames` is `conflict` (default: `CONFLICT (file location)`, the moved file unmerged
  at the side that added it), `true` (`Path updated`, no conflict) or `false` (the file stays).

`-X no-renames`, `-X find-renames[=<n>]`, `-X rename-threshold=<n>`, `merge.renames` (default `diff.renames`, default on),
`merge.renameLimit` (default `diff.renameLimit`, then 7000) and `merge.directoryRenames` apply, and a criss-cross merge
follows renames inside its virtual base. Older git releases' `ort` ignore `-X no-renames` and `merge.renames=false` (`recursive`
honors them); pyn always honors them.

Refused with nothing changed, where git has a result pyn does not reproduce: a rename onto a path the other side added
while it also edited or deleted the source, two renames to one path with either source edited on the other side, a file
renamed to two places whose edits conflict, a renamed link, submodule or other non-plain file edited on the other side, a
renamed file edited with only its mode changed against a delete, a folder renamed to several places with a file added to
it, a rename into a folder the other side renamed, an added file whose moved path is taken, a path that is not UTF-8 among the
added or removed, and, inside a virtual base, any rename conflict or folder rename conflict.
The replays follow renames the same way: `rebase` (and the merges it recreates), `cherry-pick`, `revert`, `am --3way` and
`stash apply`, `pop` and `branch` (also `--index`). Their base is the commit's parent (for `revert` the commit), the base
rebuilt from the patch's `index` lines, or the stash's base, and `-X no-renames`, `find-renames[=<n>]` and
`rename-threshold=<n>` apply to `rebase`, `cherry-pick` and `revert`; `am` and `stash` read `merge.renames`,
`merge.renameLimit` and `merge.directoryRenames`. The refusals above hold for them too. Remerge diffs follow no renames (so a merge
git resolves through a rename shows a conflict there).


#### Strategies in a replay

`rebase`, `cherry-pick` and `revert` take `-X <option>` (`--strategy-option`, repeatable) and `--strategy=<name>` (`rebase` also `-s`;
`-s` of `cherry-pick` and `revert` is `--signoff`, as in git). `pull --rebase` hands its `-s` and `-X` to the rebase, and the last
`-s` wins as in git. Each commit is merged with the tree merge, so `ort`, `recursive` and the options in the table above apply with
the same sides as in git: `ours` is the side being built on (`HEAD`), `theirs` the commit being replayed (for `revert`, the
commit's parent). A pick that becomes empty under `-X ours` is dropped by `rebase` and stops `cherry-pick` as an empty pick does.
The strategy is kept in `rebase-merge/strategy` and `strategy_opts` (a rebase) and `sequencer/opts` (a sequence of picks), so
`--continue` and `--skip` go on with it, and git can continue a stop of pyn's and the reverse.

Refused with a usage error (`129`) before anything changes: strategies other than `ort` and `recursive` (`ours`, `resolve`,
`octopus`, `subtree`; git runs them as separate programs for each pick), `-X subtree`, and the options the merge refuses
(`patience`, `histogram`, ...). Renames are followed (see [Renames in a merge](#renames-in-a-merge)). A strategy git does not define is `Could not find merge strategy` (`1`), and an
option it does not define `fatal: unknown strategy option` (`128`), both before the first commit, where git notices only when a
merge needs them. A workspace on a pyn server refuses them. git reads no strategy setting for a replay (`rebase.*` has none, and
`pull.twohead` is for merges), so none is read.

Covered against the real git by `crates/pyn-cli/tests/git_replay_strategies.rs` and `git_pull.rs`.

#### Merge options

These shape the message and the surroundings of a git home merge; `pull` passes none of them on.

| Option | What pyn does |
| --- | --- |
| `--signoff`, `--no-signoff` | Appends `Signed-off-by: <committer>` to the merge commit's message before the hooks and the editor see it, as `commit -s` does (not twice). Not added to `MERGE_MSG` of a stopped merge or to `SQUASH_MSG`. |
| `--log[=<n>]`, `--no-log`, `merge.log` | Adds git's shortlog to the generated or given message after a blank line: `* <branch>:` and one indented subject per non-merge commit brought in, newest first. More than `<n>` commits (20 by default, `merge.log=true` is 20, a number is that many) print `* <branch>: (N commits)`, `<n>` subjects and `  ...`. A tag is `tag '<name>'`, a bare commit `commit '<name>'`. `--log=0` and `--no-log` add none, a negative `--log=` value falls back to `merge.log`, a non-number is a usage error (`129`), and a bad `merge.log` is `fatal` (`128`) before anything changes. `SQUASH_MSG` has none. |
| `-F <file>`, `--file=<file>` | The message is the file, taken as `-m` is (it counts as given: no editor by default). `-` is a file of that name, as in git. An unreadable file is a usage error (`129`). `-F` with `-m` is a usage error: git concatenates them in command-line order, which pyn does not track. |
| `--cleanup=<mode>` | `strip` drops `#` lines and tidies blank lines and trailing blanks, `whitespace` tidies only (the default without the editor, so `#` lines of `-m` and `-F` stay), `verbatim` keeps every byte, `scissors` with the editor puts a scissors line after the message and drops everything from it on (otherwise it is `whitespace`, as in git, so a scissors line in `-m` stays), `default` is `strip` with the editor and `whitespace` without. Another name is `fatal` (`128`). With the editor, only `strip` appends the comment block. |
| `--into-name <name>`, `merge.suppressDest` | The title says `into <name>` instead of the real branch. ` into <dest>` is left out when the destination (the name given, else the branch, else `HEAD`) matches a `merge.suppressDest` glob (several allowed); with none set it is left out for `main` and `master`, and a setting replaces that default. |
| `--allow-unrelated-histories`, `--no-allow-unrelated-histories` | Merges histories with no common commit against an empty tree: files only one side has are added, a file both have is an add/add conflict whose diff3 base is `empty tree`. Without the option it is `fatal: refusing to merge unrelated histories` (`128`, `ORIG_HEAD` written). With `resolve`, `octopus` or `subtree` it is refused with nothing changed. |
| `--autostash`, `--no-autostash`, `merge.autoStash` | Stashes the staged and unstaged changes of tracked files (not untracked ones) when the merge is not `Already up to date`, prints `Created autostash: <id>`, and applies them after the merge commit or fast-forward (`Applied autostash.`). When the merge stops (conflicts, `--no-commit`, `--squash`, a hook veto) the stash id is kept in `MERGE_AUTOSTASH` with a hint, and applied by `commit` (so `merge --continue`) or `merge --abort`; `merge --quit` stores it as the newest stash entry instead. A merge that fails to start puts the changes back. If they do not apply cleanly they stay in the stash as `Applying autostash resulted in conflicts.` |
| `--verify-signatures`, `--no-verify-signatures`, `merge.verifySignatures` | The tip of the merged side must carry a signature the configured program (gpg, gpgsm or `ssh-keygen -Y`) accepts, as `verify-commit` judges it; otherwise `fatal: Commit <id> does not have a GPG signature.` or a bad-signature line (`128`) before anything changes. Only the tip is checked, as in git. |
| `--quit` | Forgets a merge in progress without touching the index or work tree: `MERGE_HEAD`, `MERGE_MSG`, `MERGE_MODE` and `AUTO_MERGE` go, `SQUASH_MSG` stays. Nothing in progress is success. With a branch, `--abort` or `--continue` it is a usage error (`129`). |
| `--ff`, `--commit`, `merge.ff` | `--ff` and `--commit` undo `--no-ff` and `--no-commit`. `merge.ff=false` is `--no-ff` and `merge.ff=only` is `--ff-only` unless the command line or `--squash` says otherwise. |
| `-q`, `-v`, `--progress`, `--no-progress` | `-q` silences the informational lines (`Updating`, `Fast-forward`, the diffstat); the others are accepted and change nothing. |

`--no-strategy-option` and `--no-into-name` reset what came before, as in git. The title of an ancestor spelled `<branch>^`, `<branch>^^`, `<branch>~<n>` is `branch '<branch>' (early part)` (and `branch '<branch>'` when it names the tip), and the `--log` heading follows it.

Not built, each a usage error (`129`): `--rerere-autoupdate` and `--no-rerere-autoupdate` (pyn has no rerere), `--overwrite-ignore`, `--no-overwrite-ignore`, and three negated forms whose git behavior depends on what came before or on the git version (`--no-message`, `--no-strategy`, `--no-cleanup`). `merge.verbosity`, `merge.stat=compact`, `branch.<name>.mergeOptions` and `merge.<name>.driver` are not read.

Covered against the real git by `crates/pyn-cli/tests/git_merge_flags.rs`: exit status class, `HEAD` (so the message, parents and tree), index, status, the merge state files, stashes and the work tree, never output text.

### Merge message editor

A merge commit's message opens in the editor when `-e`/`--edit` is given, or by default when stdin and stdout are terminals and
neither `-m` nor `--no-edit` is given. `GIT_MERGE_AUTOEDIT` (a boolean; anything else is `fatal: Bad value ...`, `128`) replaces
the terminal test, and an explicit `-m` turns the default off; the last of `--edit` and `--no-edit` wins. `pyn pull` passes both
flags to its merge, whose generated message does not count as `-m`. The editor is chosen as `commit` does ([lookup order](workspace.md#the-editor)) and sees `.git/MERGE_MSG` holding the message followed by git's `#` comment
block; `#` lines are dropped afterwards. The order is `pre-merge-commit`, `prepare-commit-msg` (source `merge`), the editor,
`commit-msg`; the hooks keep `GIT_EDITOR` when the editor runs and see `GIT_EDITOR=:` when it does not. An empty message
(`error: Empty commit message.`) or an editor that fails exits `1` with `Not committing merge; use 'pyn commit' to complete the merge.`,
leaving `MERGE_HEAD`, `MERGE_MODE` and `AUTO_MERGE` as for a hook veto and `MERGE_MSG` as generated. No editor opens for a
fast-forward, `--no-commit` or a conflicted merge. Covered against real git 2.43 and 2.56 by `crates/pyn-cli/tests/git_hooks.rs`
and `git_pull.rs`.

Still without `--edit`: `revert` and `cherry-pick` (`--no-edit` is accepted, `-e` is a usage error), `commit -e`/`--amend` with the
editor, and `tag -e`.

### Cherry-pick

`cherry-pick <commit>...` merges each commit's change onto the index and makes a commit with that commit's author and message,
the committer from the identity rules of `commit`, and a tree and id that match git's (the tests compare ids). The result is
the same as `merge` of the commit onto `HEAD` with the commit's parent as the merge base (`(empty tree)` for a root commit), so
the merge rules above apply, renames and files meeting folders included, with `ort`-style results, diff3 markers labelled `HEAD` and
`<abbreviation> (<subject>)` with the base `parent of <abbreviation> (<subject>)`. A commit is anything `resolve` takes, `A..B`
and `A...B` name ranges (applied oldest first, parents before children), and several arguments are applied in the order given.

1. Before anything changes it refuses, with git's wording and exit `128` (`fatal: cherry-pick failed` after `error:` lines): a
   sequence while a cherry-pick or revert is already in progress, unmerged entries, an unknown revision, an empty range (`empty commit set passed`),
   a merge commit without `-m <n>` (or `-m` past its parents), and a staged change (`your local changes would be overwritten by
   cherry-pick`; `-n` allows it). A merge in progress, a rebase and several commits onto an unborn branch are
   refused by pyn. `-m 1` on a commit that is not a merge is accepted, and `-m 0` is a usage error (`129`).
2. A path the commit changes whose file has unstaged changes, or has an untracked file in the way, stops the command with git's
   `Your local changes to the following files would be overwritten by merge` and nothing written; edits to other files stay.
3. A clean result is committed with the reflog line `cherry-pick: <subject>` on the branch (or a detached `HEAD`) and `HEAD`,
   and printed as `[<branch> <abbreviation>] <subject>` with `Author:` when it differs from the committer and the number of
   files changed (git's `Date:` line and insertion counts are not printed). On an unborn branch the commit is a root commit.
   `commit.gpgsign` or `-S[<keyid>]` signs it, `--no-gpg-sign` does not. Hooks are not run (one warning names them).
4. `-x` adds `(cherry picked from commit <id>)` (inside the trailer block the message ends with, else after a blank line) and
   `-s` a `Signed-off-by:` for the committer, in that order. `-n` leaves the merged result staged and in the files, writes the
   message to `MERGE_MSG` and commits nothing; with several commits each one merges onto the index the previous left.
5. A conflict prints `Auto-merging <path>` and `CONFLICT (...)` lines, `error: could not apply <abbreviation> (<subject>)` and the
   `--continue`, `--skip` and `--abort` hints, exits `1`, and leaves the unmerged index entries, the markers, `CHERRY_PICK_HEAD`,
   and `MERGE_MSG` with git's `# Conflicts:` list. With `-n` there is no `CHERRY_PICK_HEAD` and the hint names `add` and `rm`.
   For several commits, or a range, `.git/sequencer/` holds `head` (where `HEAD` was), `abort-safety` (where the sequence has
   got to), `todo` (one `pick <id> <subject>` line per commit still to do, the stopped one first) and `opts` (`record-origin`,
   `signoff`, `mainline`, `no-commit`, only when set). A single commit leaves no `sequencer/`. git reads and continues what pyn
   stopped and the reverse; the tests do both.
6. A commit whose change is already in `HEAD` (or is empty) stops as git does, with `CHERRY_PICK_HEAD` set, and prints `The
   previous cherry-pick is now empty`; pyn does not build `--allow-empty`, `--keep-redundant-commits` or `--empty`, so the way
   on is `--skip`.

`--continue` needs every unmerged entry staged, commits the index with `MERGE_MSG` (comment lines dropped) and the stopped
commit's author, logs `commit (cherry-pick): <subject>`, then goes on with the rest of the todo; with no `CHERRY_PICK_HEAD` (the
stopped commit was committed by hand) it checks the index equals `HEAD` and goes on. `--skip` is `reset --merge` to `HEAD`
(`ORIG_HEAD` and the reflog line of `reset`), then the rest. `--abort` is `reset --merge` to the commit `HEAD` was at when a
sequence started (to `HEAD` for a single commit) and removes `sequencer/`; when `HEAD` is no longer where the sequence left
it, it prints `You seem to have moved HEAD. Not rewinding, check your HEAD!` and only removes the state. `--quit` removes the
state and leaves the index and files. With nothing in progress `--continue`, `--skip` and `--abort` fail (`128`) and `--quit` succeeds.
`commit` and `merge` still refuse while a cherry-pick is in progress.

Differences from git: a range or list with a merge commit and no `-m` is refused before any commit is made (git applies the
ones before it); `-e`, `--edit`, `--ff`, `--cleanup`, `--rerere-autoupdate`, `--allow-empty` and
`--keep-redundant-commits` are usage errors; `--no-edit` is accepted; `-X` and `--strategy` are described in [Strategies in a replay](#strategies-in-a-replay). Covered against the real git by
`crates/pyn-cli/tests/git_cherry_pick.rs`.

### Revert

`revert <commit>...` is the inverse of [cherry-pick](#cherry-pick): each commit's change is undone by a merge with the commit
as the base and its parent as the incoming side (`(empty tree)` for a root commit), so the merge rules and the checks before
anything changes are the ones of cherry-pick. The new commit has the committer as its author, the message
`Revert "<subject>"` (`Reapply "<inner>"` when the subject is itself `Revert "<inner>"`), a blank line and `This reverts commit
<id>.`; for a merge commit with `-m <n>` the line is `This reverts commit <id>, reversing` and `changes made to <parent>.`.
Diff3 markers are labelled `HEAD` and `parent of <abbreviation> (<subject>)`, with the base `<abbreviation> (<subject>)`. A range
`A..B` or `A...B` is reverted newest first, and several arguments in the order given. The tests compare ids, refs, the index,
the files, the state files and the reflogs with the real git (`crates/pyn-cli/tests/git_revert.rs`).

1. `-n` leaves the result staged and in the files and writes `MERGE_MSG` and `REVERT_HEAD`; `-s` adds `Signed-off-by:`;
   `--no-edit` is accepted. `-X` and `--strategy` are described in [Strategies in a replay](#strategies-in-a-replay). `-x`, `-e`, `--edit`, `--cleanup`, `--rerere-autoupdate`,
   `--allow-empty` and `--keep-redundant-commits` are usage errors.
2. The reflog line is `revert: <subject>`, and `commit: <subject>` for a commit made by `--continue`.
3. A conflict exits `1` with `error: could not revert <abbreviation> (<subject>)`, the hints, the unmerged entries, markers,
   `REVERT_HEAD` and `MERGE_MSG`; several commits keep `.git/sequencer/` with `revert <id> <subject>` lines in `todo`. A revert
   that changes nothing (already undone) exits `1` with `MERGE_MSG` but, as in git, without `REVERT_HEAD`, so a single
   commit leaves nothing in progress.
4. `--continue`, `--skip`, `--abort` and `--quit` are shared with cherry-pick and work on either kind of stop, so a sequence of
   both (as git can hold) is continued by one command; git and pyn continue each other's stops.
5. Only a sequence of several commits (or a range) refuses while an operation is in progress, as in git; a single commit is
   checked by its own rules (unmerged entries, staged changes, files the revert touches).

### Rebase

`rebase [--onto <newbase>] [<upstream> [<branch>]]` (or `rebase --root [<branch>]`) replays the commits that are in `<branch>` (default the current branch) and
not in `<upstream>` onto `<newbase>` (default `<upstream>`), then moves the branch there. With no upstream, `branch.<name>.remote`
and `branch.<name>.merge` give it (the remote-tracking ref the remote's fetch refspec maps the merge ref to; `.` as remote means
the local branch). Each commit is applied exactly as [cherry-pick](#cherry-pick) applies it, with the author and message kept, a
new committer, and ids that match git's (the tests compare ids): diff3 markers labelled `HEAD` and `<abbreviation> (<subject>)`,
no rename detection. The replay runs on a detached `HEAD`; the branch moves when the last commit is made.

1. The list is the commits after the upstream, merge commits left out unless `--rebase-merges` is given (their changes arrive
   through the commits they merged, flattened, see [Rebasing merges](#rebasing-merges)), in git's `--topo-order` reversed. A commit whose change the upstream already has (the same
   changes, ignoring line numbers and whitespace, as git's patch id) is dropped, and one that becomes empty when replayed is
   dropped with `dropping <id> <subject> -- patch contents already upstream`; one that was empty already is kept. A commit
   whose parent is the commit `HEAD` is on is not recreated (git's fast-forward), so unchanged leading commits keep their ids,
   unless `--no-ff` (`-f`, `--force-rebase`) is given.
2. A branch that already contains `<newbase>` in a straight line, with the upstream's merge base being that base, prints `Current
   branch <b> is up to date.` and changes nothing (`Current branch <b> is up to date, rebase forced.` and a replay with
   `--no-ff`). A branch behind the upstream moves to it (`Successfully rebased and updated refs/heads/<b>.` on standard error).
   `<branch>` is switched to first, and a detached `HEAD` is rebased and stays detached.
3. Before anything changes it refuses with `error: cannot rebase: You have unstaged changes.` or `Your index contains uncommitted
   changes.` (exit `1`, untracked files do not count), an invalid upstream, `--onto` or branch (`fatal:`, `128`), a rebase,
   merge, cherry-pick or revert already in progress, and an unborn branch. `--autostash` (or
   `rebase.autoStash`; `--no-autostash` overrides it) stashes the changes first (`Created autostash: <id>`, the commit id in
   `rebase-merge/autostash`) and applies them at the end (`Applied autostash.`); changes that do not apply cleanly are put on
   `refs/stash` with `Applying autostash resulted in conflicts.`.
4. A conflict prints the same lines as a cherry-pick conflict with `could not apply <abbreviation>... <subject>` and the hints
   naming `pyn rebase --continue`, `--skip` and `--abort`, exits `1`, and leaves the unmerged entries and markers, `REBASE_HEAD`,
   `MERGE_MSG` with git's `# Conflicts:` list, and `.git/rebase-merge/` as git's merge backend writes it: `head-name`, `onto`,
   `orig-head`, `git-rebase-todo` (the commits still to do as `pick <id> <subject>`), `done`, `msgnum`, `end`, `stopped-sha`,
   `message`, `author-script`, `interactive`, `autostash` when there is one. (git 2.55 also lists the subjects as `# <subject>` and ends `message` with the `# Conflicts:` lines; pyn reads either and drops comment lines from the message). `ORIG_HEAD` is where the branch was. git reads and
   continues what pyn stopped and the reverse; the tests do both.
5. `--continue` needs every unmerged entry staged (otherwise it prints that the conflicts must be resolved and exits `1`),
   commits the index with the `message` file and the `author-script` author (reflog `rebase (continue)`), and goes on; an index
   equal to `HEAD` (the commit was made by hand, or the resolution is empty) commits nothing and goes on. `--skip` is `reset
   --hard` to `HEAD`, then the rest. `--abort` is `reset --hard` to `orig-head`, putting the branch and `HEAD` back
   and applying the autostash. `--quit` removes `rebase-merge/` and leaves `HEAD`, the files, `REBASE_HEAD` and `MERGE_MSG`; an
   autostash is put on `refs/stash`. With nothing in progress they are `fatal: No rebase in progress?` (`128`). `merge` and
   the other writing commands refuse while `rebase-merge/` exists; `commit` works during a rebase (on the detached `HEAD` too) so an
   `edit` stop can be amended.
6. A step that cannot run for local reasons (an untracked file in the way) leaves that commit first in `git-rebase-todo`, so
   `--continue` tries it again.

Reflog lines are `rebase (start): checkout <newbase>`, `rebase (pick): <subject>`, `rebase (continue): <subject>`, `rebase
(finish): returning to refs/heads/<b>` and `rebase (finish): refs/heads/<b> onto <id>` (`pull <arguments>` instead of `rebase`
for a pull, and `rebase: fast-forward` for a fast-forward in either).

7. `-i` (`--interactive`) writes the commits as an instruction sheet and opens it in the editor: `GIT_SEQUENCE_EDITOR`, then
   `sequence.editor`, then the message editor ([lookup order](workspace.md#the-editor); an error naming `pyn config set editor`
   without one on a dumb terminal; an editor of `:` keeps the file). The sheet is git's: `pick`, `reword`, `edit`,
   `squash`, `fixup`, `drop`, `exec`, `break` and `noop` (or `p`, `r`, `e`, `s`, `f`, `d`, `x`, `b`), ids abbreviated by the
   git home's unique-prefix policy, comment lines and blank lines ignored, lines in any order.
   A bad line prints `error: invalid command '<word>'` and
   `error: invalid line <n>: <line>` (also `missing arguments`, `could not parse '<rev>'` and `cannot 'squash' without a previous
   commit`) and stops with `HEAD` on the new base, the sheet as written, and `--edit-todo` (the editor on the remaining sheet) or
   `--abort` to go on; `--continue` repeats the errors. A sheet with no command left prints `error: nothing to do` and changes
   nothing. Unlike `rebase` without `-i`, a commit that becomes empty when replayed stops the rebase (`The previous cherry-pick is
   now empty`; `commit --allow-empty` keeps it, `--skip` or `--continue` drops it), and the up-to-date shortcut is not taken.
   - `reword` makes the commit, then opens its message (the commit's own, then a `#` comment that says where the rebase is) and
     replaces the commit with the edited message (comments dropped, whitespace cleaned); an empty message stops with the commit
     made. After a conflict, `--continue` opens the message of the resolved commit.
   - `edit` makes the commit and stops with exit `0` (`Stopped at <id>...  <subject>`, `amend`, `stopped-sha`, `author-script`,
     `message`, `REBASE_HEAD`): `commit --amend`, `commit` and other changes are allowed here (`commit` works on the detached `HEAD`
     during a rebase), and `--continue` goes on. Changes staged but not committed are committed into the commit, opening its message
     in the editor. `--skip` keeps the commit as it is and goes on.
   - `squash` and `fixup` merge the commit's change into the commit `HEAD` is on, collecting the messages in
     `rebase-merge/message-squash` as git does (`# This is a combination of <n> commits.`, `# This is the commit message #<n>:`,
     `# The commit message #<n> will be skipped:` and the skipped message commented out; a `squash!` or `fixup!` subject line is
     left out of a squashed message). Until the last of the chain the commit is replaced with the collected text as its message;
     the last one opens the collected text in the editor, or, when only `fixup` followed the first commit, keeps its message
     as it was (`message-fixup`). A conflict stops with `amend`, `current-fixups`, `message-squash`, `SQUASH_MSG` (not for a chain of
     `fixup`) and `MERGE_MSG`; `--continue` commits the resolution into the chain, `--skip` takes the commit out of it.
   - `exec <command>` runs the line with `sh -c` in the work tree root after printing `Executing: <command>`; a failure prints
     `warning: execution failed: <command>` and stops (`1`), as does a command that leaves the index or tracked files changed
     (`execution succeeded: ... but left changes`); `--continue` goes on without running it again. `break` stops with exit `0`.
     `-x <cmd>` / `--exec <cmd>` (repeatable) adds an `exec` line after every commit and its `squash`/`fixup` lines; alone it
     opens no editor.
   - `--autosquash` moves each `fixup! <subject>` or `squash! <subject>` commit under the earlier commit whose subject it names
     (the subject, the start of it, or an abbreviated id; `fixup! fixup! ...` is stripped to the target), making it `fixup` or
     `squash`, in git's order. `rebase.autoSquash` turns it on for `-i`, and `--no-autosquash` turns it off. Like current git, a
     bare `--autosquash` reorders without opening an editor (git 2.43 ignores it without `-i`). An `amend! <subject>` commit
     becomes `fixup -C`.
   - `fixup -C <commit>` keeps only that commit's message (an `amend!` commit's without its `amend!` line) and `fixup -c`
     does the same but opens the editor on the collected text at the end of the chain. The earlier messages are written as
     skipped (`# The 1st commit message will be skipped:`); after a `squash` in the chain, `-C` and `-c` behave like `squash`.
     A `squash!` or `fixup!` subject line is commented out of a message that is squashed in, an `amend!` line always.
     `--continue` after a conflict opens the editor on the collected text (the last command of a chain) or on the message of
     the resolved commit, as git does.
   - `rebase.missingCommitsCheck` (`ignore`, `warn`, `error`; anything else warns and is ignored) compares the sheet with
     `git-rebase-todo.backup` once the editor is closed, on `--edit-todo` and on a `--continue` before the first command:
     a commit with no line is listed under `Warning: some commits may have been dropped accidentally.`; `error` then stops
     with the edited sheet kept (`--edit-todo`, `--continue` or `--abort` go on from there), `warn` goes on. Use `drop` to
     remove a commit.
   - `rebase.instructionFormat` is the text after the id on each sheet line (a `--format` template). An empty commit gets
     `# empty` after it. A line keeps the text it was written with, so `pick <id>` stays without a subject. The sheet ends with
     git's whole comment block, which also lists `label`, `reset`, `merge` and `update-ref`. What the editor leaves is cleaned as
     `git stripspace --strip-comments` does (comment lines and doubled blank lines go), as in git; the blank and `#` lines the
     sheet was generated with stay in `git-rebase-todo` and `done` when no editor runs.
   - `Rebasing (n/m)` goes to standard error, followed by `\r`, before each command (not with `-q`). A stop writes `patch`
     (the commit's change against its first parent as `diff-tree -p` prints it: no renames, `Binary files ... differ`).
   - `rewritten-pending` and `rewritten-list` hold `<old> <new>` lines as in git: written after each `pick`, `reword`,
     `squash` and `fixup` once the commands that melt into it are done, and after `edit`, a conflict or `--skip` when the
     rebase goes on; leading picks that already sit on the new base are not recorded. When the rebase ends the list is used
     to copy notes (`notes.rewrite.rebase`, default on; `notes.rewriteRef` or `GIT_NOTES_REWRITE_REF`, globs allowed;
     `notes.rewriteMode` `concatenate`, `overwrite`, `ignore` or `cat_sort_uniq`) in one commit per notes ref, `Notes added by
     'git notes copy'`. A notes tree gets 0, 1 or 2 levels of two-digit directories by size. The `post-rewrite` hook gets the same list (see [Client hooks](#client-hooks)).
   - `rebase-merge/` is git's: `git-rebase-todo`, `git-rebase-todo.backup`, `done`, `msgnum`, `end`, `stopped-sha`, `amend`,
     `message`, `author-script`, `interactive`, `patch`, `rewritten-pending`, `rewritten-list`, `refs-to-delete`, `update-refs`,
     `squash-onto`; git continues what pyn stopped and the reverse (the tests do both).
   - A fast-forward of `HEAD` onto a replayed commit is logged `rebase: fast-forward` and, when `HEAD` is already there, not at
     all. Leading picks that already sit on the new base are not run: the start checks the last of them out, as git does.

#### Rebasing merges

`--rebase-merges[=rebase-cousins|no-rebase-cousins]` (`-r`; `rebase.rebaseMerges` is `true`, `false` or a mode;
`--no-rebase-merges` turns it off; any other mode is `fatal: unknown mode: <mode>`, `128`) recreates the merge commits. The
sheet is git's: it starts with `label onto` (the first merge base of the upstream and the branch), then for each branch tip
(merged branches first, the rebased branch last) a `# Branch <label>` comment, a `reset` to where the branch started (`onto` for
the merge base, `[new root]` for a root, in `no-rebase-cousins` mode the abbreviated id of a fork point that is not the merge
base, followed by `# <subject>`; `rebase-cousins` resets those to `onto`), its `pick` lines, a `label` where a later `merge`
or branch needs one (`branch-point`, or the name of the merged branch), and `merge -C <merge> <label> # <subject>`. A label is
the quoted name in `Merge branch '<x>'`, the branch of `Merge pull request ... from <x>` or the whole subject, with every run of
other characters turned into `-` and a `-2`, `-3` suffix to keep it unique; **like git 2.55, a local branch that points at the
tip of a merged branch names it instead** (the last such branch in ref order; git 2.43 uses the subject). Every command counts
in `Rebasing (n/m)`, `msgnum` and `end`; blank and `#` lines do not.

- `label <name>` writes `refs/rewritten/<name>` at `HEAD` and lists it in `refs-to-delete`; the refs are deleted when the rebase
  ends, is aborted or quit. A name `refs/rewritten/<name>` cannot be is `error: '<name>' is not a valid label` when the sheet is
  read.
- `reset <label>` moves `HEAD` and the work tree, local changes included, to the label (or any revision, or `[new root]`: an
  empty commit, written to `squash-onto`), logged `rebase (reset): '<label>'`. Nothing is logged when `HEAD` does not move.
- `merge [-C <commit> | -c <commit>] <label> [# <oneline>]` merges the label into `HEAD` with the original merge's message and
  author (the author is the current identity without `-C`/`-c`; the message is the oneline or `Merge branch '<label>'`), and the
  committer is the current identity. When the original merge's parents are `HEAD` and the labels unchanged, and `--no-ff` is not
  given, it is a fast-forward to the original (`rebase: fast-forward`). A label that is already merged does nothing. `-c`, and
  a merge without a commit, make the commit with the original message and then open the editor on it; the commit is amended only
  when the message changes. The merge commit's reflog line is `rebase (merge): <subject>` as in git 2.55 (git 2.43 names the
  command before it). A conflict stops with the merge in progress (`MERGE_HEAD`, `MERGE_MODE` `no-ff`, `AUTO_MERGE`, `MERGE_MSG`
  with git's `# Conflicts:` list), `stopped-sha`, `message`, `author-script`, an empty `patch` and `REBASE_HEAD`; conflict
  markers are labelled `HEAD` and `refs/rewritten/<label>`. `--continue` commits it with both parents (and drops it, as git
  does, when nothing is staged), `--skip` and `--abort` undo it. A merge is recorded in `rewritten-list` when it ran or stopped.
  A label that does not name a commit, a merge with nothing to merge, or a merge that cannot run leaves the command first in
  `git-rebase-todo` with `REBASE_HEAD` set, exits `1`, and prints git's `Could not execute the todo command` hint.
- `update-ref <ref>` records `HEAD` for a branch. `--update-refs` (or `rebase.updateRefs`; `--no-update-refs`) puts an
  `update-ref` line after the last command of every commit a local branch points at (the rebased branch left out), in reverse ref
  order, and writes `update-refs` (`<ref>`, the id it had, the id `update-ref` found, zeros before that, sorted by name). When the
  rebase ends each branch that still has its old value moves to the recorded one (reflog `rewritten during rebase`) and
  `Updated the following refs with --update-refs:` lists them; one that moved meanwhile is listed under `Failed to update ...`.
  It also works without `--rebase-merges`.

`--root` replays every commit down to the root (the first argument is then the branch). Without `--onto` the new base is an empty
commit (`rebase (start): checkout <its id>`, `onto` and `squash-onto` hold its id) and the root commit is fast-forwarded onto it,
so a plain `--root` keeps every id. With `--onto <newbase>` the commits after `<newbase>` are replayed on it (the root commit
too, if `<newbase>` does not contain it). The sheet's header names only the branch tip. `--root` with `--fork-point` is
`fatal: options '--root' and '--fork-point' cannot be used together` (`128`).

The **fork point** is used with no `<upstream>` argument and with `--fork-point` (`--no-fork-point` turns it off): the branch's
commits are counted from the best common ancestor of the branch and every commit the upstream's reflog names, if that is one of
them, so commits of a rewritten upstream are not replayed again. A name that is not a ref is `fatal: No such ref: '<name>'`.
The upstream stays the new base (`--onto` aside), the comment line still says `<upstream>..<branch>`, and a branch whose fork
point is not the new base is not fast-forwarded. In `no-rebase-cousins` mode the sheet then keeps the fork point as the base of
the branch, as git does.

Differences from git: `--signoff`, `--keep-base`, `--reapply-cherry-picks`, `--apply` and `--empty` other than `drop`
are usage errors (`129`); `-s` and `-X` are described in [Strategies in a replay](#strategies-in-a-replay). A `merge` of several labels (an octopus merge) stops with `octopus merges are not built` and
leaves the command first in the sheet; git continues it (`git rebase --continue`) and a merge whose branches have more than one
merge base is merged into a virtual base as `merge` does. pyn writes `AUTO_MERGE` only for a merge that stopped on conflicts; git writes it for every pick. A workspace's own
rebase refuses `fixup -C`/`-c` and `--rebase-merges`, `--root`, `--fork-point` and `--update-refs` (and the sheet commands `label`,
`reset`, `merge` and `update-ref`), and leaves `amend!` commits where they are. There is no `post-rewrite` hook. `-m`, `-v`, `-n`, `--keep-empty` and the `--no-` forms of the refused flags are accepted and ignored. Commits whose timestamps tie are ordered by the walk of the graph, as git's, not by
date. After an interactive rebase git leaves `REBASE_HEAD` behind in some cases; pyn removes it. Covered against the real git
(2.43 and 2.56) by `crates/pyn-cli/tests/git_rebase.rs` and, for pull, `crates/pyn-cli/tests/git_pull.rs`.

### Tags

`tag` lists `refs/tags` sorted (`-l <pattern>` filters). `tag <name> [<object>]` writes a lightweight tag at `HEAD` or the object
(a tag name tags the tag object itself); `-a`, or just `-m`, writes an annotated tag: a tag object with the tagger from
`GIT_COMMITTER_*` or `user.name` and `user.email` (an error without them), the object's type, and the cleaned message, byte for
byte as git writes it. There is no editor, so `-a` needs `-m`. `-f` replaces a tag (`Updated tag '<name>' (was <id>)`), `-d` deletes
(`Deleted tag '<name>' (was <id>)`). `-s`, `-u <keyid>`, `--no-sign` and `tag.gpgsign` sign an annotated tag (see [Signing](#signing)). Tags get no reflog unless `core.logAllRefUpdates=always`. As in git, `-a`, `-m`, `-f`, `-s` and `-u` need a tag name and are a usage error (`129`) when listing or with `-d`, and `-l` with `-d` is too; `-d` with no name does nothing.

On a pyn server workspace with `local_store` on, `pyn tag` has the same create, list (`-l`), `-a`/`-m`, `-f` and `-d` forms, with the
tag refs kept in the local store (`refs/tags/` there). The tagger is the signed-in account (`<account>@pyn.invalid`), `HEAD` is
the workspace branch's local tip, and the commit is `HEAD~N`, a hash or another tag. Tags are local only: `pyn push` and
`pyn pull` neither send nor fetch them, the server has no tag concept (publishing tags is a separate decision and needs a contract
change), and a workspace without the store refuses `tag` and says how to turn the store on. `pyn show`, `pyn diff` and `pyn log` accept a tag name
wherever they take a commit (`log v1..v2`, `log v1~2`, `log ^v1 HEAD`). Signing (`-s`, `-u`) needs a git home.

### Signing

pyn makes the signature by running the configured external program, as git does; pyn never links a crypto library and does not
need git installed. It writes the signature as git encodes it, so the object ids agree with git's for the same signature bytes.

- `gpg.format` is `openpgp` (default), `x509` or `ssh`; any other value stops with exit `128`. The program is `gpg.openpgp.program`
  then `gpg.program` (default `gpg`) for openpgp, `gpg.x509.program` (default `gpgsm`), or `gpg.ssh.program` (default `ssh-keygen`).
- The key is the `-S<keyid>` or `-u` value, else `user.signingkey`. For openpgp and x509 with neither, it is the committer
  (tagger) `Name <email>`.
- openpgp and x509 run `<program> --status-fd=2 -bsau <key>` with the object on standard input; the signature is standard output,
  and the run counts only if it exits `0` and reports `[GNUPG:] SIG_CREATED` on standard error.
- ssh runs `<program> -Y sign -n git -f <keyfile> <buffer>` and reads `<buffer>.sig`. `user.signingkey` is a private key file
  (`~/` is expanded) or a literal public key (`ssh-...` or `key::...`), signed through ssh-agent with `-U`. Without
  `user.signingkey`, the first line `gpg.ssh.defaultKeyCommand` prints (run with `sh -c`) is the key, as git takes it; a
  command that fails or prints nothing stops signing.
- A commit carries the signature as a `gpgsig` header (`gpgsig-sha256` in a SHA-256 repository) after the other headers, folded
  with a leading space on each continuation line; it signs the commit as it would be without that header. `--amend` signs anew
  (or not) and drops the old signature. A tag carries its signature appended to the message.
- `tag.gpgsign` makes every tag annotated and signed, so it needs `-m`. If the signer fails or is missing, nothing is written
  and the exit status is `128`.

The other commands that create commits sign them the same way and through the same signer:

- `merge` signs the merge commit, `cherry-pick` and `revert` each commit they make, `rebase` (also `-i` and `--rebase-merges`)
  each commit it replays, rewords or merges, and `pull` passes `-S` and `--no-gpg-sign` to its merge or its rebase. A
  commit a rebase or cherry-pick fast-forwards over is reused, not signed again, as in git.
- `-S[<keyid>]`, `--gpg-sign[=<keyid>]` and `--no-gpg-sign` (the last of the two wins) are accepted on all of them and win over
  `commit.gpgsign`, which signs them when set. There is no `rebase.gpgSign` or `merge.gpgSign`, as in git. A stopped
  cherry-pick, revert or rebase keeps the flag (`sequencer/opts`, `gpg_sign_opt`) for `--continue`, so the commits made after the
  conflict are signed too; `pyn commit` and `pyn merge --continue` take their own flags. A pyn server workspace refuses the flags.
- `stash` commits and the empty commit a `rebase --root` builds on are not signed, as in git.
- A signer that fails, or a bad `gpg.format`, writes no commit. The branch does not move and the command stops with `128`,
  leaving a merge, cherry-pick, revert or rebase stopped where it can be retried: `MERGE_HEAD` (merge), `CHERRY_PICK_HEAD` or
  `REVERT_HEAD`, or the failed pick first in the rebase sheet, as git does. Fix the signer and run `pyn commit`,
  `--continue` or the command again.

#### Verifying

`verify-commit`, `verify-tag`, `log --show-signature` and `show --show-signature` check a signature by running the same
program as git does, so pyn still links no crypto library. The signature's own text picks the format.

- openpgp and x509 run `<program> --keyid-format=long --status-fd=1 --verify <sigfile> -` with the signed object on standard
  input. A signature is good when the program exits `0` and reports `GOODSIG`, and the `TRUST_` level it reports reaches
  `gpg.minTrustLevel` (default `undefined`, so any). `BADSIG`, `ERRSIG`, `EXPSIG`, `EXPKEYSIG` and `REVKEYSIG` are not good.
- ssh needs `gpg.ssh.allowedSignersFile`; without it the check reports the error and fails. pyn runs `ssh-keygen -Y
  find-principals`, then `-Y verify -n git -I <principal>` (with `gpg.ssh.revocationFile` as `-r`) for each principal until one
  holds, passing the commit or tag time as `-Overify-time`, so a signer's `valid-after` and `valid-before` apply at that
  time. With no principal the key is checked by `-Y check-novalidate` and the signature is reported but not good.
- `verify-commit` and `verify-tag` print the program's report on standard error (`--raw` prints the gpg status lines
  instead, `-v` prints the signed object on standard output first) and exit `1` if any named object is unsigned, not good,
  missing, or of the wrong type (`verify-commit v1` on an annotated tag fails, as in git); no name is a usage error (`129`).
  A verifier that cannot be run counts as not good.
- `log` and `show` print the report on standard output right after the `commit` line, nothing for an unsigned commit, and
  `No signature` when an ssh check could not be made. A bad signature does not change their exit status.
- The program's own wording is passed through, so reports differ from git's wherever the installed gpg or ssh-keygen does;
  the tests compare exit status and what the report says, not its bytes. The report is not colored.

### Stashing

A stash is the commit git makes: the work tree's tracked files as the tree, with `HEAD`, a commit of the index, and with `-u` a
parentless commit of the untracked (not ignored) files as its parents, on `refs/stash` with a reflog line `WIP on <branch>:
<abbreviation> <subject>` (`On <branch>: <message>` with `-m`). The reflog is the list, so pyn writes `logs/refs/stash` (always, as
git does) and keeps its chain consistent when an entry is dropped. After saving, the index and tracked files are reset to `HEAD`
(`ORIG_HEAD` and a `reset: moving to HEAD` line follow, as git's), and with `-u` the untracked files are removed; ignored files
stay. `list` prints `stash@{N}: <message>`. `apply` and `pop` merge the stash's changes onto the current index and work tree
with the same tree merge as `merge` (labels `Updated upstream`, `Stash base` and `Stashed changes`), stage files the stash adds,
leave other changes unstaged, restore untracked files that do not already exist, and print the status; `pop` then drops the entry
(`Dropped <name> (<id>)`, the name as typed or `refs/stash@{N}`). A conflict, or local changes to a file the stash touches, keeps the stash. `drop` and `clear`
remove entries. `stash push -p` is described in [Choosing hunks](#choosing-hunks).

`stash push -- <paths>` stashes only the named paths (pathspecs, see [Pathspecs](#pathspecs), relative to where you stand): the stash's work tree
is the index with those paths taken from the work tree, and only they are put back to `HEAD`, with no `ORIG_HEAD` or `HEAD`
reflog line (a whole-tree stash writes both). A name that matches nothing in the index fails with `error: pathspec '...' did not
match any file(s) known to git` (`1`), before anything is saved; with `-u` a name must match a tracked or untracked file, and git's
own order of failures is kept (an unmatched name stops after the stash is saved and before anything is changed). `-k` keeps the
staged changes in the index and work tree and stashes everything as usual. `apply` and `pop` take `--index` to restore the
staged state: the recorded index is merged onto the current one first (a clash is `error: conflicts in index. Try without
--index.`, `1`, and nothing changes), the work tree is merged as usual, and a conflict there prints `Index was not unstashed.`; a
real index restore writes `ORIG_HEAD` and a `reset: moving to HEAD` line, as git does. Where git applies the staged part as a
patch, pyn merges it, so a clash is judged by the tree merge. A stash is named `stash@{N}`, `N`, nothing (the newest) or, for
`apply`, `show` and `branch`, any stash-like commit.

`stash show` prints the diffstat by default (`stash.showStat`), or the patch with `-p`/`--patch` (`stash.showPatch`), both with
`-p --stat`, and takes `--stat`, `--shortstat`, `--numstat`, `--raw`, `--dirstat`, `--name-only`, `--name-status`, `-U<n>` and the other
`diff` options (`-w`, `-W`, `--color`, `--word-diff`, ...); an option without an output format shows the patch rather than the configured default, as git does;
`-u`/`--include-untracked` and `--only-untracked` (or `stash.showIncludeUntracked`) add the untracked files the stash saved. Numeric output has the same
format-combination limits as `diff --numstat`. `stash branch <name> [<stash>]` creates the branch at the commit the stash was
made on, switches to it, applies
the stash with `--index` and drops it (a conflict keeps it). `stash create [<message>]` prints a stash commit without storing it
or changing the work tree (nothing is printed when there is nothing to stash), and `stash store [-m <message>] <commit>` puts
such a commit on the list (`Created via "git stash store".` without `-m`; storing the newest entry again does nothing).

git 2.43 cannot stash in a SHA-256 repository, so the SHA-256 stash is tested for consistency, not against git.

### Choosing hunks

`add -p`, `restore -p`, `checkout -p`, `reset -p` and `stash push -p` share one prompt, with git's header, hunks, keys and texts. For each changed file it
prints the diff header, then each hunk followed by `(<n>/<total>) <Verb> this hunk [<keys>]? `, and reads a line from standard
input. `<Verb>` and the help text follow the command: `Stage` for `add`, `Stash` for `stash`, `Unstage` for `restore --staged` and `reset`,
`Discard ... from worktree` / `from index and worktree` for `restore` from the index or `HEAD`, and `Apply ... to worktree`,
`to index` / `to index and worktree` when `--source` names anything but the literal word `HEAD` (the diff is then shown from the
current state towards the source, with the `a/` and `b/` names swapped, as git does). A mode change, a deletion and an addition
are asked as `mode change`, `deletion` and `addition` (an empty file added or removed has no lines to show, but is still asked about).

The answers are `y`, `n`, `q`, `a`, `d` (upper case too), `j`, `J`, `k`, `K`, `g`, `/`, `s`, `e`, `p`, `P` and `?`, as in git 2.55.
An answer is one letter: a blank line asks again, `Only one letter is expected, got '<text>'` answers a longer one (except `g<n>`
and `/<regex>`, which take their argument at once), and `Unknown command '<c>' (use '?' for help)` a letter that is no key. Only
the keys that can apply are offered and listed in the help. `j` and `k` go to the next and previous undecided hunk and `J` and `K`
to the next and previous one, all rolling over at the ends; they say `No other undecided hunk` or `No other hunk` when there is
nowhere to go. A hunk already decided shows `(was: y)` or `(was: n)` in its prompt. The hunk is printed when the prompt moves to it
(and again by `p` and `P`, which print it the same way without a pager), not after help or a refused key. `g` lists the hunks in
pages of 20 (` ` undecided, `+` chosen, `-` skipped) and asks for a number; `/` takes a regular expression (the `regex` crate's
syntax, not POSIX) matched against the hunk text, from the current hunk on. `s` splits a hunk at the context between its runs of
changes and prints `Split into <n> hunks.` (`Sorry, cannot split this hunk` when it has one run); `e` writes the hunk to
`.git/addp-hunk-edit.diff`, starts [the editor](workspace.md#the-editor) and takes the hunk back with `#`
lines removed; an edit that leaves nothing keeps the hunk unchanged, one that does not fit the file is reported
(`error: patch failed: <path>:<line>`) and asks `Your edited hunk does not apply. Edit again (saying "no" discards!) [y/n]? `.
Messages about a key go to standard output, errors from applying to standard error. A blank line follows each file's questions;
`q` stops with the choices already made applied, and so does the end of input, which also skips the files not yet asked about.
With nothing to ask about it prints `No changes.` (or `Only binary files changed.`) on standard output and exits `0`.

What a choice does: `add -p` writes the chosen hunks (and mode changes) into the index only. `restore -p` takes them out of the
work tree, the index or both; when the chosen hunks fit the work tree but not the index it says so and asks
`Apply them to the worktree anyway? ` as git does (a mode change that differs from the file's mode only warns, as `git apply` does). `stash push -p` shows `HEAD` against the work tree, ignoring the index and new
files, saves a stash whose work tree is `HEAD` with the chosen hunks, takes them out of the work tree and keeps the index
(`--no-keep-index` also puts the staged changes of the paths back to `HEAD`); `-u` is refused, and with no hunk chosen it prints
`No changes selected` and exits `1`. Each file is applied as soon as its questions are over, as git does. `add -p` cannot be
combined with `-n`, nor `restore -p` with `--ours`, `--theirs` or `--merge`. The prompt reads standard input, so a
script can answer it; binary files and a file replaced by a symlink (or the reverse) are not offered.

Differences from git: binary files are counted for the notice but never asked about; the text of regular-expression errors is
the `regex` crate's; the diff uses 3 lines of context unless `diff.context` and `diff.interHunkContext` say otherwise (a bad value is `fatal`; with `diff.context=0` git's own apply refuses the chosen hunks, pyn applies them) and ignores `--inter-hunk-context` and colour (`color.interactive`,
`interactive.diffFilter` and `interactive.singleKey` are not read). An index with unmerged paths prints `<path>: needs merge` on standard output when the prompt starts and after each file whose hunks were applied (not for `reset -p` and `restore --staged -p`), and the unmerged paths are skipped; with nothing else to ask about nothing more is printed. `restore -p -S -W`, and `checkout -p` with a tree-ish, do not offer an unmerged path's differences as git does, and `stash push -p` refuses while a merge is in progress. git 2.43 differs from 2.55 in the prompt (no `p`, `P` or
`(was: ...)`, `j`/`k` offered only when they apply, help wording, hunk printed again after every answer, first letter of a longer
answer taken, end of input moving on to the next file, messages on standard error); pyn follows 2.55, and the tests that compare
with the real git check the index, work tree, stash and exit status, on inputs both versions treat alike, and with git 2.55 or later the whole transcript on standard output. Covered by
`git_patch.rs`.

### Interactive add

`add -i [<path>...]` prints the status table (`staged` and `unstaged` as `+<added>/-<deleted>`, `binary`, `unchanged` or
`nothing`; no rename detection, an intent-to-add file counts as `+0/-0` staged) and the menu, and reads one line per prompt from
standard input, like git's built-in version. A command is chosen by number or by a unique prefix of its name (`s`, `u`, `r`,
`a`, `p`, `d`, `q`, `h`); a blank line shows the menu again, `?` the prompt help and anything else says `Huh (<text>)?` on
standard error. End of input leaves with `Bye.`. The commands that list files take numbers, ranges (`3-5`, `5-`), a comma or
space separated list, `-` to unselect, `*` for all, and a name or unique prefix of it (the prefix is shown in brackets, up to
four characters), as git does, with its quirks (`1x` selects from item 1 to the end). A blank line ends the choice.

- `status` lists the files; `update` stages the work tree state of the chosen files (a file that is gone is removed from the
  index) and says `updated <n> path(s)`; `revert` puts the chosen index entries back to `HEAD`, naming the files `HEAD` does not
  have (`note: <path> is untracked now.`), and says `reverted <n> path(s)`; `add untracked` stages chosen untracked files
  (`added <n> path(s)`, `No untracked files.`); `diff` shows `diff --cached` against `HEAD` for the chosen files; `help`
  describes the commands.
- `patch` asks about the chosen files' hunks with the prompt of [`add -p`](#choosing-hunks) (binary files are left out;
  `No changes.` or `Only binary files changed.` goes to standard error when none is left).
- Paths limit every list, are taken from where you stand, and the names in the lists are relative to the repository root.
  `-N`, `-u`, `-A`, `-f` and `-v` are accepted and ignored, `-p` wins over `-i`, and `-n` is `fatal` (`128`), as in git.

An unmerged path is listed as git lists it (`+0/-0` staged, the work tree against the `ours` stage as unstaged, `+0/-0` when `ours` is missing), `<path>: needs merge` is printed when the command starts and after a `revert` that leaves other unmerged paths (which then skips `reverted <n> path(s)`), `update` stages the work tree file and clears the conflict, `revert` puts the `HEAD` entry back, `diff` prints `* Unmerged path <path>` and `patch` says `ignoring unmerged: <path>` and leaves it out.

Differences: colour (`color.interactive`), `interactive.diffFilter` and `interactive.singleKey` are not read; `diff.context` and `diff.interHunkContext` apply to `patch`; the text of the `diff` command comes from `pyn diff --cached`, which still shows an unmerged path as a deletion where git prints `* Unmerged path <path>` (and `U` / `Unmerged` in the name-status, raw and stat forms).
git 2.43 keeps a carriage return at the end of an input line, which later versions strip; pyn strips it. Covered by
`git_add_interactive.rs`, which gives the same input to git and pyn in SHA-1 and SHA-256 repositories.

### Sparse checkout

`pyn sparse-checkout init|set|add|list|reapply|disable|check-rules` keeps git's own state, so git and pyn read each other's:
`core.sparseCheckout` and `core.sparseCheckoutCone` in the work tree's config (`config.worktree`, with
`extensions.worktreeConfig` turned on in the shared config, as git does, and read back by pyn when that extension is set), the
patterns in `info/sparse-checkout` of the work tree's git directory, and the skip-worktree flag of the index entries. `set` and
`init` default to cone mode, as git does; `--cone` and `--no-cone` choose, and `--stdin` and `--skip-checks` work as in git.
`--sparse-index` is a usage error (pyn has no sparse index; `--no-sparse-index` is the default). A cone file that is not valid
cone patterns is read as non-cone patterns. Files with local changes that the new patterns leave out stay, with git's warning.
`check-rules` takes `-z`, `--rules-file` and `--cone`/`--no-cone`.

`switch`, `checkout <branch>`, `reset --hard`, `reset --mixed`, `restore --staged`, a fast-forward `merge`, `clone --sparse`
(the root files only, cone mode) write only the files the patterns include and add the other entries with
the skip-worktree flag. `status` ignores skipped entries and adds the line `You are in a sparse checkout with N% of tracked files
present.` after the branch line; `add` and `rm` refuse a path that only names skipped entries, with git's message and status `1`.
`ls-files -t` and `-v` show `S` for them.

Also sparse-aware, with git's rules: files outside the patterns stay out of the work tree and keep their skip-worktree
flag, and results are computed in the index and trees.

- A merge that is not a fast-forward, `cherry-pick`, `revert`, `rebase` (all backends pyn has) and `am` update the index for
  skipped paths without writing them. A conflict on a path outside the patterns is written to the work tree with its unmerged
  entries (the flag is dropped), is reported, and `merge --abort`, `reset --merge` and `reset --hard` put the file back as
  git does.
- `stash` (push, pop, apply, branch) leaves skipped files out; applying a stash that changes paths outside the patterns writes
  them to the work tree and drops their flag, as git does.
- `restore` and `checkout` of paths from the index leave skipped entries alone (a pathspec that names only skipped entries is an
  error); `--ignore-skip-worktree-bits` restores them and drops the flag. `restore -p` and `checkout -p` from the index, `reset -p`
  and `add -p` skip them.
- `reset --keep` and `--merge` update skipped entries in the index only. `apply --index`, `--cached` and `--3way` treat a skipped
  path the patch names as an ordinary one.

Still refused with an error that changes nothing, because git's result there is not worth copying yet: `restore` or `checkout`
from a tree when the tree has a different version of a skipped path or a path outside the patterns that the index lacks;
`--ignore-skip-worktree-bits` together with a tree; `restore -p`, `checkout -p`, `reset -p` from a tree that differs in a skipped
file; `stash -p`; applying a stash with `--index`, or one that conflicts, when it changes paths outside the patterns.
`add --sparse` and `rm --sparse` are not built. Covered by `git_sparse.rs` and `git_sparse_replay.rs`, which run each scenario on
two copies of a repository, pyn on one and git on the other, and compare the patterns file, the `ls-files -t` tags, the files,
`HEAD` and the in-progress state, for cone and non-cone patterns.

### Linked work trees

`pyn worktree` keeps git's own layout, so git and pyn work on the same linked work trees: `.git/worktrees/<name>/` holds `HEAD`,
`gitdir`, `commondir` and, while locked, `locked` (the reason and a newline, or empty), and the work tree has a `.git` file
`gitdir: <admin folder>`. `<name>` is the last component of the path, with a number appended when it is taken. Paths are written
absolute (no `worktree.useRelativePaths`).

| Command | Behaviour |
| --- | --- |
| `add [-f] [--detach] [--no-checkout] [--lock [--reason <text>]] [-b <branch>\|-B <branch>] [--orphan] [-q] <path> [<commit-ish>]` | Without `-b`/`-B`/`--detach`: a branch named `commit-ish` is checked out (refused when another work tree has it, unless `-f`), any other commit-ish is detached, and with no commit-ish the branch named like the folder is used or created at `HEAD`. `--orphan` makes an unborn branch with an empty work tree. Prints `Preparing worktree (...)` and `HEAD is now at ...` on stderr. A failure removes what it made. |
| `list [-v] [--porcelain [-z]] [--expire <time>]` | The main work tree first, then the linked ones by admin folder name; `[branch]`, `(detached HEAD)`, `(bare)`, `locked` and `prunable`. Porcelain output is git's (`worktree`, `HEAD`, `branch`/`detached`/`bare`, `locked [reason]`, `prunable <reason>`). |
| `lock [--reason <text>] <worktree>`, `unlock <worktree>` | Create and remove `locked`. A work tree is named by its path or by a unique trailing part of it. |
| `move [-f -f] <worktree> <new-path>` | Renames the folder and rewrites `gitdir`; an existing folder as destination receives it. A locked work tree needs `-f -f`. |
| `remove [-f [-f]] <worktree>` | Refuses the main work tree, a locked one (`-f -f` overrides), one with changes or untracked files, or with a checked-out submodule (`-f` overrides the last two). |
| `prune [-n] [-v] [--expire <time>]` | Removes admin folders whose work tree is gone and that are not locked. |
| `repair [<path>...]` | Rewrites `gitdir` and the `.git` file so each points at the other, for the given work trees and for those whose `.git` file still exists. |

Not built: `add --track`/`--guess-remote` and `worktree.guessRemote`, `--[no-]relative-paths`, carrying sparse and per-worktree config over to a new work tree
(`config.worktree`), the `HEAD` reflog of a new work tree, and `ORIG_HEAD`. `remove` and `move` do not run hooks.

### Submodules

`pyn submodule` keeps git's layout so git and pyn can continue each other's work: `.gitmodules` in the work tree
(`submodule.<name>.path`, `url`, `branch`, `update`, `shallow`, `ignore`, `fetchRecurseSubmodules` are read; `add`, `set-branch` and
`set-url` write `path`, `url` and `branch`), `submodule.<name>.url`, `active` and `update` in `.git/config`, the submodule's git
directory in `.git/modules/<name>` with `core.worktree` pointing back, and a `.git` file `gitdir: <relative path>` in its work tree.
A repository pyn builds passes `git submodule status`, `git fsck` and a clean `git status`, and the other way round; the tests check
both against git 2.43 and 2.56 (`crates/pyn-cli/tests/git_submodule_lifecycle.rs`). Nested repositories are cloned and fetched
through the same code as `pyn clone` and `pyn fetch`; no git program runs. `-q`/`--quiet` goes before or after the subcommand.

| Command | Behaviour |
| --- | --- |
| `add [-b <branch>] [-f] [--name <name>] [--depth <n>] [--] <repository> [<path>]` | Clones into `.git/modules/<name>`, writes `.gitmodules` and the config, and stages `.gitmodules` and the gitlink. A relative repository (`./`, `../`) is recorded as given and resolved against the current branch's remote (else `origin`, else the work tree) for the clone. An existing repository at the path is adopted (`Adding existing repo at ...`). A path already in the index is `fatal` (`128`); a kept git directory is reused only with `-f`. Not built: `--reference`, `--dissociate` (usage error), and refusing an ignored path. |
| `status [--cached] [--recursive] [<path>...]` | ` ` current, `-` not checked out, `+` at another commit than recorded, `U` merge conflict, then the id and the path, and for a checked-out submodule its name in git's order (`describe`, `--tags`, `--all`; git's `--contains` step is not tried). No command defaults to `status`. |
| `init [<path>...]` | Registers `submodule.<name>.url` (a relative url resolved), `update` (not `!command`) and `active`, and prints `Submodule '<name>' (<url>) registered for path '<path>'`. Without paths it follows `submodule.active`. |
| `update [--init] [--recursive] [--remote] [--checkout\|--merge\|--rebase] [-f] [-N] [--depth <n>] [<path>...]` | Clones a submodule that is not checked out (or reconnects its kept git directory), fetches when the recorded commit is missing (then that commit by id), and moves it by `submodule.<name>.update` or the flag: detached checkout, merge or rebase, with git's `Submodule path '<path>': checked out '<id>'` lines; `!command` runs the command with the id appended. `--remote` follows `origin/<branch>` (`submodule.<name>.branch`, `.` for the superproject's branch, else the remote `HEAD`) instead of the recorded commit. `update = none` skips. `shallow = true` in `.gitmodules` clones at depth 1. |
| `deinit [-f] (--all \| [--] <path>...)` | Refuses local changes without `-f` and a `.git` directory (not a gitdir file), empties the folder, removes `submodule.<name>` from `.git/config`; the git directory stays. |
| `sync [--recursive] [<path>...]` | Copies the resolved `.gitmodules` url to `.git/config` and to the submodule's remote. |
| `set-branch (-b <branch> \| -d) <path>`, `set-url <path> <url>` | Edit `.gitmodules`; `set-url` then syncs. |
| `foreach [--recursive] <command>` | Runs `sh -c` in each checked-out submodule with `name`, `sm_path`, `path`, `displaypath`, `sha1` and `toplevel` set; a failure is `fatal: run_command returned non-zero status for <path>` (`128`). |
| `summary [--cached \| --files] [-n <n>] [<commit>] [--] [<path>...]` | git's `* <path> <a>...<b> (<n>):` blocks with `>` and `<` first-parent commit subjects. Type changes between a file and a submodule are not summarized. |
| `absorbgitdirs [<path>...]` | Moves a `.git` directory inside a submodule to `.git/modules/<name>` and links both ways, recursively. |

**Recursion.** `fetch --recurse-submodules` fetches in every checked-out active submodule, and `=on-demand` (the default) only where the
index or a ref records a commit the submodule lacks (git decides by the commits it just fetched); `fetch.recurseSubmodules`,
`submodule.<name>.fetchRecurseSubmodules` and `submodule.recurse` are read. `switch`, `checkout` and `reset --hard` take
`--recurse-submodules` (and `--no-recurse-submodules`; `submodule.recurse` turns it on): afterwards each checked-out submodule is moved to the commit
now recorded, without cloning or fetching, and a missing commit is `fatal`. Without it a changed gitlink is left alone in the work tree and
shows as modified; a populated submodule never counts as an untracked directory in the way. `pull --recurse-submodules=yes` fetches and then updates. Committing a changed gitlink is an ordinary
`add <path>` and `commit`.

Not built: `diff --submodule`, `--also-filter-submodules`, `submodule.<name>.update` set to `!command` from `.gitmodules` (it is
not copied by `init`), `submodule.fetchJobs` and `-j` (one at a time), `--reference`, `fetch.recurseSubmodules` on `remote update`,
`merge`, `rebase` and `cherry-pick --recurse-submodules`, `mv` and `rm` of a submodule keeping `.gitmodules` in step, and `push
--recurse-submodules`. Messages for some failures (a clone that fails, a bad `--branch`) carry pyn's wording after the same `fatal:` line.

### Bare repositories

A folder that is itself a git directory (`HEAD`, `objects/` and `refs/`, and `core.bare = true` or no work tree) is a git home, found from
the folder or any folder inside it. `pyn init-git --bare` and `pyn clone --bare` / `--mirror` make one. A command that git runs
only with a work tree fails as git does, before looking at its options: `fatal: this operation must be run in a work tree` (`128`)
for `status`, `add`, `rm`, `mv`, `commit`, `restore`, `switch`, `checkout`, `merge`, `pull`, `rebase`,
`cherry-pick`, `revert`, `am`, `stash`, `clean`, `reset --hard`/`--merge`/`--keep`, `diff` with fewer than two revisions and no
`--cached`, `grep` without a revision or `--cached`, and `ls-files -o`/`-m`/`-d`. `reset --soft` works, and `reset` and `reset --mixed`
are `fatal: mixed reset is not allowed in a bare repository`.

Everything that reads history or refs works unchanged: `log`, `show`, `diff <a> <b>`, `blame` (of `HEAD` unless a revision is given),
`grep <rev>`, `branch` (a branch is never "checked out" in a bare repository, so its `HEAD` branch can be moved or deleted), `tag`,
`ls-tree`, `cat-file`, `rev-parse` (`--git-dir` prints `.` at the top; `--is-bare-repository`, `--show-toplevel` fails), `fetch`,
`push`, `remote`, `bisect` (which never checks out) and `worktree list` (`(bare)`). A bare repository writes no reflogs unless
`core.logAllRefUpdates` is set, as in git. `pyn`'s own folder is not created in one.

There is no work tree, so `log a.txt` without `--` is `fatal: ambiguous argument 'a.txt': unknown revision or path not in the working tree.` (`128`) unless `a.txt` is a revision, and paths after `--` are relative to the git directory wherever in it `pyn` runs: `../a.txt` is `outside repository` (`128`), as in git.

### Separate git directory

`pyn init-git --separate-git-dir` and `pyn clone --separate-git-dir` keep the git directory away from the work tree, which holds a
`.git` file `gitdir: <absolute path>`. Nothing else differs: git and pyn open the repository through that file, and a command run
inside the git directory itself fails as git does for the commands that need a work tree. 
`pyn init-git --separate-git-dir <dir>` in the work tree of an existing repository moves its git directory to `<dir>` (absent or empty,
its parent existing) and leaves or rewrites the `.git` file, then points each linked work tree's `.git` file at the moved
`worktrees/<name>` folder; it prints `Reinitialized existing Git repository in <dir>/`. The configuration is not edited, so
`core.worktree` stays as written, as in git. `-b` is ignored with `warning: re-init: ignored --initial-branch=<name>`, and an
`--object-format` that differs from the repository's is `fatal: attempt to reinitialize repository with different hash` (`128`,
before anything moves, where git moves first). A non-empty `<dir>` is `fatal: unable to move <old> to <dir>: Directory not empty`
(`128`), and a linked work tree is refused. Submodule work trees keep their relative `.git` files, which git does not rewrite
either. `pyn worktree list` names the real work tree as main where git names the moved git directory.

### Clean

`clean` removes the files git does not track from the work tree, and prints git's lines: `Would remove <path>` with `-n`,
otherwise `Removing <path>` (`-q` prints neither), paths relative to where you stand and a folder with a trailing `/`. It
refuses with `fatal: clean.requireForce defaults to true and neither -i, -n, nor -f given; refusing to clean` (`128`) unless
`-n`, `-f` or `-i` is given (or `clean.requireForce` is `false`), and `-x` with `-X` is `128`. Tracked files, submodule folders,
`.git` and pyn's own `.pyn` folder are never removed, and a link is removed as a link, never followed.

- Default: untracked files that no ignore rule hides (every `.gitignore`, `.git/info/exclude`, the global excludes file).
  Untracked folders stay unless `-d` or a path is named; with either, a folder whose contents are all to be removed is one
  `dir/` entry, and one that also holds ignored files is listed file by file so those stay.
- `-x` also removes ignored files (the standard rules are not read); `-X` removes only ignored files, and lists them inside
  untracked folders without `-d`.
- `-e <pattern>` adds an ignore rule for this run that wins over every file, including with `-x`; with `-X` it also selects.
- A nested repository the index does not track is left alone, silently when it is the folder itself and as
  `Would skip repository <path>` / `Skipping repository <path>` when it is inside an untracked folder (the rest of that folder
  is still removed). `-ff` removes nested repositories whole.
- Paths limit the clean to themselves and what is under them; standing in a folder limits it to that folder. Paths are
  git pathspecs, see [Pathspecs](#pathspecs) (a folder also matches with a trailing slash, so `dir/*` takes `dir/` itself).
  A path outside the repository is `128`.
- `-X` with a glob lists an all-ignored folder as one `dir/` entry when the text before the glob's first wildcard can lead into
  it (`*.log` and `?` lead into every folder, `u*` only into folders starting with `u`), even if nothing inside matches, as git does.
- A folder you stand in is not removed: `Would refuse to remove current working directory` / `Refusing to ...`, then the
  folder's entries are listed with paths built from it, as git does.
- A path that cannot be removed prints `warning: failed to remove <path>` and the exit status is `1`.

With `-i` the items to remove are listed first (`Would remove the following items:`, in columns down the page of the
`COLUMNS` width, else 80), then the menu, with git's prompts and texts:

| Command | Does |
| --- | --- |
| `1` clean | Removes what is listed, with the usual `Removing <path>` lines (`-n`, `-q` and `-ff` apply). |
| `2` filter by pattern | Lists the items, then asks `Input ignore patterns>> ` until an empty line. Patterns are separated by spaces and are ignore rules (`!` negates, the last match wins); an item is dropped when a rule matches it, a folder above it, or a folder item with its trailing slash. A line that matches nothing prints `WARNING: Cannot find items matched by: <line>`. |
| `3` select by numbers | Numbers the items in rows (`*` marks the chosen), asks `Select items to delete>> `, and keeps only the chosen. Answers are numbers, `3-5` ranges (`3-` to the end), `*`, a case-insensitive unique name prefix, and `-` before any of them to unselect, separated by spaces or commas; `?` shows the help and an empty line finishes. A word that names nothing prints `Huh (<word>)?`. |
| `4` ask each | Asks `Remove <path> [y/N]? ` for each item; `y`, `ye` and `yes` in any case remove it, anything else keeps it. |
| `5` quit | Prints `Bye.` and removes nothing. |
| `6` help | Describes the commands. |

A menu answer is a number or a case-insensitive prefix of a command name (`c`, `f`, `s`, `a`, `q`, `h`); an empty answer or
`?` shows the menu or its help again, and anything else prints `Huh (<answer>)?`. A list that becomes empty prints
`No more files to clean, exiting.` and ends the run. End of input quits (`Bye.`), cancels a selection, and stops
asking. A folder holding a nested repository is one entry that, chosen, prints `Skipping repository <path>` for the
repository and removes the rest; a folder you stand in is the entry `./` and is handled as above once chosen. Nothing is
listed, and nothing printed, when there is nothing to clean. Colours (`color.interactive`) are not applied.

Differences from git: the order of the
lines inside a folder that holds a nested repository is by name, where git uses the file system's order (and prints the
folders in it without a trailing `/`, which pyn copies); names are laid out by their character count, not their
display width; and a last answer without a newline in `ask each` is read the way git reads it (an `y` stays the answer for
the items after it), but the filter prompt stops at the end of input where git can loop. Git 2.43 and 2.55 behave alike
for every answer the menu reads. Covered against the real git, on SHA-1 and SHA-256 repositories, by
`crates/pyn-cli/tests/git_clean.rs`, which compares exit status, the work tree and the actions taken, not the menu text.

### Grep

`grep` prints git's lines for the pattern: `<path>:<line>` (with `-n`, `<path>:<number>:<line>`), paths relative to where you
stand, quoted as git quotes them, in index order, and with revisions `<rev>:<path>` for each revision in turn. The exit
status is `0` when anything matched (also under `-q`), `1` when nothing did, `128` for a fatal error (no pattern, a bad
pattern, an unknown revision or path, `--cached` with a revision) and `129` for a usage error. A file with a NUL in its first
8000 bytes is binary and prints `Binary file <name> matches` once (`-a` searches it as text, `-I` skips it); `-c` and `-l`
treat it like any other file. Without a revision only tracked, regular files are searched, from the work tree (a file deleted
there is skipped, an intent-to-add file is searched, an unmerged path once); `--cached` reads the index and leaves out
unmerged and intent-to-add paths. Symbolic links and submodules are never searched. Options go before the pattern, as git
requires, and `--` ends the revisions. Pathspecs are git's (see [Pathspecs](#pathspecs)) and default to the current folder. The settings `grep.lineNumber`, `grep.patternType`
(`default`, `basic`, `extended`, `fixed`), `grep.extendedRegexp`, `grep.fullName` and `core.quotePath` are read; colour is
never produced.

Patterns are POSIX basic expressions by default (`-G`), extended with `-E` (the last of `-G`, `-E`, `-F` wins), or fixed
strings. The GNU additions git relies on work in both: `\w \W \s \S \b \B \< \>`, and in basic expressions `\| \+ \?`.
Expressions are translated for the `regex` crate in `crates/pyn-cli/src/git_home/grep_regex.rs`, with the error messages of
the GNU library (`fatal: command line, '<pattern>': Unmatched ( or \(`; `-e option, '...'` for `-e`). A pattern is split at
newlines into several, as git does. `--and`, `--or`, `--not` and parentheses combine patterns per line with git's
precedence (`--not`, then `--and`, then `--or`, which juxtaposed patterns also mean); a stray `--or` is ignored as git does.
A plain pattern list that matches an empty line also matches the empty line git imagines after a final newline (so
`grep -n '^$'` reports one more line for a file that ends in a newline), exactly as git does, but not with `-v`, `-w` or
`--and`/`--not`/parentheses.

What differs, all of it refused or documented rather than silently different:

| Difference | pyn |
| --- | --- |
| Back-references (`\1`, `(a)\1`) | Fatal error `back-references are not supported` (`128`); a reference to a group that does not exist gives git's `Invalid back reference`. |
| Alternation with `-w` | The regex engine takes the first branch that matches, where the GNU library takes the longest: `grep -Ew 'a\|ab'` can miss a word git finds. Without `-w` a line matches either way. |
| ``\` ``, `\'` | Refused (`128`): git's result depends on how it reads the file, not on the line. With `-i`, a backslash before a lowercase letter that is not an operator (`\a`) is refused: git matches nothing there. |
| Locale | Matching is by characters in UTF-8, as in a UTF-8 locale; `[[:alpha:]]` and similar classes are ASCII only. A pattern that is not valid UTF-8 is refused. |
| Binary attributes | `-diff` (and `binary`) makes a file binary and `diff` forces text, as in git; `textconv` and `diff=<driver>` settings are not consulted. |
| Revisions | Commits and tags that peel to one (`HEAD~2`, a branch, a hash); a tree such as `HEAD:sub` is not a revision here, so it is read as a path and fails with git's `ambiguous argument`. |
| Not built (usage error `129`) | `--untracked`, `--no-index` (so also searching outside a repository), `--recurse-submodules`, `--exclude-standard`, `-o`, `-L`, `-z`, `-f`, `-m`, `-P`, `-A`, `-B`, `-C`, `-p`, `-W`, `--column`, `--heading`, `--break`, `--color`, `--all-match`, `--max-depth` and abbreviated long options. |

Covered against the real git, on SHA-1 and SHA-256 repositories, by `crates/pyn-cli/tests/git_grep.rs` (output, order and
exit status for the work tree, the index, revisions, binary files, subfolders, conflicts and a few hundred patterns of
both syntaxes) and `crates/pyn-cli/tests/git_exit_status.rs`, which compare only behaviour the git releases in use agree on.

### Bisect

`bisect` is `git bisect` with the same search, text, exit statuses and state. `start [<bad> [<good>...]] [-- <path>...]`
begins (a bare `start` waits for `bad` and `good`), `good`/`old` and `bad`/`new` (or the words chosen with
`--term-good`/`--term-old` and `--term-bad`/`--term-new`) answer for the commit checked out or the revisions named, `skip`
takes revisions or `<a>..<b>` ranges, `next` repeats the step, `reset [<commit>]` ends it, `log` prints the record,
`replay <file>` redoes one, `terms` prints the words and `run <command>...` answers by exit status until the bad commit is
found. `--no-checkout` leaves `HEAD` alone and moves `BISECT_HEAD`; `--first-parent` follows only first parents.

Each step prints `Bisecting: N revisions left to test after this (roughly M steps)` and `[<id>] <subject>`, and checks the
commit out detached (the reflog line is `checkout: moving from <branch or id> to <id>`). The commit tried is the one git
picks: the candidates are the commits reachable from the bad one and not from the good ones (history-simplified for a path
limit), each weighed by how many candidates it reaches, and the one closest to half wins, with git's tie rules and its
choice away from skipped commits. When a bad commit has no candidate before it, `<id> is the first bad commit` follows with
the commit message and diffstat, and the bisection stays recorded until `reset`. Good commits that are not ancestors of the
bad one make it test their merge base first (`Bisecting: a merge base must be tested`). Exit statuses are git's: `0` for a
step and for the report, `1` for an error, `2` when only skipped commits are left, `3` for a bad merge base, `4` when a path
limit leaves nothing to test, `129` for a usage error and `128` for a fatal one. `run` ends with `0` once the first bad
commit is found. The command's status `0` means good, `125` skip and `1` to `127` bad; a status of `128` or more ends the
run, and `bisect run` then exits with `256` minus that status. A first status of `126` or `127` makes `run` try the command on
a good commit before it believes the status.

The state is git's, byte for byte: `BISECT_START`, `BISECT_LOG`, `BISECT_TERMS`, `BISECT_NAMES`, `BISECT_EXPECTED_REV`,
`BISECT_ANCESTORS_OK`, `BISECT_FIRST_PARENT`, `BISECT_HEAD` and `BISECT_RUN` in the git directory of the work tree, and
`refs/bisect/<bad>`, `refs/bisect/<good>-<id>` and `refs/bisect/skip-<id>`. A bisection started by either tool continues
in the other, and a log from either replays in the other.

What differs, all of it refused or documented rather than silently different:

| Difference | pyn |
| --- | --- |
| `visualize`, `view` | Usage error (`129`): they start `gitk` or `git log`. |
| `skip <a>...<b>` | Usage error (`129`); `<a>..<b>` is built. |
| Pathspecs | Git's, see [Pathspecs](#pathspecs), relative to where `start` and the later steps run. |
| Prompts | With a terminal on standard input, the questions git asks (`Do you want me to do it for you [Y/n]?`, `Are you sure [Y/n]?`) are asked and answered there; otherwise git's non-interactive answers apply. |
| Wording | Messages say `pyn bisect` where git says `git bisect`; the text in `BISECT_LOG` stays `git bisect` so both tools read it. The report quotes the terms (`first 'bad' commit`) as git 2.55 does; older gits print them bare. |
| `bisect run` | The command runs through `sh -c` from the work tree root, as git runs it. |
| Linked work trees | Each work tree has its own state, as in git. |

Covered against the real git, on SHA-1 and SHA-256 repositories, by `crates/pyn-cli/tests/git_bisect.rs` (a bad commit at
several places of a straight history, random merge histories with equal and skewed commit times, skipped commits and
ranges, path limits, `--first-parent`, `--no-checkout`, custom terms, `run`, `reset`, `replay`, and bisections that git
starts and pyn continues, and the reverse; output, exit statuses, `HEAD`, refs and files are compared) and
`crates/pyn-cli/tests/git_exit_status.rs`, which compare only behaviour the git releases in use agree on.

### Blame

`blame` and `annotate` print git's lines: for each line of the file at `<rev>` (the work tree file when no revision is
given) the commit that last changed it, found by diffing along the parents as git does. A parent whose file is identical
takes all of it; otherwise each parent in order takes the lines that are unchanged against it, so a merge hands a line to
its first parent when both have it. A file that is not in a parent under the same path is looked up as a rename (exact
content, then a uniquely same-named file at least 75% similar, then the best match of at least 50%) and
followed; `-f` shows the old names. The diff is a port of git's own line diff with its hunk placement and
`diff.indentHeuristic`, so ambiguous insertions land on the same lines. Every layout is git's: the default (abbreviated id
with `^` on boundary commits, author, date, line number, aligned), `-e`, `-s`, `-f`, `-n`, `-l`, `-t`, `-b`, `--root`,
`--abbrev=<n>`, `--date=<default|iso|iso-strict|rfc|short|relative|unix|raw>`, `-p` and `--line-porcelain`, and
`annotate`'s tab-separated layout (also `-c`). `-L <start>,<end>`, `,+<n>`, `,-<n>`, `/<regex>/` (a basic expression)
and `^/<regex>/` anchors work as git reads them, repeated or overlapping. `-w` ignores whitespace when comparing a commit
with its parent. `--first-parent` follows only the first parent of each merge. Revisions are `<rev>` (before or after the file, with or without `--`), `<a>..<b>` and `^<rev>`;
commits reachable from the excluded side are boundaries, and so are root commits unless `--root` or `blame.showRoot`.
`blame.blankBoundary`, `blame.showEmail`, `blame.date`, `diff.indentHeuristic` and `core.abbrev` are read. Author and committer names and emails are mapped by the [mailmap](#mailmap).

A modified work tree file is blamed on `Not Committed Yet <not.committed.yet>` at the current time, with `HEAD` (and
`MERGE_HEAD`) as parents; `--contents <file>` (or `-` for standard input) does the same for another file's content as
`External file (--contents)`. A file that is not in `HEAD` or the index is a fatal error (`128`), as is a path missing
from the named revision. A binary file is blamed line by line like git does, not refused. The exit status is `0`, `128`
for a fatal error and `129` for a usage error.

What differs, all of it refused or documented rather than silently different:

| Difference | pyn |
| --- | --- |
| Not built (usage error `129`) | `-M`, `-C`, `--incremental`, `-L :<funcname>`, `--ignore-rev`, `--ignore-revs-file`, `--reverse`, `--show-stats`, `--progress`, `--score-debug`, `--color-lines`, `--color-by-age`, `--minimal`, `--diff-algorithm`, `-S`, abbreviated long options, `<a>...<b>`, the other revision-list options, and `--date=human`, `local`, `format:...` and `auto:...`. |
| Refused (`128`) | `diff.algorithm` other than `myers`. |
| Abbreviation length | The [abbreviation policy](#log-on-a-git-home) (`core.abbrev`, else the object-count default), grown until the abbreviation of every blamed commit is unique, plus one for the boundary mark. |
| Content filters | `textconv` is not applied. The line-ending conversion of the work tree file is not applied to the lines `blame` reads from it. |
| Commit encodings | Author names and subjects are printed as stored, without re-encoding from an `encoding` header. |
| Renames | The single added path is matched as git's rename detection would, but `diff.renameLimit` is not applied. |

Covered against the real git, on SHA-1 and SHA-256 repositories, by `crates/pyn-cli/tests/git_blame.rs` (every layout,
ranges, renames, merges, boundaries, a dirty work tree, awkward files and seeded random histories) and
`crates/pyn-cli/tests/git_exit_status.rs`, which compare only behaviour the git releases in use agree on.

### Reflogs

pyn appends the reflog lines git would, when `core.logAllRefUpdates` would: `HEAD`, `refs/heads/*`, `refs/remotes/*` and
`refs/notes/*` get a log created (the default in a work tree, and `true`); `always` creates one for every ref; `false` creates
none; an existing log is appended to either way. The lines are `commit: <subject>` (`commit (initial)`, `commit (merge)`, `commit (amend)`),
`checkout: moving from <old> to <new>`, `branch: Created from <start>`, `Branch: renamed <old> to <new>`, `merge <name>:
Fast-forward` or `Merge made by the 'ort' strategy.`, `reset: moving to HEAD`, the stash message and, for a fetch, `fetch <arguments>: fast-forward` and its siblings (see [Fetch on a git home](#fetch-on-a-git-home)). A line names the committer
identity (with the committer date from `GIT_COMMITTER_DATE`); with no identity configured a reflog line falls back to the login name
so branching and switching still work. A moved branch is also logged in `HEAD`'s log, as git does.

Every command that moves a ref writes git's line: `commit: <subject>`, `commit (initial)`, `commit (merge)`, `commit (amend)`,
`merge <name>: ...` (`pull <arguments>: ...` when a pull ran the merge), `reset: moving to <rev>`, `cherry-pick: ...`, `revert: ...`,
`rebase (start|pick|continue|finish|abort): ...`, `checkout: moving from <old> to <new>`, `branch: Created from <start>`,
`tag: tagging <abbreviation> (<subject>, <date>)` (tags only get a log with `core.logAllRefUpdates=always`), `update by push`
and the stash and fetch lines above. `ORIG_HEAD` gets `reset: updating ORIG_HEAD` (`rebase: ...`, or no prefix after a merge) when
it already has a log or `core.logAllRefUpdates=always`. Covered against the real git, one step at a time, by
`crates/pyn-cli/tests/git_reflog.rs`.

#### Reflog revisions

`<ref>@{N}` is where `<ref>` was `N` moves ago, from its reflog (`@{0}` is the current value, and `N` equal to the length reads
the value before the oldest line when there is one); `@{N}` alone is the current branch's. `@{-N}` is the branch (or commit,
when `HEAD` was detached) checked out `N` checkouts ago, read from the `checkout: moving from <old> to <new>` lines of `HEAD`'s
reflog; `switch`, `checkout` and every command that takes a revision accept it, and `switch @{-1}` is `switch -`.
`[<branch>]@{upstream}` (also `@{u}`, any case) is the remote-tracking ref `branch.<name>.remote`/`merge` names through the
remote's fetch refspec (or the local branch for remote `.`), and `[<branch>]@{push}` the one a `push` would update
(`branch.<name>.pushRemote`, `remote.pushDefault`, `remote.<name>.push` and `push.default` as git reads them, including
`cannot resolve 'simple' push to a single destination`). `~N` and `^N` may follow any of them. A name with no reflog, or `N`
past its end, fails (`log for '<ref>' only has <n> entries`, `128`). Not built: dated selectors (`@{yesterday}`,
`@{2.days.ago}`) and `:/<text>`. In code, `Repo::resolve` (`pyn-gitwork`) understands all of the above and
`Repo::previous_checkout(n)` returns the `@{-n}` name for commands that need the branch rather than its commit.

#### The `reflog` command

`reflog [show] [-n <N>] [<ref>]` (`reflog` alone is `show HEAD`) prints `<abbreviation> <ref>@{N}: <message>` per line, newest
first, with `<ref>` as typed (`HEAD`, a branch's short name, otherwise the full name). `reflog list` prints `HEAD` and every ref
with a log. `reflog exists <full-ref>` is silent and exits `1` when the ref has no log. `reflog expire [--expire=<time>]
[--expire-unreachable=<time>] [--all] [--rewrite] [--updateref] [-n|--dry-run] [--verbose] [<ref>...]` removes lines older than
`--expire` (default `gc.reflogExpire`, 90 days) and older lines whose commits the ref no longer reaches (default
`gc.reflogExpireUnreachable`, 30 days; for `HEAD`, reachability is from any ref), leaving an empty file as git does. `<time>` is
`never`, `all`, `now`, a count of seconds, an ISO date or `N.days.ago`. `reflog delete [--rewrite] [--updateref] [-n]
[--verbose] <ref>@{N}...` removes single lines, one selector after the other as git does (so a later `@{N}` counts in the log
as the earlier one left it). `--rewrite` makes each kept line start where the one before it ended and `--updateref` points a
non-symbolic ref at the newest kept line when the newest was removed. `--verbose` prints `keep`, `prune` or `would prune` and
the message per line to standard error. Not built: `show --all`, other `log` options (`--format`, `--date`), and the per-ref
`gc.<pattern>.reflogExpire` settings.

### Config flag forms

`pyn config` with a git flag or a key (`--list`, `-l`, `--get`, `--get-all`, `--add`, `--unset`, `--unset-all`, `<key>`,
`<key> <value>`, with `--global`, `--system`, `--local` or `-f <file>`) reads and writes git's config files, not pyn's
settings: the system file (`/etc/gitconfig`, or `GIT_CONFIG_SYSTEM`; none with `GIT_CONFIG_NOSYSTEM`), the user's
(`GIT_CONFIG_GLOBAL`, else `$XDG_CONFIG_HOME/git/config` or `~/.gitconfig`) and the repository's `.git/config`. The
subcommands `list`, `get`, `set` and `init` keep addressing pyn's own store (the two stores stay apart, so a
subcommand never touches git config and a flag form never touches `config.toml`).

Without a scope flag, reads layer system, user and repository (the last value wins) and writes go to the repository's file;
outside a repository reads use the first two and a write is `fatal: not in a git directory` (`128`). Output, exit statuses
(`1` for a missing key or an invalid key, `5` for an `--unset` that finds nothing or several values, `255` for a file that
cannot be written) and the resulting files match git; edits keep every other line, comment and the order of the file.
`include` directives are not followed. Not built (usage error, `129`): `--replace-all`, `--get-regexp`, `--get-urlmatch`,
`--remove-section`, `--rename-section`, `--edit`, `--type` and its shorthands, `--show-origin`, `--show-scope`, `-z`,
`--name-only`, `--default` and value patterns. `crates/pyn-cli/tests/git_config_flags.rs` compares each form with real git.

### Inspection commands

`rev-parse`, `ls-files`, `ls-tree`, `cat-file`, `show-ref`, `shortlog`, `describe` and `name-rev` print what git prints, byte for
byte, with git's exit statuses; `crates/pyn-cli/tests/git_plumbing.rs` compares each with the real git on the same repository
(SHA-1 and SHA-256, loose and packed objects, from the root and from folders). They read the repository and never write it.
Options that are not listed here are a usage error (`129`), where git would accept them. Outside a git repository they fail
with `fatal: not a git repository (or any of the parent directories): .git` (`128`).

| Command | Supported |
| --- | --- |
| `rev-parse` | `<rev>` in any form `log` takes, plus `^{}`, `^{commit}`, `^{tree}`, `^{blob}`, `^{tag}`, `^{object}`, `<tree-ish>:<path>` (also `./` and `../` paths), `:<path>` and `:<n>:<path>` for the index, `^<rev>`, `<a>..<b>`, `<a>...<b>`, `<rev>^@`, `<rev>^!`; `--verify`, `-q`, `--short[=<n>]`, `--abbrev-ref[=strict\|loose]`, `--symbolic`, `--symbolic-full-name`, `--git-dir`, `--absolute-git-dir`, `--git-common-dir`, `--show-toplevel`, `--show-prefix`, `--show-cdup`, `--is-inside-work-tree`, `--is-inside-git-dir`, `--is-bare-repository`, `--is-shallow-repository`, `--show-object-format`, `--all`, `--branches`, `--tags`, `--remotes`. Options act on the revisions after them, as in git; a name that is no revision is printed back and must be a file. |
| `ls-files` | `-c`, `-d`, `-m`, `-o`, `-s`, `-u`, `-i` with `--exclude-standard`, `-t` and `-v` (the `S` tag marks a skip-worktree entry), `--sparse` (accepted, as pyn has no sparse index), `-z`, `--full-name`, `--error-unmatch`, and pathspecs (see [Pathspecs](#pathspecs)). Paths are relative to the current folder, and only that folder is listed unless a pathspec says otherwise. `-m` lists deleted files too, so `-m -d` lists them twice, as git does. |
| `ls-tree` | `-d`, `-r`, `-t`, `-l`, `-z`, `--name-only`, `--name-status`, `--object-only`, `--full-name`, `--full-tree`, `--abbrev[=<n>]`, and paths. Paths are literal (a `*` is an ordinary character), `:(top)`, `:/` and `:(literal)` are read, and any other magic is git's `pathspec magic not supported by this command` (`128`); `.` and `..` name a folder's contents. |
| `cat-file` | `-t`, `-s`, `-p`, `-e`, `<type> <object>` (tags are followed to the type), `--batch` and `--batch-check` with git's default format. |
| `show-ref` | `--head` (shown whatever the other options say, as git documents), `--heads`, `--tags`, `-d`, `-s`/`--hash[=<n>]`, `--abbrev[=<n>]`, `--verify`, `--exists`, `-q`, and patterns that match the end of a ref name on a `/`. |
| `shortlog` | `-n`, `-s`, `-e`, `-c`, a revision range and `-- <path>...`; with no revision it reads a `git log` on standard input unless that is a terminal, then it uses `HEAD`. Names and emails (of the committer with `-c`) are mapped by the [mailmap](#mailmap), also from a log on standard input, and outside a repository from a `.mailmap` in the current folder. |
| `check-mailmap` | `<contact>...` and `--stdin`, each as `Name <user@host>` or `<user@host>`; the canonical form is printed. A contact without angle brackets is a fatal error. |
| `describe` | `--tags`, `--all`, `--always`, `--long`, `--abbrev=<n>`, `--dirty[=<mark>]`, `--match`, `--exclude`, `--exact-match`, `--candidates=<n>`, and commit-ishes. The nearest tag is chosen by the walk git uses, including its limit of candidate tags. Describing a blob and `--first-parent`, `--contains` and `--broken` are not built. |
| `name-rev` | `--tags`, `--name-only`, `--no-undefined`, `--always` and commit-ishes. `--all`, `--stdin`, `--annotate-stdin`, `--refs` and `--exclude` are not built, and `name-rev` with no commit is a usage error where git prints nothing. |

Every abbreviated id these commands print (`--short`, `--abbrev`, `--hash=<n>`, `describe`, `name-rev --always`, the `blame` id
column, `worktree list`) follows the [abbreviation policy](#log-on-a-git-home): `core.abbrev` read as git reads it, the
object-count default when it is unset or `auto`, and a prefix lengthened until unique. An explicit `<n>` is a minimum of 4 or more.

### Mailmap

One mailmap is read for every command that maps identities, from the sources git reads, later ones overriding earlier
ones: the work tree's `.mailmap` (not when it is a symbolic link; not in a bare repository), the blob named by
`mailmap.blob` (`HEAD:.mailmap` in a bare repository when unset; a blob that does not exist is skipped) and the file
named by `mailmap.file` (`~/` and work-tree-relative paths; `~user/` is refused with `128`). The four line forms
(`Proper Name <commit@email>`, `<proper@email> <commit@email>`, `Proper Name <proper@email> <commit@email>` and
`Proper Name <proper@email> Commit Name <commit@email>`), comments and git's whitespace and malformed-line rules are read
as git reads them. Emails and names compare ignoring ASCII case, a line that names the commit's name wins over one that
names only the email, and a later line replaces the parts it sets. A line whose first email is empty is ignored.

It maps: `blame` and `annotate` (the author and committer columns and porcelain lines), `shortlog` (also `-e` and `-c`),
the `%aN %aE %aL %cN %cE %cL` placeholders of `log` and `show` (always, as in git), the `Author:` and `Commit:` lines of
the `short`, `medium`, `full` and `fuller` formats when `--use-mailmap` (or `--mailmap`) is given or `log.mailmap` is
true (the default; `--no-use-mailmap` and `--no-mailmap` turn it off, the last flag wins), the `rebase.instructionFormat`
placeholders, `--author` and `--committer` with the same condition, and `check-mailmap`. `%an`, `%ae`, `%al` and
`--pretty=raw` stay as recorded. With `--committer` git maps the committer line only when the author line mapped too, and
pyn follows that (observed on git 2.43). A pyn server does not map: `--use-mailmap` there is a usage error.

Not built: `cat-file --mailmap` and `--use-mailmap`, `for-each-ref` and `branch` `:mailmap` modifiers, `rev-list
--use-mailmap`, and `check-mailmap` contacts that are only an email.

### Storage maintenance

`count-objects`, `fsck`, `prune`, `prune-packed`, `repack`, `gc`, `maintenance`, `pack-refs`, `pack-objects` and `unpack-objects` keep a git home's `.git` healthy, so a repository used only
through pyn does not accumulate loose objects. What they write is git's format: `git fsck --full --strict` and
`git verify-pack` read the packs and indexes without complaint, and `crates/pyn-cli/tests/git_storage.rs` compares every
command with the real git on a twin of the same repository (SHA-1 and SHA-256; git 2.43 and 2.55). Nothing here runs git
or links a git library. Options not listed are a usage error (`129`).

| Command | Supported |
| --- | --- |
| `count-objects` | `-v`, `-H`: the loose count and size (from disk blocks, as git), `in-pack`, `packs`, `size-pack`, `prune-packable`, `garbage` (with `warning: garbage found:` lines), `size-garbage`, and `alternate:` lines. Sizes in `-H` round as git's do. |
| `fsck` | Reads every loose and packed object (`--no-full` skips packs, `--connectivity-only` skips content checks) and walks from the refs, `HEAD`, the reflogs (`--no-reflogs` leaves them out), the index and its cache-tree, and linked work trees. Prints `dangling`, or with `--unreachable` `unreachable`, lines (`--no-dangling` hides them), `missing` and `broken link from` lines, `root` (`--root`), `tagged` (`--tags`), and writes `lost-found/` (`--lost-found`). Object findings use git's ids and wording (`treeNotSorted`, `duplicateEntries`, `zeroPaddedFilemode`, `hasDotgit`, `badFilemode`, `missingAuthor`, `badTimezone`, `missingTaggerEntry`, ...); `--strict` turns warnings into errors and reports `100664` modes. Exit status is git's bit set: `1` bad object, `2` missing or unreachable pointer, `4` bad pack (also a finding in a packed object), `8` bad cache-tree. `--name-objects` appends `(<name>)` to `broken link` and `missing` lines and to `-v`'s `Checking <id>` lines, naming an object after the ref, `HEAD`, `<ref>@{<time>}`, index path (`:path`), cache-tree (`:`) or ancestry (`main^`, `main~2`, `main^2:dir/file`) that first reaches it, in git's order of roots and traversal. `-v` prints `Checking object directory`, one `Checking <type> <id>` per object, the `HEAD` links, reflog lines, cache-tree nodes and `Checking connectivity (<n> objects)` with git's object-table size, on standard error. `--progress` (and a terminal on standard error, unless `--no-progress` or `-v`) prints the `Checking object directories` and, with packs, `Checking objects` lines, each finishing with `done.` but without git's intermediate percentages. Output order follows the sorted object ids, not git's hash-table order, so the `dangling` and `Checking` lines compare as sets, and from inside a linked work tree git names roots `main-worktree/...` and prints absolute index paths, which pyn does not. Several work trees also repeat reflog lines in git. A shallow boundary, SHA-256 repositories and packs of several hundred objects are covered by the tests; the commit-graph checks (`Verifying commits in commit graph`) are not done. A pack that fails its checksum or index check is reported as one error. |
| `prune` | `-n`, `-v`, `--expire <time>` (`now`, `never`, a date, `2.weeks.ago`; the default removes every unreachable loose object) and `<head>...` to keep more alive. Keeps whatever objects newer than `--expire` refer to. Also removes stale `tmp_obj_*` and `tmp_pack_*` files. Lists `<id> <type>` for `-n` and `-v`. |
| `repack` | Without `-a`/`-A`, packs the reachable loose objects that no pack holds. `-a` packs everything reachable into one pack (objects in `.keep` packs excluded); `-A` also turns unreachable objects of packs it deletes into loose objects (`--unpack-unreachable=<time>` limits that to recent ones, as `gc` uses it, honouring the per-object times of a cruft pack's `.mtimes`). `-d` removes packs the new one replaces and loose objects that are now packed; `-l` leaves out and ignores objects borrowed from alternates; `-f` and `-F` stop existing deltas being reused (the default reuses them, as git does); `-q`, `-n`, `--threads` are accepted; `--cruft` (with `-a` or `-A`) writes the unreachable objects younger than `--cruft-expiration` into a cruft pack with its `.mtimes`, and `-k` keeps them in the new pack instead; `--window` and `--depth` (default `pack.window` 10, `pack.depth` 50) set the delta search. Prints `Nothing new to pack.` as git does. |
| `gc` | `pack-refs --all --prune`, `reflog expire --all`, `repack -d -l` (`-A` with `--unpack-unreachable=<gc.pruneExpire>`, or `-a` for `--prune=now`), `prune --expire <gc.pruneExpire>` (default 2 weeks; `--prune=<date>`, `--no-prune`) and `worktree prune --expire <gc.worktreePruneExpire>` (default 3 months). `--aggressive` uses `gc.aggressiveWindow` (250) and `gc.aggressiveDepth` (50) and recomputes deltas (`-f`). Unreachable objects go into a cruft pack that keeps their ages (`gc.cruftPacks`, on by default; `--no-cruft`; not with `--prune=now`), and `prune` then removes loose ones past the expiry. `gc.packRefs=false` skips packing refs. `--keep-largest-pack` (and `gc.bigPackThreshold`) leave those packs out of the repack. `gc` takes `gc.pid` (`<pid> <host>`) and refuses to run beside a live `gc` of that machine, or one on another machine, unless `--force` (`fatal: gc is already running on machine ...`, `128`; a lock older than 12 hours is ignored). A failed `gc --auto` writes `gc.log`; the next `gc --auto` prints it and stops until it is removed or older than `gc.logExpiry` (1 day), and a successful `gc` removes it. `--quiet`, `--detach`, `--cruft` and their `--no-` forms are accepted. `gc --auto`: see [Auto gc](#auto-gc). |
| `pack-refs` | `--all` (otherwise tags and already packed refs), `--no-prune`. Writes `packed-refs` with git's `peeled fully-peeled sorted` header, byte for byte, and removes the loose files it packed. `--include <pattern>` and `--exclude <pattern>` (repeatable, `*` also matches `/`) choose the loose refs to pack: `--include` replaces the default of tags, `--all` is `--include refs/*`. A `packed-refs` file is written even when nothing was packed, as git does. |
| `prune-packed` | `-n` (lists `rm -f <path>`), `-q`. Removes the loose objects some pack holds. |
| `pack-objects` | `<base-name>` or `--stdout`, objects (or with `--revs` revisions, `^rev` and `--not`, plus `--all`) on standard input, `--incremental`, `--local`, `--window`, `--depth`, `-q`; `--thin`, `--delta-base-offset`, `--no-reuse-delta` and `--no-reuse-object` are accepted. A named pack is written with deltas and its hash printed; `--stdout` writes whole objects. `git index-pack`, `unpack-objects` and `verify-pack` read both. Not built: `--keep`, `--include-tag`, `--max-pack-size`, `--filter`, `--cruft`, `--stdin-packs` and bitmap output. |
| `unpack-objects` | A pack on standard input becomes loose objects (objects already present are skipped); `-n` only checks it, `-q`, `--strict` and `-r` are accepted. |
| `maintenance` | `run [--auto] [--quiet] [--task=<task>]...` with the tasks `gc`, `loose-objects` (`prune-packed`, then one `loose-<hash>` pack of the loose objects, `maintenance.loose-objects.batchSize`, `--auto` after `maintenance.loose-objects.auto` loose objects) and `pack-refs`; with no `--task` the tasks `maintenance.<task>.enabled` turns on, else `gc`. `register` records the repository in the user config's `maintenance.repo` and sets `maintenance.auto=false` and `maintenance.strategy=incremental` in its config; `unregister [--force]` removes it. `maintenance.auto=false` also stops the automatic `gc` after commands. The `prefetch`, `commit-graph` and `incremental-repack` tasks, `start` and `stop` (a scheduler) are not built. |

Packs are written with offset deltas: a delta an object already has in a pack is kept when its base is packed too (and the
chain stays within `depth`); the rest, trees and blobs of one kind ordered by size and path-name hash, are each tried
against the previous `window` objects (`pack.window`), so a repacked history is close to git's in size. Every pack gets a
`.rev` file, and a cruft pack a `.mtimes`; `git fsck`, `verify-pack` and `cat-file --batch-all-objects` read them.

#### Auto gc

`gc --auto` does nothing unless `gc.auto` (default 6700; `0` turns it off) is exceeded, estimated as git does by counting the
loose objects in `objects/17` against `ceil(gc.auto / 256)`, or `gc.autoPackLimit` (default 50) is exceeded by the packs
without a `.keep`. Too many packs repacks everything (`-A`); too many loose objects packs only the loose ones. It runs in the
foreground and prints `Auto packing the repository for optimum performance.` unless `--quiet`. `pyn commit`, `pyn fetch`
(unless `--no-auto-gc` or `--dry-run`), `merge`, `pull`, `rebase` and `am` run it afterwards (not when `maintenance.auto` is `false`), and a failure is a warning, never the command's status.

Not built: commit-graph, multi-pack-index and bitmap files (git ignores their absence), `--geometric`, `--max-pack-size`
and `--max-cruft-size`.

### Patches

`format-patch`, `apply` and `am` write and read the same text as git, and `crates/pyn-cli/tests/git_format_patch.rs`,
`git_apply.rs` and `git_am.rs` compare each with the real git on twin repositories (SHA-1 and SHA-256): standard output, the
files written, exit statuses, whether standard error carries anything, the work tree, the index and, for `am`, the commit ids
and the session files in `.git/rebase-apply`, so git can carry on a session pyn stopped and the other way round. Settings and
options that are not listed are a usage error (`129`) where git would accept them; settings that change the result and are
not read (`format.*`, `am.*`, `apply.whitespace`, `apply.ignoreWhitespace`, `core.whitespace`) are refused the same way.

| Command | Supported |
| --- | --- |
| `format-patch` | `<since>`, `<a>..<b>`, `-<n>` (alone, or ending at a revision), `--root`, `--stdout`, `-o <dir>`, `-n`/`--numbered`, `-N`/`--no-numbered`, `--subject-prefix`, `--cover-letter`, `-s`/`--signoff`, `--signature`, `--no-signature`, `--no-stat`/`-p`, `-U<n>`, `--suffix`, `-q`, `--no-renames`. Merge commits are skipped. Files are named `0001-subject.patch` as git names them and listed on standard output. Subjects and names are wrapped and encoded (`=?UTF-8?q?...?=`) as git does, with the MIME headers when the message is not ASCII. With no revision it uses `@{upstream}`, and prints nothing when there is none. |
| `apply` | `--check`, `--stat`, `--numstat`, `--summary`, `--apply`, `--index`, `--cached`, `-R`, `-p<n>`, `-3`/`--3way`, `--whitespace=warn\|nowarn`, `--allow-empty`, patch files and `-` (standard input). |
| `am` | mbox files and standard input, `--continue`/`-r`, `--skip`, `--abort`, `--quit`, `--show-current-patch[=diff\|raw]`, `-3`/`--3way`, `-s`/`--signoff`, `-q`, `--whitespace=warn\|nowarn`, `-S`/`--gpg-sign`, `--no-gpg-sign`. |

`format-patch` ends each patch with `-- ` and `pyn <version>` where git prints its own version. A rename is a delete and a
create (the diff has no rename detection), and a binary change is a usage error (`129`). `--cover-letter` is refused (`129`)
when the range walks over a merge commit, because git's diffstat for that letter depends on what it walked. A commit message
with a long first paragraph is joined and wrapped at 78 columns as git does.

`apply` takes hunks exactly where git does: the context must match without fuzz, searched outwards from the line the hunk
names, and a hunk without leading or trailing context is held to the start or end of the file. Everything is checked before
anything is written, so a failure changes nothing (`1`; a malformed patch is `128`), including a patch that needs a folder where a file or a symbolic link is, or a file where a folder keeps files the patches do not remove. It handles creations, deletions, renames,
copies, mode changes, symlinks and several patches (or several files) in one input, applies to the work tree alone, to the work
tree and index (`--index`, which requires the work tree to match the index by content, where git compares file stat data) or to
the index alone, and reports whitespace errors in added lines (trailing whitespace, a space before a tab, blank lines at the end
of the file) as `--whitespace=warn` does, but only when it applies the patch, as git does. `--3way` merges each file against the
blob the patch's `index` line names; conflicts are left as `ours`/`theirs` markers (with a base section when
`merge.conflictStyle` is `diff3`; `zdiff3` is a usage error) and as unmerged index entries, with `U <path>` and exit `1`.
Binary patches (`GIT binary patch`), submodule patches and these options are a usage error (`129`): `--reject`, `-C`,
`--ignore-space-change`, `--ignore-whitespace`, `--unidiff-zero`, `--directory`, `--include`, `--exclude`, `-z`, `--recount`,
`--inaccurate-eof`, `--build-fake-ancestor`, `-N`, `--unsafe-paths`, `-v`, `-q`, `--no-add` and `--whitespace=error`, `fix`
and `strip`. A `Binary files a and b differ` patch without data is refused at apply time as git refuses it. `apply` runs only
inside a git repository.

`am` reads UTF-8 and ASCII emails (`Content-Transfer-Encoding` 7bit or 8bit, no multipart) with RFC 2047 headers and the
author, date and subject taken from the email or from the first lines of its body; the subject loses `Re:` and `[...]` tags, and
the message ends at the `---` line. It applies each patch with `apply --index`, commits it with the author and date of the
email and the committer from the configuration, logs `am: <subject>` and keeps the session in `.git/rebase-apply` with git's
file names. Commits are signed when `commit.gpgsign` is set or `-S` is given (`--no-gpg-sign` overrides the setting), with
`-S` applying to the invocation that gives it, as in git. On a failure it prints `Patch failed at <nnnn> <subject>` and the
lines telling how to go on (naming `pyn am`), and exits `128`; `--continue` commits the index, `--skip` resets to `HEAD` and
goes on, `--abort` restores `ORIG_HEAD`. `--3way` rebuilds a base from the blobs in the patch's `index` lines, applies the
patch to it, and merges the result with the current tree as `cherry-pick` merges: conflict markers are diff3 style whatever
`merge.conflictStyle` says, and no `Removing <path>` lines are printed (older git, which merges with merge-recursive, prints them). A file meeting a folder is moved to `<path>~HEAD` (or `<path>~<subject>`) while its unmerged entries stay at `<path>`, as git leaves them. Maildirs, StGit and Mercurial formats, `-k`, `-b`, `-c`/scissors, `--ignore-date`, `--committer-date-is-author-date`,
`-i`, `--rerere-autoupdate`, `--allow-empty` and `-C`/`-p` are not built (a patch with no changes stops the session with
`Patch is empty.` and the hint to record an empty commit is left out). The apply hooks run as in [Client hooks](#client-hooks);
`-n`/`--no-verify` skips `applypatch-msg` and `pre-applypatch` for the invocation that gives it.

### What pyn will not write

Any command that would write a work tree file with `working-tree-encoding` (other than UTF-8) stops first and changes
nothing (exit `128`, naming the path), because pyn does not apply it. `apply` and the patch-mode commands that write the work
tree (`checkout -p`, `restore -p`) also stop for a path whose line endings, `ident` or filter git would convert, since they
copy bytes as they are. Filters run as described in [Filter drivers](#filter-drivers).
The hooks pyn does not run yet (see [Client hooks](#client-hooks)) draw one warning naming the ones that exist
for the command. The split index, a sparse index and `GIT_DIR` are not supported, and the commands that talk to a remote are `ls-remote`, `fetch`, `pull`, `push`, `clone`, `remote show`, `update` and `prune`, and the submodule commands that clone or fetch (see [Transports and credentials](#transports-and-credentials)).

### Client hooks

A git home runs the repository's client hooks as git does, as the user's own processes: no sandbox, in the work tree root, with
standard output sent to standard error and no standard input except where git gives one. A hook is found in `core.hooksPath`
(relative to the work tree root, `~/` expanded) or `<common git dir>/hooks`; only a file with an executable bit counts, so `*.sample` files
are ignored and a non-executable hook earns git's `The '<name>' hook was ignored` hint (silenced by `advice.ignoredHook=false`). pyn
never copies hooks: `clone` creates none, as git's templates give none, so a freshly cloned repository runs no hook until the user installs one.
`pyn.hooks=false` in the git configuration turns every hook off, and the warning naming the hooks that exist returns.

| Hook | Arguments and input | Effect of a nonzero status |
| --- | --- | --- |
| `pre-commit` | none; `GIT_INDEX_FILE`, `GIT_AUTHOR_*`, `GIT_EDITOR=:` | `commit` stops, exit `1`; skipped by `--no-verify`. Runs before the "nothing to commit" check, as in git. With `commit -a` the index is `<git dir>/index.lock`. |
| `pre-merge-commit` | none | `commit` concluding a merge runs it instead of `pre-commit`; a clean `merge` runs it before committing and a veto leaves the merge in progress (exit `1`). Skipped by `--no-verify`. |
| `prepare-commit-msg` | `.git/COMMIT_EDITMSG` (`.git/MERGE_MSG` for a merge) and the source: `message` (`-m`, `-F`), `commit <rev>` (`-C <rev>`, or `HEAD` for `--amend`), `merge`; the hook may rewrite the file | Stops the commit, exit `1`. Not skipped by `--no-verify`. |
| `commit-msg` | the message file; may rewrite it | Stops the commit, exit `1`; skipped by `--no-verify`. |
| `post-commit` | none | Ignored. Runs after the ref moves. |
| `post-rewrite` | `amend` or `rebase`; `<old> <new>` per line on standard input | Ignored. `amend` after `commit --amend` and after each commit a rebase's `squash` or `fixup` replaces; `rebase` once when a rebase ends, with the pairs of its `rewritten-list` (none, no run). `cherry-pick` and `am` do not run it, as in git. |
| `pre-rebase` | the upstream as typed (the configured upstream's ref when none; `--root` for `--root`; the commit id for `pull --rebase`) and the branch when one is given | `rebase` stops, exit `128`; skipped by `--no-verify`. Not run when the branch is already up to date and not forced (`-f`), nor when `--continue`, `--skip` or `--abort` resume. Runs after the check for local changes. |
| `applypatch-msg` | `.git/rebase-apply/final-commit`, which it may rewrite; no `GIT_INDEX_FILE` | `am` stops with the session kept, exit `1`; skipped by `-n`/`--no-verify`. Runs once per patch, before it is applied, not again by `--continue`. |
| `pre-applypatch` | none | `am` stops with the patch applied to the index and the session kept, exit `1`; skipped by `-n`/`--no-verify`. Runs before the commit, also for `--continue`. |
| `post-applypatch` | none | Ignored. After the commit. |
| `post-merge` | `0` | Ignored. After a merge that fast-forwards or commits, not after `commit` concluding one. A `--squash` merge runs it with `1`. |
| `post-checkout` | `<old HEAD> <new HEAD> <flag>`: `1` for `switch` and `checkout` of a branch or commit, `0` for `checkout -- <paths>` and `restore`; the all-zero id stands for no commit. `clone` (not `--bare`, `--no-checkout` or an empty remote) runs it with the zero id, the new `HEAD` and `1`, with `GIT_DIR` set and no `GIT_PREFIX`; `worktree add` (not `--no-checkout` or `--orphan`) runs it the same way from inside the new work tree, with `GIT_PREFIX` as the command's directory | The command exits `1` after finishing its work, as git's does. |
| `reference-transaction` | `prepared`, `committed` or `aborted`; `<old> <new> <ref>` per ref on standard input (see below) | `prepared` refused: the update is not made, `aborted` is reported, and the command fails (`128`). `committed` and `aborted` are ignored. |
| `pre-auto-gc` | none | `gc --auto` (and the automatic `gc` after a command) does nothing and exits `0`. Runs once the repository needs a `gc` and before the "Auto packing" message; `gc` without `--auto` does not run it. |
| `pre-push` | `<remote name or URL> <url>`; one line `<local ref> <local id> <remote ref> <remote id>` per ref about to be sent on standard input (`(delete)` and the zero id for a deletion; refs that are up to date or rejected are left out); also run by `--dry-run` | `push` sends nothing and exits `1`; skipped by `--no-verify`. |

The environment also carries `GIT_PREFIX` (the directory the command was run from, relative to the work tree root, with a trailing `/`, or empty) and
whatever the caller had. `GIT_DIR` and `GIT_WORK_TREE` are not set. Oracle tests run one recording script under git and under pyn on git 2.43 and 2.56 and compare
arguments, standard input, working directory, environment and the resulting exit status and repository state.

`reference-transaction` runs around every ref update pyn makes on a git home, one transaction per update: a branch or tag created, moved or
deleted, the remote-tracking refs `fetch`, `pull` and `push` update, `ORIG_HEAD`, and the commit, merge, reset, `switch`/`checkout` and
`cherry-pick` moves of a branch. A move of the branch `HEAD` is on lists `HEAD` first, then the branch, as git does; a detached `HEAD`
moves as `HEAD` alone. The old id is the ref's current id, or the zero id for a new ref, for `ORIG_HEAD` and for the forced resets of
`branch -f`, `switch -C`, `checkout -B` and `worktree add -B` (git passes no old id there). A ref that changed under pyn before the
update fails without running the hook. A commit's hooks see `GIT_AUTHOR_*` as git exports them. Differences, none of them compared
by the tests beyond the set of refs: switching `HEAD` to another branch (a symbolic ref update) runs nothing, as in git 2.43 (newer git
may report it); a deletion reports the old id and the zero id, where git 2.43 reports the zero id twice and adds `aborted` and repeated
`prepared`/`committed` runs for its `packed-refs` pass; a `clone` or `worktree add` reports its ref
creations only when a hook is installed by `core.hooksPath` (never by a template, which pyn does not copy), one transaction per ref and not in git's grouping; `rebase`, `cherry-pick`,
`revert`, `stash` and `am` do not report their pseudo refs (`CHERRY_PICK_HEAD`, `REBASE_HEAD`, `MERGE_HEAD`, `AUTO_MERGE`), `rebase`'s
`HEAD` detach and the `reset --hard` a `stash` makes; `git update-ref` and `symbolic-ref` have no pyn command.

Replaying commands run the hooks git runs for them. `cherry-pick` and `revert` (and the picks of `rebase`) make each commit
with `prepare-commit-msg` (`.git/COMMIT_EDITMSG` and `message`; `GIT_INDEX_FILE=.git/index`, `GIT_EDITOR=:`, no `GIT_AUTHOR_*`)
and then `post-commit`; no `pre-commit` or `commit-msg` runs, and `-n` runs nothing. A refusal by `prepare-commit-msg`
leaves the pick stopped (`CHERRY_PICK_HEAD`, `MERGE_MSG`, and the `sequencer` directory for several commits; a `revert` leaves no
`REVERT_HEAD`) and ends with `128`. `--continue` commits the resolved index as `git commit` does: `pre-commit`,
`prepare-commit-msg` (source `merge`), `commit-msg`, `post-commit`, with `GIT_AUTHOR_*` and a refusal ending with `1`. In a
rebase a refused pick stops it for `--continue` (exit `1`); `--continue` after a conflict opens the editor after
`prepare-commit-msg` (source `message`) and runs `post-commit`. A `squash` or `fixup` replaces the commit `HEAD` is on: the
message hook, `post-commit` and `post-rewrite amend` run per step, the last of a chain with the editor opened after
`prepare-commit-msg`. Oracle tests run one recording script under git and under pyn and compare arguments, order, standard
input, environment, exit status and repository state; a rebase's `post-checkout` hook and the messages the editor shows are
not compared.

Not built, and warned about when they exist: the commit hooks (`pre-commit`, `prepare-commit-msg` with source `commit HEAD`,
`commit-msg`, `post-commit`, `post-rewrite amend`) for a `reword`, for the merge commits of `--rebase-merges`, and for the
commit a rebase makes when `--continue` finishes an `edit` or a `squash` chain; `post-checkout` during a rebase;
`reference-transaction` for the pseudo refs and the rebase and stash moves listed above. `post-update`, `pre-receive`,
`update` and `push-to-checkout` are server side and never run by a client.

### Git LFS

`filter=lfs` is pyn's own implementation (decision #775): no `git-lfs` program and no LFS library is used, and
`filter.lfs.clean`, `smudge` and `process` are ignored whether or not they are configured (`filter.lfs.required` is honoured).
`clean` turns work tree content into a pointer file and stores the content in `.git/lfs/objects/<aa>/<bb>/<oid>` (the
common directory of a linked work tree; `<aa>` and `<bb>` are the first two pairs of the SHA-256 hex id), written through
`.git/lfs/tmp`:

```text
version https://git-lfs.github.com/spec/v1
oid sha256:<64 lowercase hex digits>
size <bytes>
```

Content that already is a pointer file (at most 1024 bytes, `version`, `oid sha256:` and `size` lines, a trailing newline) and
empty content are passed through unchanged, so `status` sees pointers as unmodified. `smudge` turns a pointer file into the
stored object; a pointer whose object is not in the local store stays as the pointer file, with one warning on standard error
per invocation, because the git home cannot download LFS objects yet (its Batch API client is not built; a pyn server already serves the Batch API, see [Git LFS](lfs.md)). A stored object with the wrong size
or hash is an error. `GIT_LFS_SKIP_SMUDGE=1` (or `true`) leaves pointers as they are. `fetch` and `pull` also warn once when
the `.gitattributes` in the tip of a ref that moved applies `filter=lfs` to a path whose pointer names an object that is not
stored (`clone` in the commit it checks out, none for `--bare` or `-n`); only `.gitattributes` files in the tree are read for
that check. The warning wording is not part of the contract. Covered by `crates/pyn-cli/tests/git_lfs_client.rs` (the pointer
bytes are also compared with `git lfs pointer` when `git-lfs` is installed and skipped otherwise) and
`crates/pyn-cli/tests/git_lfs.rs` (#311, #628).

Not built on the git home: downloading and uploading objects (the Batch API client, `lfs.url`, transfer adapters, credentials), the
locking API client and `lockable`, `lfs.storage`, `lfs.fetchinclude` and `fetchexclude`, pointer extensions (`ext-*` lines), and the `git lfs`
commands (`track`, `fetch`, `pull`, `push`, `ls-files`, `prune`, `lock`, `unlock`, `locks`).

### Refused commands

Only the commands that act on the repository in the working directory refuse, with exit `128` and this message, naming the
home that has them:

```text
fatal: `pyn lock` needs a pyn server, and this repository is a git repository; `pyn convert --home pyn` will move it to one
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

Two commands fail on a git home with exit `1`, as the other git-only commands do:

| Command | Why |
| --- | --- |
| `update` | Says it needs git transport (#255). |
| `get` | Per-path revisions, which git has no equivalent of; not scheduled. |

`pyn convert` adopts the repository as a git home; moving it to a pyn server is `pyn convert --home pyn`, see [Moving to a pyn server](#moving-to-a-pyn-server).

### Packs (library)

`pyn-git` holds the pack plumbing the git transport uses (see [Transports and credentials](#transports-and-credentials));
`pyn-gittransport` calls it; `pyn fetch` reads packs through it and `pyn push` sends packs it writes with `write_pack`.

- **Ingest.** `index_pack` reads a pack from any stream, checks the header, every entry's zlib data, the object count and the
  trailing checksum, resolves offset and reference deltas, and computes each object id with git's rules (SHA-1 with collision
  detection, or SHA-256). A thin pack is completed: bases found in the object database are appended to the stored pack, so the
  result is self-contained. The pack and its version 2 index are written to `objects/pack` through temporary files and renames,
  and nothing is left behind on error. Limits cover the object count, object size, delta chain depth and pack size.
- **Write.** `write_pack` writes the given objects as an undeltified version 2 pack and can produce the matching index.
  `PackWriter` takes one entry at a time, whole or as an offset delta; `ObjectDb::write_packfile` orders objects, searches
  for deltas in a window and installs the pack and index. `ObjectDb::loose_objects` and `pack_garbage` enumerate the store,
  and `RefStore::pack_refs` writes `packed-refs`.
- **Walk.** `reachable_objects` lists what `want` needs that `have` does not already imply (tags, commits newest first, then
  trees and blobs), as `git rev-list --objects --objects-edge-aggressive want ^have` does, with the boundary commits.
  A `have` this repository does not hold is ignored; any other missing object is an error. `commits_newest_first` and
  `existing` support negotiation.

## Open items

- Unknown flags on supported commands are a usage error (`129`) from the argument parser, not a message that says the git
  flag is unsupported; the remaining gaps are tracked in #223 and #224.
- On a pyn server the output shapes of `pyn status` and `pyn commit` are not aligned with git's, and `-C <dir>` and other
  global git flags are not taken.
