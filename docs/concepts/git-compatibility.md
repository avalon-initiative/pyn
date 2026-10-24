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
Messages that git writes to standard output (for example the nothing-to-commit notice and the unmerged-path listing of a
conflicted commit) go to standard output, and errors to standard error. `fetch`, `pull` and `push` outside any repository fail
with `fatal: not a git repository (or any of the parent directories): .git` (`128`). The statuses are checked against git by
`crates/pyn-cli/tests/git_exit_status.rs`, whose allow-list names the differences that remain. Where git exits `128` for a bad
value of an option of `log` or `show` (a missing `--since` value), and `255` for a failed
`reflog expire` or `reflog delete`, pyn does the same.

Deliberate status differences that remain:

- **`checkout -B` is not built**, so `checkout -b <a> -B <b>` is a usage error (`129`) where git says the options cannot be
  used together (`128`).
- **An unknown option of `log`, `show` or `reflog`** is a usage error (`129`); git exits `128`. The parser cannot tell an
  option git lacks from one pyn does not build, and the unbuilt ones are `129` by design.
- **`status --porcelain=<version>`** takes only `v1`; `v2` is unbuilt and an unknown version such as `v9` (git: `128`) is
  the same parser rejection, a usage error (`129`).
- **`pyn diff` outside a repository** asks for a server (`128`); git prints its `--no-index` usage (`129`). pyn has no
  `--no-index` form.

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
| `git init` | `pyn init [<dir>]` | different | Starts a pyn server-home repository, not a git one: creates `<dir>` if missing (the current folder if omitted) with `.pyn/pyn.toml` (every path `shared`, with a commented example of an exclusive rule) and `.pyn/local_only/` holding `repo.toml` with `repo_tool = "pyn"`, and prints `Initialized empty pyn repository in <path>/.pyn`. In a folder that already has a `.git` or `.pyn`, or inside a git work tree or pyn workspace, it fails (`128`) and, for a git repository, names `pyn convert`. With a server set (`--server`, `PYN_SERVER` or the user setting) and `--repo owner/name`, it asks `Create owner/name on <server>? [y/N]` on a terminal (default no); `--create-on-server` creates it without asking, with the same call as `pyn repo create` and the folder's `.pyn/pyn.toml` as the first policy, and records the server and repository in `.pyn/local_only/config.toml`; `--no-create-on-server` never asks. `--create-on-server` without a server and `--repo` is a usage error (`129`). Without a terminal nothing is created on the server (decision #562). |
| `git init [<dir>]` | `pyn init-git [<dir>]` | same | Starts a new repository: a real `.git` written by pyn (no git program) plus `.pyn/local_only/repo.toml` with `repo_tool = "git"`, and `.pyn/` hidden in `.git/info/exclude`. Creates `<dir>` if missing and prints `Initialized empty Git repository in <path>/.git/`. `-b`/`--initial-branch <name>`, `--object-format sha1\|sha256` and `-q`/`--quiet` work as in git. Divergence: with no `-b` and no `init.defaultBranch` the first branch is `main` (git's default is `master`). `--bare`, `--template`, `--separate-git-dir` and `--shared` are a usage error (`129`). In a folder that already has a `.git` or `.pyn`, or inside a git work tree or pyn workspace, it fails (`128`) and names `pyn convert` (decision #562). |
| | `pyn convert [--home git\|pyn] [-y]` | pyn only | Without `--home`, adopts an existing git repository (hides `.pyn/` in `.git/info/exclude`, creates `.pyn/local_only/`, records `repo_tool = "git"`; safe to run again); outside a git repository, or in a pyn server workspace, it fails with `fatal: ...` (status 128). `--home git` in a pyn server workspace moves it to a git home keeping its history, see [Leaving a pyn server](#leaving-a-pyn-server). `--home pyn` is not built (it needs #95): it fails with `fatal: moving to a pyn server home is not built yet (#572)` (status 128) and changes nothing. |
| `git clone <repo> [<dir>]` | `pyn clone <source> [<dir>]` | same | A source that is `<server>/owner/name` or `owner/name` on the configured server is cloned from a pyn server, and prints one summary line, not progress. A git URL (`https://`, `ssh://`, `user@host:path`) is cloned into a git repository with git's behaviour, flags and output; see [Clone from a git URL](#clone-from-a-git-url), which also says how an `http(s)` address is told apart. |
| `git clone -b`, `--bare`, `-o`, `-n`, `--single-branch`, `--no-tags`, `-c`, `-q`, `--progress` | `pyn clone ...` | git URL: supported | Git's behaviour. On a pyn server clone they are a usage error (`129`): that clone takes everything the server has, on its default branch. |
| `git clone --depth`, `--shallow-*`, `--filter`, `--mirror`, `--recurse-submodules`, `--sparse`, `--reference`, `--separate-git-dir`, `--template`, `--local`, `--no-hardlinks` | none | unsupported | Usage error (`129`) naming the flag: shallow, partial, mirror and submodule clones are not built, and `--local`, `--no-local`, `--no-hardlinks` and `--shared` are not taken: a local-path source is cloned through the transfer protocol, see [Local paths](#local-paths-and-file-urls). |
| `pyn clone --revision <commit>` | | pyn only | Clone as of a commit (a hash, `HEAD~N`). Git does this with `clone` then `checkout`. |
| `pyn clone --no-local-store` | | pyn only | A pyn server clone keeps a partial local store of the history under `.pyn/local_only/store/` and commits locally by default (`--local-store` says the same); this opts out and commits straight to the server. See [the local store](workspace.md#the-local-store). A usage error for a git URL. |
| `pyn migrate` | | pyn only | Converts a workspace made before local commits; see [migrating](workspace.md#migrating-a-workspace). |
| `git status` | `pyn status` | different | Header `On branch <name>`, then a table with mode, revision, staged, state, lock and path plus a summary line, because mode and lock state have no place in git's layout. `STAGED` is `added` or `modified` for a staged path, and `STATE` then describes what changed after it was staged. Lock state is shown in this default table's lock column and by `pyn lock`/`pyn locks`, not in `-s`/`--porcelain` output. On a git home the table is `STAGED`, `UNSTAGED`, `PATH` with git's states. |
| `git status -s`, `--short`, `--porcelain[=v1]`, `-b`, `--branch`, `<pathspec>...` | same | same | Git's `XY <path>` lines; see [the short format](#the-short-status-format). Pathspecs are literal files and folders, taken relative to where you stand, and limit every format. `-z`, `--porcelain=v2`, `--ignored`, `-u<mode>`, `--ahead-behind` and glob or magic pathspecs are a usage error (`129`). `pyn status --revision` fails on a git home. |
| `pyn status --revision <commit>` | | pyn only | Compare the workspace with a past commit (`behind` / `ahead`). |
| `git add <path>...` | `pyn add <path>...` | same | Stages a path, a folder or `.`; a path is required unless `-u` or `-A` is given. On an exclusive path of a pyn server it fails unless you hold the lock (see below). |
| `git add -u`, `-A`, `-n`, `-v`, `-f` and their long forms | same | same | `-u` and `--update` stage tracked files only; `-A` and `--all` take new files too and, with no path, cover the whole repository. `-n` and `--dry-run` print `add '<path>'` and `remove '<path>'` and stage nothing. Ignored files are refused (exit `1`, as git) without `-f`. |
| `git rm [-r] [-f] [-n] [-q] [--cached] [--ignore-unmatch] [--] <path>...` | `pyn rm ...` | supported | Removes tracked files from the work tree and stages the removal. On a git home it is git's `rm`, see [Removing and moving](#removing-and-moving). On a pyn server it stages a deletion for the next commit, and an exclusive path needs your lock first. |
| `git mv [-f] [-n] [-k] [-v] <source>... <destination>` | `pyn mv ...` | supported | Renames a file or folder, or moves them into an existing folder. On a git home it is git's `mv`; on a pyn server it stages a deletion at the old path and an addition at the new one. See [Removing and moving](#removing-and-moving). |
| `git add -p` | `pyn add -p` (`--patch`) | git home only | Asks about each hunk with git's answers, see [Choosing hunks](#choosing-hunks). On a pyn server it is a usage error (`129`): a pyn server stages whole files. |
| `git add -i`, `-e`, `--intent-to-add`, `--chmod`, `--renormalize`, `--pathspec-from-file`, glob and magic pathspecs | none | unsupported | Usage error (`129`), or `pathspec ... did not match any files` for a glob the shell did not expand. A path is a file or a folder, taken relative to where you stand. |
| `git commit -m <msg>` | `pyn commit -m <msg>` | different | Commits what is staged as one unit of work. On a pyn server it is one changeset uploaded to the server, so commit and publish are one step (no `push` needed), it releases the locks it held, and the output is `<path> is now at revision N` for each file. On a git home it is a git commit on the current branch (or on a detached `HEAD`, which moves only `HEAD`) and prints `[<branch> <id>] <subject>`, with `detached HEAD` in place of the branch name when detached. `-m` is required (no editor) and repeats for more paragraphs. Nothing staged exits `1` with a message. |
| `git commit -a`, `-am <msg>` | `pyn commit -a`, `-am <msg>` | same | Stages every changed tracked file, then commits; new files are not included. |
| `git commit <path>...` | `pyn commit -m <msg> <path>...` | different | On a pyn server: one atomic commit of the named paths as they are on disk, whatever is staged (git's `--only`). Not available on a git home, which says to `add` the paths first. |
| `git commit -F <file>` (`-` is standard input) | same | git home: supported; pyn server: supported | The same message reading and cleanup on both homes, with and without the local store; `-m` and `-F` together are refused. On a pyn server `--file` stays the content file (it has no short form, so `-F` does not clash). See [Commit flags on a git home](#commit-flags-on-a-git-home). |
| `git commit --file=<file>`, `--allow-empty`, `--author=<name <email>>`, `--date=<date>`, `-s`, `--signoff`, `--amend`, `-S[<keyid>]`, `--gpg-sign[=<keyid>]`, `--no-gpg-sign`, `--allow-empty-message`, `--cleanup=<mode>`, `--reset-author`, `--trailer <trailer>`, `-C <commit>`, `--reuse-message=<commit>` | same | git home: supported | Git's behaviour on a git home; on a pyn server a usage error (`129`) naming the flag, because commits there are immutable (no `--amend`) and `--file` is the content file. `--allow-empty` is refused there because the server rejects a pushed commit that changes no file. `--author` and `--date` are the exception: a pyn server workspace with the local store takes them on a commit of staged changes (author and author date only; the committer stays the signed-in account, the audit actor the pusher, and they survive a push), and refuses them without the local store, on a path commit and on a merge conclusion. `-s` waits until accounts carry emails. See [Commit flags on a git home](#commit-flags-on-a-git-home). |
| `git commit -q`, `--quiet`, `-n`, `--no-verify`, `--verify`, `-v`, `--verbose`, `--status`, `--no-status`, `--no-edit` | same | supported | Both homes. `-q` prints no summary (errors and warnings stay); the rest are accepted and ignored because pyn runs no hooks and opens no editor. On a git home `--no-verify` also leaves out the warning about the `pre-commit` and `commit-msg` hooks. |
| `git commit -c <commit>`, `--reedit-message`, `-e`, `--edit`, `-t <file>`, `--template`, `--fixup`, `--squash`, `--dry-run`, `--short`, `--branch`, `--long`, `--porcelain`, `-z`, `--ahead-behind`, `-u`, `--untracked-files`, `-i`, `--include`, `-o`, `--only`, `-p`, `--patch`, `--interactive`, `-U`, `--unified`, `--inter-hunk-context`, `--pathspec-from-file`, `--pathspec-file-nul`, `--no-post-rewrite`, `--post-rewrite`, `--no-quiet`, `--no-verbose` and the other `--no-` forms git generates for its options | none | unsupported | Usage error (`129`). Each needs an editor, a status report, or a path selection `commit` does not have: use `add` to choose paths and `-m`/`-F` for the message. |
| `pyn commit --file <local>` | | pyn only | Content file for a single path. In a workspace the file at the path is the default. |
| `pyn commit --base <rev>` | | pyn only | The revision the edit started from; the server rejects the commit if the branch has moved on from it on an exclusive path. |
| `git log [-n <n>] [<path>]` | `pyn log [-n <n>] [<path>]` (alias `history`) | different | Without a path, newest first, 50 by default. With a path, oldest first and all revisions (git prints newest first). It lists the workspace's branch (`On branch <name>` goes to standard error). Output is a table of per-path revisions, not commit blocks. `-n` is also `--max-count`; `--limit` is an alias. On a git home it is git's output (see below). |
| `git log --author`, `--grep`, `-i`, `--since`, `--until`, `<rev>`, `<a>..<b>`, `^<rev>`, `[<rev>...] -- <path>` | same | supported | Git's output on a git home (see [Log on a git home](#log-on-a-git-home)). On a pyn server the server filters the commits (`GET /commits` parameters, see [commits](commits.md#filtering-commits)), so the output is the commit listing of `--commits` (or the given format), never a filtered page of a longer history. The differences from git: `--author` and `--grep` are a substring, always ignoring case (`-i` is accepted and changes nothing), and a pattern with `\ [ * ^ $` is a usage error; `--author` matches the name or email of the author; `--since` and `--until` compare the author date, inclusive, and take the date forms of a git home (RFC 3339, `YYYY-MM-DD`, `2 days ago`, ...); a revision is `HEAD`, a commit hash (4 digits or more) or a [local tag](#tags) (workspaces with the local store), each optionally followed by `~N`, `^`, so branch names are not revisions there; a tag of an unpublished local commit is refused with a message (the listing is the server's history), and a path with a tag's name is a path unless `--` follows the revisions; one path only, and not with diff output (`--stat`, `-p`, ...: usage error, `129`); unpublished local commits are not listed when a filter, revision or path is given. |
| `git log --committer`, `--invert-grep`, `--all-match`, `--no-merges`, `--merges`, `--skip`, `--reverse`, `<a>...<b>`, a repeated `--author` or `--grep`, more than one path | same | git home: supported | Git's output on a git home. On a pyn server a usage error (`129`) naming the option (`<a>...<b>` fails with `1`): the server has no committer, merge, count or symmetric filters. |
| `git log --stat`, `--shortstat`, `--name-only`, `--name-status`, `-p`, `-U<n>` | same | supported | Both homes. The diff of each commit against its first parent follows its text, as `show` prints it (a merge shows none); `--stat` with `-p` separates them with `---`. On a pyn server they list commits like `pyn log --commits` (`-n` or 50, medium format unless a format is given) and take no `--filter` (usage error, `129`); a path or revision narrows the commits (see the filter row above); unpublished local commits are diffed from the local store. Exclusive paths show as in `show`. `--stat`, `--shortstat`, `--name-only` and `--name-status` combine with no other of the four (usage error, `129`), and `-p` with a name flag too. |
| `git log --oneline`, `--format=`, `--pretty=`, `--date=` | same | supported | Both homes, through one renderer (the formats and `%` placeholders of [Log on a git home](#log-on-a-git-home)). On a pyn server they list commits like `pyn log --commits` (newest first, `-n` or 50) and take no `--filter` (usage error, `129`); a path or revision narrows the commits (see the filter row above); `%d` and `%D` are empty there, as no refs are known. | |
| `git log --graph`, `--follow`, `--decorate`, `--all`, `-S`, `-G`, `--first-parent`, `--show-signature`, `--notes`, `-E`, `-F`, `-P`, `-<n>` | none | unsupported | Usage error (`129`). Rename-aware output is #274. `pyn log` is of the workspace's branch; another branch's log needs a switch (`pyn log <branch>` is not built). |
| `pyn log --commits` | | pyn only | List commits (`COMMIT`, `AUTHOR`, `WHEN`, `FILES`, `MESSAGE`) instead of revisions. |
| `pyn log --filter <glob>` | | pyn only | Only paths matching the glob. |
| `git diff [<c1> [<c2>]] [-- <path>...]` | `pyn diff [<c1> [<c2>]] [-- <path>...]` | different | Unified output as git, computed in the CLI. Commits are hashes (or abbreviations of 4 or more digits), `HEAD`, `~N`, `^`, or `a..b`; two commits work outside a workspace. With no commit it compares the work tree with the index: the staged content, or the last update for what is not staged. File modes print as in git, including `old mode` and `new mode`. On a git home an argument that is neither a revision nor an existing path (a deleted file needs `--`) fails with `fatal: ambiguous argument '<arg>'` (or `bad revision` before `--`), exit `128`, as git does; `a...b` is not supported and fails the same way. |
| `git diff -U<n>`, `--unified=<n>`, `--stat`, `--name-only`, `--name-status`, `--exit-code`, `-q` | same | same | `--name-status` prints `A`, `M`, and `D`; gitlink type changes print `T`. On a git home `--stat` (also in `show`, `log` and `stash show`) prints a binary file as git does, `Bin <old> -> <new> bytes` (`Bin` alone for a rename with identical content, as in the merge diffstat), aligned in the graph column and left out of the insertion and deletion totals; `--numstat` prints `-` and `-`. `diff` and `show` list a moved file as a deleted and an added one, where git pairs them as a rename. |
| `git diff --numstat` | same | different | Both homes. Prints added and deleted line counts per path, or `-` and `-` for a binary file. It combines with none of `--stat`, `--shortstat`, `--name-only`, `--name-status` or `--check` (usage error, `129`); git can combine numeric and other stats or give another format precedence. |
| `git diff --cached`, `--staged` | `pyn diff --cached`, `--staged` | different | The staged content against the last commit (the last update on a pyn server). On a git home it takes one optional commit to compare with, as git; on a pyn server it takes none. |
| `git diff --shortstat`, `-w` (`--ignore-all-space`), `--check` | same | same | Both homes. `--shortstat` prints only the totals line. `-w` compares lines with all whitespace removed, prints context lines as they are in the new file, and leaves out a file that differs only in whitespace (so `--exit-code` is `0` when nothing else changed); `--name-only` and `--name-status` still list it, as git 2.43 does (newer git versions differ on this edge; pyn lists the file). `--check` prints `path:line: trailing whitespace.` (or `space before tab in indent.`, or both) and the added line, then `path:line: new blank line at EOF.`, for git's default whitespace rules, and exits `2` if it printed anything; `-w` does not affect it (newer git versions differ here too). `--check` and `--shortstat` combine with no other format (`--stat`, `--name-only`, `--name-status`): a usage error (`129`), where git prints both stats or refuses with `128`. |
| `git diff --word-diff`, `--color`, `--no-index`, `-M`, `-C` | none | unsupported | Usage error (`129`). |
| `git show [<commit>...] [-- <path>...]` | `pyn show [<commit>...] [-- <path>...]` | different | Commit header (`commit`, `Author:`, `Date:`), message, then the commit's diff. The newest commit if none is named. The author line carries the email. On a git home the output is git's, except for merge commits (see below): it takes several commits, and paths (after `--`, or existing ones) limit each diff. A commit that changes none of the paths still prints its header, as git 2.55 does (older git leaves it out). A pyn server takes one commit and no paths (usage error, `129`). |
| `git show -U<n>`, `--stat`, `--shortstat`, `-w`, `--check`, `--name-only`, `--name-status` | same | same | The diff flags behave as on `diff`, and `--stat` is laid out as git lays it out at 80 columns (long names cut with `...`, graph scaled); `--check` exits `2` when it reports a problem. |
| `git show --numstat` | same | different | The numeric rows and format-combination limits are the same as `diff --numstat`. |
| `git show <rev>:<path>` | `pyn get <path> [--rev N] [-o <file>]` | different | `show` takes commits only (`HEAD:foo` fails with `fatal: ... is not a commit`, `128`). File content at a revision is `pyn get`, to stdout or a file. `--rev` is a per-path revision, not a commit. |
| `git show --oneline`, `-s` (`--no-patch`), `--format=`, `--pretty=`, `--date=` | same | supported | Both homes, with the formats of `log`. `-s` prints the commit only; `--oneline` and the formats print their line, then the diff (blank line between for a template or a named format, none for `--oneline`). Without them a pyn server prints its own header (see above). |
| `git show <tag>` | `pyn show <tag>` | different | On a pyn server with the local store, an annotated tag prints `tag <name>`, `Tagger:`, `Date:` and its message, then the commit; a lightweight tag, or a tag with `~N`/`^` steps, shows the commit. A git home shows the commit only. |
| `git show <object>` | none | unsupported | `fatal: ... is not a commit`. |
| `git restore --source=<rev> <path>` | `pyn restore --source <rev> <path>` (or `pyn restore <path> <rev>`) | different | pyn's restore appends a new revision on the server: it needs the lock, the `restore` permission and a typed confirmation (`--yes` skips it); `-m` sets the message. The revision is required (git defaults to the index or HEAD) and `-s` is the short form of `--source`. Git's restore only edits the working tree. |
| `git restore --staged <path>...` | `pyn restore --staged <path>...` (`-S`) | same | Puts the staged entries under the paths back to the last commit and leaves the files alone. On a git home `--source <commit>` takes the entries from that commit, as git; on a pyn server there is no commit to take them from and it is a usage error. |
| `git restore [-s <rev>] [-S] [-W] <path>...` (the work tree) | `pyn restore ...` on a git home | git home only | Takes the paths from the index (or `--source`) into the work tree, and with `--staged --worktree` into both. See [Branches, switching and the work tree](#branches-switching-and-the-work-tree). On a pyn server `--worktree` is a usage error (`129`): a restore there makes an older revision the new head. |
| `git restore --ours\|--theirs [-W] <path>...` | `pyn restore --ours\|--theirs <path>...` | git home only | Writes one side of an unmerged path into the work tree and leaves the path unmerged. On a pyn server it is a usage error (`129`). See [`restore`](#branches-switching-and-the-work-tree). |
| `git restore --merge <path>...` | `pyn restore --merge <path>...` | git home only | Recreates the conflicted merge of an unmerged path in the work tree, with diff3 markers. `--merge` has no `-m` short form (that is `--message`), and `--conflict=<style>` is not accepted. |
| `git restore -p` | `pyn restore -p` (`--patch`) | git home only | Asks about each hunk of the work tree, the index (`--staged`) or both, from the index, `HEAD` or `--source`, see [Choosing hunks](#choosing-hunks). On a pyn server it is a usage error (`129`). |
| `git reset [--soft\|--mixed\|--hard\|--keep\|--merge] [-q] [<rev>]`, `git reset [<rev>] [--] <path>...` | `pyn reset ...` | git home: supported; pyn server: different | On a git home it is git's reset, see [Reset](#reset). On a pyn server `pyn reset [--soft\|--mixed\|--hard] [<commit>]` only drops unpublished local commits ([workspace](workspace.md#the-local-store)); `--keep`, `--merge`, `-q`, paths and a second revision are a usage error there. |
| `git branch`, `git switch`, `git checkout` | `pyn branch`, `pyn switch`, `pyn checkout` | supported | See [branching and merging](#branching-and-merging). On a git home they work on `.git` as git does, see [Branches, switching and the work tree](#branches-switching-and-the-work-tree). |
| `git merge` | `pyn merge` | supported | See [branching and merging](#branching-and-merging). On a git home it merges as git does, see [Merging](#merging). |
| `git fetch` | `pyn fetch` on a git home | git home only | See [Fetch on a git home](#fetch-on-a-git-home). On a pyn server it fetches into the local store (a workspace cloned with `--no-local-store` runs `pyn update`), see [Remotes on a pyn server](#remotes-on-a-pyn-server). |
| `git pull` | `pyn pull` on a git home | git home only | Fetch, then merge through `pyn merge`; see [Pull on a git home](#pull-on-a-git-home). On a pyn server it fetches into the local store and merges (a workspace cloned with `--no-local-store` runs `pyn update`), see [Remotes on a pyn server](#remotes-on-a-pyn-server). |
| `git push` | `pyn push` on a git home | git home only | See [Push on a git home](#push-on-a-git-home). On a pyn server it publishes the local commits (a workspace cloned with `--no-local-store` prints that there is nothing to push, because a commit goes straight to the server); see [Remotes on a pyn server](#remotes-on-a-pyn-server). |
| `git remote` | `pyn remote` | git home; pyn server partly | On a git home it lists and edits the remotes in `.git/config`, see [Remotes](#remotes). On a pyn server `pyn remote`, `pyn remote -v` and `pyn remote get-url origin` show the one remote, `origin`: the server and repository set by `--server` and `--repo` or `pyn config set server` and `repo`; `add`, `remove`, `rename` and `set-url` say that a server home has exactly one home and exit `1`. |
| `git tag [-a] [-s] [-u <keyid>] [--no-sign] [-m <msg>] [-f] [<name> [<commit>]]`, `-d`, `-l` | `pyn tag ...` | supported | See [Tags](#tags). On a pyn server the tags are local-only refs in the local store (workspaces with `local_store` on); they are never pushed, and `-s`, `-u` are refused (#447). |
| `git stash [push] [-p] [-u] [-k] [-m <msg>] [-- <paths>]`, `list`, `pop`, `apply` (`--index`), `drop`, `clear`, `show`, `branch`, `create`, `store` | `pyn stash ...` on a git home | git home only | See [Stashing](#stashing). On a pyn server it prints the git-command message. |
| `git clean [-n] [-f] [-d] [-x\|-X] [-q] [-i] [-e <pattern>] [--] [<path>...]` | `pyn clean ...` on a git home | git home only | See [Clean](#clean). Without `-f`, `-n` or `-i` it refuses, as `clean.requireForce` does; `-i` is git's menu; pathspecs take globs (`*`, `?`, `[...]`) but no `:` magic. |

`pyn update [<path>...]` is the pre-branch stand-in for `fetch` plus `pull`: it fast-forwards files without local
changes and never overwrites an edit, reporting skipped files. `--revision <commit>` moves the workspace to an older or
newer commit. It is pyn only; on a pyn server `pyn fetch` and `pyn pull` run it on a pyn server. A git home has `pyn pull` instead, and `pyn update` is refused there.

## History and inspection

Commands people reach for that are not in the daily set.

| Git | pyn | Status | Notes |
| --- | --- | --- | --- |
| `git ls-files` | `pyn files` | different | Table of every file with mode, head revision and lock holder. No flags. On a git home `pyn files` lists the index, every file `shared`, with no revision or lock, and `pyn ls-files` is git's command; see [Inspection commands](#inspection-commands). |
| `git ls-tree [<rev>] [<path>]` | `pyn ls [<path>]` | different | One folder with each entry's mode, last change and lock. Head only; no revision argument. On a git home `pyn ls` lists `HEAD`'s tree, with no last change, and `pyn ls-tree` is git's command; see [Inspection commands](#inspection-commands). |
| `git cat-file`, `git show <rev>:<path>` | `pyn get <path>` | different | See `show` above. On a git home `pyn cat-file` is git's command; see [Inspection commands](#inspection-commands). |
| `git rev-parse`, `git show-ref`, `git shortlog`, `git describe`, `git name-rev` | the same names on a git home | git home only | See [Inspection commands](#inspection-commands). On a pyn server they fail with `fatal: not a git repository (or any of the parent directories): .git` (`128`). |
| `git config --list`, `--get <key>`, `<key> <value>` | `pyn config list`, `get`, `set` | different | Subcommands instead of flags. `--global` and `--local` are accepted where they apply (`set` takes `--global` only; the default is the workspace). Keys are pyn's own. `pyn config init` is pyn only: it prompts for the user settings. |
| `git config --unset`, `-e`, `--system`, `--file` | none | unsupported | Usage error (`129`). |
| `git fsck` | `pyn verify [<path>]` | different | Checks every revision's hash and its link to the one before, and the audit log's chain. Silent and exit 0 when intact, as git; otherwise a `PROBLEM`, `WHAT`, `PATH` table and exit 1. `-v` / `--verbose` also prints what was checked. See [integrity](integrity.md). |
| `git reflog [show\|list\|exists\|expire\|delete]` | `pyn reflog ...` on a git home | git home only | See [Reflogs](#reflogs). On a pyn server it prints the git-command message; the closest analogue there is `pyn audit`, an append-only log of lock, revision and access events, filtered with `--path`, `--actor`, `--action`, `--before`, `--limit` and permission-gated (`view_audit`). |
| `git grep [<options>] [-e] <pattern> [<rev>...] [[--] <path>...]` | `pyn grep ...` on a git home | git home only | See [Grep](#grep). Searches the tracked files of the work tree, the index (`--cached`) or the commits named; `-n -i -w -l -c -v -q -h -H -a -I -E -G -F`, `-e` (repeatable), `--and`, `--or`, `--not` and parentheses, pathspecs after the revisions or `--`. On a pyn server or local-store workspace it says it needs a git repository (`1`). |
| `git blame`, `git annotate` | none | unsupported | Git-command message. Line history needs merge-capable text history; not scheduled. |
| `git submodule` | none | unsupported | Submodule lifecycle commands are not built. On a git home, `status` and `diff` inspect recorded gitlinks and initialized submodule checkouts without changing them; see [On a git home](#on-a-git-home). |
| `git format-patch`, `git apply`, `git am` | the same names on a git home | git home only | See [Patches](#patches). On a pyn server `format-patch` and `am` fail with `fatal: not a git repository (or any of the parent directories): .git` (`128`), as does `apply`, which git would run outside a repository too. |
| `git archive`, `git bundle`, `git notes`, `git bisect`, `git worktree`, `git gc`, `git fsck` | none | unsupported | `pyn: '<cmd>' is not a pyn command` (`1`). Only the commands in the git-only list print the "git command pyn does not support yet" message, so these read as unknown until that list is widened (#223). |

## Branching and merging

Branches and merge exist on a pyn server ([branches](branches.md), [merging](merging.md)); remotes on a pyn server are the one server and repository (see [Remotes](#remotes)). The model is
decision #233 (commit graph, per-commit trees, branch heads); policy per branch is [branches](branches.md#policy-and-modes); exclusive paths form a tree of
revisions with a pointer per branch (decision #299).

| Git | pyn | Status | Notes |
| --- | --- | --- | --- |
| `git branch [<name> [<start>]]`, `-d`, `-D`, `-m`, `-l`, `-v`, `--show-current` | `pyn branch ...` | supported | `-d` refuses a branch that is not on the default branch (git: not merged into `HEAD`). `-a` and `-r` list the server as the remote `origin` (see [Remotes on a pyn server](#remotes-on-a-pyn-server)); `-M`, `-c`, `-C`, `-f`, `-u`, `--unset-upstream` and patterns are not built on a pyn server (usage errors); a git home has them (see below). The workspace's branch cannot be deleted. |
| `git switch <branch>`, `-c <new> [<start>]`, `-f` | `pyn switch ...` | supported | Local changes carry over where the target branch has the same file, and stop the switch (nothing changed, the paths listed) where it does not; `-f` discards them. On a pyn server `-`, `--detach` and `-C` are not built; a git home has `-`, `--detach`, `-C` and `--orphan` (see below). Prints `Switched to branch '<name>'` on standard error, as git. |
| `git checkout <branch>`, `-b <new> [<start>]`, `-f`, `[<tree-ish>] -- <path>...` | `pyn checkout ...` | supported | Reserved for git's meaning. `-- <path>` restores from the revision the workspace has (git: the index; a staged path must be unstaged first), `<branch\|commit> -- <path>` from that branch or commit. On a pyn server a path that is `exclusive` and has a local edit (locked by you or not) is not discarded silently: the files are named and the discard needs the path typed back or `--yes`, and a run without a terminal refuses (`128`, nothing changed); `shared` paths discard silently as in git. On a pyn server there is no detached HEAD: `checkout <commit>` is refused with what to use instead. Taking a lock is `pyn lock`. |
| `git merge <branch\|commit>`, `-m`, `--no-ff`, `--ff-only`, `--no-commit`, `--abort`, `--continue`, `--stat`, `-n`/`--no-stat` | `pyn merge ...` | supported | Shared files merge three-way with diff3 conflict markers; a binary conflicts as a whole file. Exclusive files take the pointer that descends from the other, and conflict where both branches changed one or a path became exclusive while a branch edited it (a person chooses a side, taking the lock). A conflicted merge leaves the workspace as git does and is concluded with `pyn add` and `pyn commit` (no `-m` needed), or given up with `--abort`. Differences: `--preview` (pyn's own) reports what would merge and conflict and changes nothing; a merge needs a clean index and no local edits to the files it touches; `--squash`, `--no-verify`, `--edit`, `--signoff` and `--ff` are usage errors, and so are `-s`, `-X`, `-S` and `--no-gpg-sign` (they need a git repository; see [Merging](#merging) and [Signing](#signing)). `git checkout --ours` and `--theirs` are `pyn checkout --ours\|--theirs -- <path>`. `pyn status` marks unmerged paths in its `STATE` column. Recursive merge of criss-cross histories as git does, with the base of a conflict shown as `merged common ancestors`. |
| `git cherry-pick <commit>...`, ranges, `-n`, `-x`, `-s`, `-S[<keyid>]`, `--no-gpg-sign`, `-m <n>`, `--continue`, `--skip`, `--abort`, `--quit` | `pyn cherry-pick ...` | supported | Each commit's change becomes a new commit, with its author and message. On a git home see [Cherry-pick](#cherry-pick). On a pyn server workspace with local commits the commits, ranges and `-x` work, and `-n`, `-s`, `-S`, `--no-gpg-sign`, `-m` and `--quit` are refused; see [cherry-pick in commits](commits.md#cherry-pick). A pyn server workspace without local commits refuses the command. |
| `git revert <commit>...`, ranges, `-n`, `-s`, `-S[<keyid>]`, `--no-gpg-sign`, `-m <n>`, `--no-edit`, `--continue`, `--skip`, `--abort`, `--quit` | `pyn revert ...` | supported | Each commit's change is undone by a new commit (`Revert "<subject>"`). On a git home see [Revert](#revert). On a pyn server workspace with local commits the commits and ranges work, and `-n`, `-s`, `-S`, `--no-gpg-sign`, `-m` and `--quit` are refused; see [revert in commits](commits.md#revert). A pyn server workspace without local commits refuses the command. |
| `git rebase [--onto <newbase>] [<upstream> [<branch>]]`, `--continue`, `--skip`, `--abort`, `--quit`, `--autostash`, `--no-ff`, `-S[<keyid>]`, `--no-gpg-sign` | `pyn rebase ...` | supported (git home) | Replays the commits after the upstream one by one onto the new base, with the same ids as git. See [Rebase](#rebase), including `--rebase-merges`, `--root`, `--update-refs` and the fork point. A pyn server workspace with a local store rebases only its unpublished local commits, interactively too (`edit`, `exec`, `break`, `--exec`, `--edit-todo`, `--autostash`, `--no-ff`, `--quit` and `<branch>` work), and refuses a range with a published one, `--rebase-merges`, `--root`, `--fork-point` and `--update-refs` ([commits](commits.md#rebase)); without a local store it refuses the command. |

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
only what the server sends;
`fetch.output`, `fetch.showForcedUpdates`, `fetch.recurseSubmodules`, `fetch.writeCommitGraph`, `remote.<name>.promisor`,
`--refmap`, `--negotiation-tip`, `--server-option`, `--stdin` and `-4`/`-6` are not read or taken; `--all` runs
the remotes one at a time whatever `-j` or `fetch.parallel` say, so `FETCH_HEAD` lists them in configuration order; the stale-ref hint after a ref could
not be written names `pyn remote prune`.

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
| `--no-edit` | Accepted: no editor is ever started. |
| `--no-rebase`, `--rebase`, `-r`, `--autostash` | `--no-rebase` merges; `--rebase` fast-forwards or replays as above; `--autostash` stashes before a rebase and applies after (a usage error unless the pull rebases). |
| `-s`, `--strategy`, `-X`, `--strategy-option` | Passed to the merge as for `pyn merge`, see [Strategies and options](#strategies-and-options). A strategy name git does not define is `Could not find merge strategy '<name>'.` (`1`); more than one `-s`, and `-X` options pyn cannot honor, are usage errors (`129`), as are `-s` and `-X` with a pull that rebases (git hands them to its rebase; pyn's rebase has no strategies yet). |
| `-S[<keyid>]`, `--gpg-sign[=<keyid>]`, `--no-gpg-sign` | Passed on: the merge commit of a merge pull, or each commit a rebasing pull makes, is signed as for `pyn merge` and `pyn rebase`, see [Signing](#signing). |
| `--edit`, `-e`, `--squash`, `--signoff`, `--cleanup`, `--recurse-submodules`, `--allow-unrelated-histories`, `--all` | Usage error (`129`): not built (`--all`: use `pyn fetch --all`). |

Config read: `pull.ff`, `pull.rebase`, `branch.<name>.rebase`, `branch.<name>.merge`, `branch.<name>.remote`, `merge.ff`, plus what
`pyn fetch` reads, plus `merge.stat`. `pull.twohead`, `pull.octopus` and `branch.<name>.mergeOptions` are not read. The message of an annotated tag
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
or `localhost`; the path is percent-decoded. `clone` records a relative path as an absolute one, as git does.

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
| `git blame` | Not scheduled. |
| Commands that maintain git's own storage or run its hooks (`gc`, `fsck`, `bundle`, `submodule`, `worktree`, hooks in `.git/hooks`) | pyn reads a git repository's `.git/` on a git home (decision #245) but does not maintain it or run its hooks. pyn's own hooks are tracked in #216. |

## On a git home

Decision #245: a repository has one home, a pyn server or git. In a git repository (a `.git` is found) pyn behaves as git,
through its own implementation of git's object store, refs and index, with no git library or binary (decision #30). A
`.pyn/pyn.toml` there is ignored: no path is exclusive, and the commands that need a pyn server refuse and name it. Going
from a git home to a pyn server is one explicit step, `pyn convert --home pyn` (issues #572 and #95), which does not exist yet. The other direction exists: see [Leaving a pyn server](#leaving-a-pyn-server).

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
  signatures stay valid; a SHA-1 repository needs the rehash tracked in #572. Local commits the server lacks, and the local tags,
  come along from the local store; a branch that has diverged from the server's is refused (`pull` or `rebase` first).
- **Lost:** locks (a lock held on the server stays there and no longer applies), revision numbers, the shared and exclusive
  modes (`.pyn/pyn.toml` stays as local state that a git home ignores), changes staged with `pyn add`, and the record of which
  revision each file is at. Files that were read-only because of the exclusive mode become writable.
- **How:** the commits and trees come through the fetch route (all branches), each file by its id, verified by hash. The new
  `.git` is built in `.git.pyn-convert` and renamed into place only when complete, so an error leaves the workspace as it was.
  The index is written from `HEAD`, so unmodified files are clean and edited ones show as modified. The command then records
  `repo_tool = "git"`, hides `.pyn/` in `.git/info/exclude`, and removes `.pyn/local_only/state/` and `store/` (the marker alone
  decides the home; the settings in `config.toml` stay). It refuses when a `.git` exists or a merge is in progress.

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
| `grep [<options>] [-e] <pattern> [<rev>...] [[--] <path>...]` | git's output over the work tree, the index (`--cached`) or revisions; see [Grep](#grep). |
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
| `reflog` | See [Reflogs](#reflogs). |
| `rev-parse`, `ls-files`, `ls-tree`, `cat-file`, `show-ref`, `shortlog`, `describe`, `name-rev` | See [Inspection commands](#inspection-commands). |
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
select what is ignored; staged gitlinks remain visible in status even under `all`. Submodule initialization and updates are not built.
With no worktree `.gitmodules`, an unmerged index entry for that file disables the recorded configuration fallback.

Differences from git, all deliberate or not yet done:

- Files are read as they are on disk: `core.autocrlf`, `.gitattributes` filters and `core.eol` are not applied, so a
  repository that depends on them can show files as modified that git does not.
- No rename or copy detection: a rename is a delete and an add.
- `log` and `show` print no ref decorations (`%d` in a `log` template does); commit abbreviations are 7 digits and are not
  lengthened to stay unique.
- A nested repository that the index does not track is one untracked `dir/`.
- `merge` and `stash apply` detect no renames, write diff3 conflict markers whatever `merge.conflictStyle` says and merge only when
  there is one merge base (`merge -s ours` needs none); `stash apply` does not write `AUTO_MERGE`; `switch` and `checkout` print their own wording where git's differs (`Fast-forward` without a diffstat, the status
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
`relative`, `unix` and `raw` and applies to `Date:` and `%ad`/`%cd`; commit dates print in the offset recorded in the commit. `show` takes the same formats and `--date=`.

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
- Passing a revision that is not a commit, a dated reflog selector (`HEAD@{yesterday}`) or `:/<text>` fails with exit `128`
  where git accepts them.

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
print nothing. Lock state is shown in the default table's lock column and by `pyn lock`/`pyn locks`.
`-b` prints `## <branch>...origin/<branch>` with `[ahead N]` for local commits not yet published, and just
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
  A pyn server workspace with the local store parses both the same way; only the author changes.
- `-s` and `--signoff` append `Signed-off-by: <committer name> <email>` after git's trailer rules: no blank line when the last
  paragraph is a trailer block, and nothing when the last line already is that line.
- `--amend` replaces the tip with a commit that has the tip's parents, author (name, email and date) and message, and the
  index as its tree; `-m` or `-F` replace the message, `--author` and `--date` replace the author fields, and `--no-edit` is
  accepted (pyn never opens an editor). `GIT_AUTHOR_*` is ignored, as in git. The committer and its date are new, a `gpgsig`
  header is dropped and other headers are kept. It exits `128` with no commit to amend or during a merge; an amend that
  would leave the commit equal to its parent's tree exits `1` unless `--allow-empty` is given; amending a merge commit is allowed.
  The branch and `HEAD` logs get `commit (amend): <subject>`, and `ORIG_HEAD` is not written.
- `-S`, `--gpg-sign[=<keyid>]` (the key id attached or after `=`) and `--no-gpg-sign` (the last of the two wins) sign the commit;
  `commit.gpgsign=true` signs by default. See [Signing](#signing).
- `--allow-empty-message` accepts a message that is empty after cleanup, and stores it as zero bytes. `--cleanup=<mode>` is
  `strip` (also drops `#` lines), `whitespace` (the default, and `default` and `scissors`, which differ from it only when an editor
  is used) or `verbatim` (the message exactly as given, `-m` paragraphs joined by a blank line); it applies to `-m`, `-F` and `-C`,
  and an unknown mode is an error (`128`). `core.commentChar` is not read. A comment-only message is not empty under `whitespace`.
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

Not built, on either home: `-c`/`--reedit-message`, `-e`/`--edit`, `-t`/`--template`, `--fixup`, `--squash`, `--dry-run`, `--short`,
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
refused run leaves both untouched. Paths are literal files and folders relative to where you stand (no globs).

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
with the same reflog lines.

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
-r, options to 'git branch' do not take a branch name.` (`128`). `-r -d`, `-vv` (the upstream column) and `--contains` are not built.

`-u <upstream> [<branch>]` (`--set-upstream-to`) writes `branch.<name>.remote` and `merge` and prints `branch '<name>' set up to track '<upstream>'.`
on standard output. A local branch as upstream is remote `.` with `merge = refs/heads/<name>`; a remote-tracking branch needs a
`remote.<name>.fetch` refspec that maps it, else `cannot set up tracking information; starting point '<ref>' is not a branch`. Naming the
branch itself only warns (`not setting branch '<name>' as its own upstream`), an unknown upstream is `fatal: the requested upstream branch
'<name>' does not exist` with git's hint (off with `advice.setUpstreamFailure`), and an unknown branch `branch '<name>' does not exist`
(all `128`). `--unset-upstream [<branch>]` removes both settings, or is `fatal: branch '<name>' has no upstream information` (`128`).
Covered against the real git (2.43 and 2.55) by `crates/pyn-cli/tests/git_branch_flags.rs`. 
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
- `switch -C <branch> [<start>]` (`--force-create`) creates the branch or resets an existing one to the start point (default `HEAD`), logs
  `branch: Reset to <start>` (on `HEAD` alone when the branch is already there and current), and prints `Switched to and reset branch`,
  `Reset branch` (already on it) or `Switched to a new branch`. `switch --orphan <new>` points `HEAD` at an unborn branch and empties the index and the
  tracked files of the work tree (untracked files stay; local changes stop it unless `-f`), logs nothing in the `HEAD` reflog and prints
  `Switched to a new branch '<new>'`; it refuses a start point, an existing name, and being combined with `-c`, `-C` or `--detach`.
  `checkout --orphan` and `-B` are not built. On a pyn server `-C` and `--orphan` are usage errors.
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
`--source` is given; `--staged` alone is described under [staging](#staging-and-committing). The paths are prefixes (no globs).

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
(`HEAD@{1}`, `@{-1}`; see [Reflogs](#reflogs)). Unlike git, a sparse checkout refuses every mode but `--soft`. A running rebase or cherry-pick's own
state folder (`rebase-merge/`, `sequencer/`) is not removed. Covered against the real git by `crates/pyn-cli/tests/git_reset.rs`.

### Merging

`merge <branch|tag|commit>` follows git's `ort` results for what pyn handles, with these steps:

1. It refuses unmerged entries (`error: Merging is not possible because you have unmerged files.`, git's hints and `fatal: Exiting because of an unresolved conflict.`, `128`), a merge or cherry-pick not yet concluded (`fatal: You have not concluded your merge (MERGE_HEAD exists).`, `128`), a rebase or revert already in progress, and an unknown name (`1`). `ORIG_HEAD`
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
`commit`. `merge --preview` needs a pyn server. `--squash` is not built.

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
| `no-renames`, `diff-algorithm=myers\|default\|minimal`, `no-renormalize` | Accepted; pyn detects no renames and has one diff. |
| `renormalize` | Accepted when no path being merged has a `text`, `eol`, `crlf`, `filter`, `ident` or `working-tree-encoding` attribute and `core.autocrlf` is unset, since then it changes nothing; otherwise the merge is refused with nothing changed (`128`) because pyn converts no line endings (#273). |
| `patience`, `histogram`, `diff-algorithm=patience\|histogram` | Usage error (`129`): only the Myers diff is built. |
| `find-renames[=<n>]`, `rename-threshold=<n>` | Usage error (`129`): rename detection is not built. |

Not built: several `-s` in one command, `-X` for `resolve`, `merge.<name>.mergeoptions` and `branch.<name>.mergeOptions`.
Because renames are never followed, the results differ from git's where one side renamed a file the other changed, with any
strategy or option.

### Cherry-pick

`cherry-pick <commit>...` merges each commit's change onto the index and makes a commit with that commit's author and message,
the committer from the identity rules of `commit`, and a tree and id that match git's (the tests compare ids). The result is
the same as `merge` of the commit onto `HEAD` with the commit's parent as the merge base (`(empty tree)` for a root commit), so
the merge rules above apply: no renames, `ort`-style results for everything else, diff3 markers labelled `HEAD` and
`<abbreviation> (<subject>)` with the base `parent of <abbreviation> (<subject>)`. A commit is anything `resolve` takes, `A..B`
and `A...B` name ranges (applied oldest first, parents before children), and several arguments are applied in the order given.

1. Before anything changes it refuses, with git's wording and exit `128` (`fatal: cherry-pick failed` after `error:` lines): a
   sequence while a cherry-pick or revert is already in progress, unmerged entries, an unknown revision, an empty range (`empty commit set passed`),
   a merge commit without `-m <n>` (or `-m` past its parents), and a staged change (`your local changes would be overwritten by
   cherry-pick`; `-n` allows it). A merge in progress, a rebase, a sparse checkout and several commits onto an unborn branch are
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
ones before it); `-e`, `--edit`, `--ff`, `--cleanup`, `-X`, `--strategy`, `--rerere-autoupdate`, `--allow-empty` and
`--keep-redundant-commits` are usage errors; `--no-edit` is accepted. Covered against the real git by
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
   `--no-edit` is accepted. `-x`, `-e`, `--edit`, `--cleanup`, `-X`, `--strategy`, `--rerere-autoupdate`,
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
   merge, cherry-pick or revert already in progress, a sparse checkout, and an unborn branch. `--autostash` (or
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
   `sequence.editor`, then the message editor (`GIT_EDITOR`, `core.editor`, `VISUAL`, `EDITOR`, `vi`; `Terminal is dumb, but EDITOR
   unset` without one on a dumb terminal; an editor of `:` keeps the file). The sheet is git's: `pick`, `reword`, `edit`,
   `squash`, `fixup`, `drop`, `exec`, `break` and `noop` (or `p`, `r`, `e`, `s`, `f`, `d`, `x`, `b`), ids abbreviated to seven
   digits, comment lines and blank lines ignored, lines in any order. A bad line prints `error: invalid command '<word>'` and
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
     'git notes copy'`. A notes tree gets 0, 1 or 2 levels of two-digit directories by size. The `post-rewrite` hook is not run.
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

Differences from git: `-s`, `-X`, `--signoff`, `--keep-base`, `--reapply-cherry-picks`, `--apply` and `--empty` other than `drop`
are usage errors (`129`). A `merge` of several labels (an octopus merge) stops with `octopus merges are not built` and
leaves the command first in the sheet; git continues it (`git rebase --continue`) and a merge whose branches have more than one
merge base stops the same way. pyn writes `AUTO_MERGE` only for a merge that stopped on conflicts; git writes it for every pick. A workspace's own
rebase refuses `fixup -C`/`-c` and `--rebase-merges`, `--root`, `--fork-point` and `--update-refs` (and the sheet commands `label`,
`reset`, `merge` and `update-ref`), and leaves `amend!` commits where they are. There is no `post-rewrite` hook. `-m`, `-v`, `-n`, `--keep-empty` and the `--no-` forms of the refused flags are accepted and ignored. Commits whose timestamps tie are ordered by the walk of the graph, as git's, not by
date. After an interactive rebase git leaves `REBASE_HEAD` behind in some cases; pyn removes it. Covered against the real git
(2.43 and 2.55) by `crates/pyn-cli/tests/git_rebase.rs` and, for pull, `crates/pyn-cli/tests/git_pull.rs`.

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
  `user.signingkey`, `gpg.ssh.defaultKeyCommand` is not built and signing stops.
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

`log --show-signature` and `verify-commit` / `verify-tag` equivalents are not built: pyn does not verify signatures, and `log
--show-signature` stays a usage error.

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

`stash push -- <paths>` stashes only the named paths (literal names, not patterns, relative to where you stand): the stash's work tree
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
`-p --stat`, and takes `--stat`, `--shortstat`, `--numstat`, `--name-only`, `--name-status`, `-U<n>` and `-w`;
`-u`/`--include-untracked` and `--only-untracked` (or `stash.showIncludeUntracked`) add the untracked files the stash saved. Numeric output has the same
format-combination limits as `diff --numstat`. `stash branch <name> [<stash>]` creates the branch at the commit the stash was
made on, switches to it, applies
the stash with `--index` and drops it (a conflict keeps it). `stash create [<message>]` prints a stash commit without storing it
or changing the work tree (nothing is printed when there is nothing to stash), and `stash store [-m <message>] <commit>` puts
such a commit on the list (`Created via "git stash store".` without `-m`; storing the newest entry again does nothing).

git 2.43 cannot stash in a SHA-256 repository, so the SHA-256 stash is tested for consistency, not against git.

### Choosing hunks

`add -p`, `restore -p` and `stash push -p` share one prompt, with git's header, hunks, keys and texts. For each changed file it
prints the diff header, then each hunk followed by `(<n>/<total>) <Verb> this hunk [<keys>]? `, and reads a line from standard
input. `<Verb>` and the help text follow the command: `Stage` for `add`, `Stash` for `stash`, `Unstage` for `restore --staged`,
`Discard ... from worktree` / `from index and worktree` for `restore` from the index or `HEAD`, and `Apply ... to worktree`,
`to index` / `to index and worktree` when `--source` names anything but the literal word `HEAD` (the diff is then shown from the
current state towards the source, with the `a/` and `b/` names swapped, as git does). A mode change, a deletion and an addition
are asked as `mode change`, `deletion` and `addition`.

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
`.git/addp-hunk-edit.diff`, starts the editor (`GIT_EDITOR`, `core.editor`, `VISUAL`, `EDITOR`) and takes the hunk back with `#`
lines removed; an edit that leaves nothing keeps the hunk unchanged, one that does not fit the file is reported
(`error: patch failed: <path>:<line>`) and asks `Your edited hunk does not apply. Edit again (saying "no" discards!) [y/n]? `.
Messages about a key go to standard output, errors from applying to standard error. A blank line follows each file's questions;
`q` stops with the choices already made applied, and so does the end of input, which also skips the files not yet asked about.
With nothing to ask about it prints `No changes.` (or `Only binary files changed.`) on standard output and exits `0`.

What a choice does: `add -p` writes the chosen hunks (and mode changes) into the index only. `restore -p` takes them out of the
work tree, the index or both; when the chosen hunks fit the work tree but not the index it says so and asks
`Apply them to the worktree anyway? ` as git does. `stash push -p` shows `HEAD` against the work tree, ignoring the index and new
files, saves a stash whose work tree is `HEAD` with the chosen hunks, takes them out of the work tree and keeps the index
(`--no-keep-index` also puts the staged changes of the paths back to `HEAD`); `-u` is refused, and with no hunk chosen it prints
`No changes selected` and exits `1`. Each file is applied as soon as its questions are over, as git does. `add -p` cannot be
combined with `-n`, nor `restore -p` with `--ours`, `--theirs` or `--merge`. The prompt reads standard input, so a
script can answer it; binary files and a file replaced by a symlink (or the reverse) are not offered.

Differences from git: binary files are counted for the notice but never asked about; the text of regular-expression errors is
the `regex` crate's; the diff uses 3 lines of context and ignores `diff.context`, `--inter-hunk-context` and colour (`color.ui`,
`interactive.diffFilter` and `interactive.singleKey` are not read). git 2.43 differs from 2.55 in the prompt (no `p`, `P` or
`(was: ...)`, `j`/`k` offered only when they apply, help wording, hunk printed again after every answer, first letter of a longer
answer taken, end of input moving on to the next file, messages on standard error); pyn follows 2.55, and the tests that compare
with the real git check only the index, work tree, stash and exit status, on inputs both versions treat alike. Covered by
`git_patch.rs`.

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
- Paths limit the clean to themselves and what is under them; standing in a folder limits it to that folder. A name with
  `*`, `?`, `[` or a backslash is a glob, matched against the whole path with `*` crossing folders, as in a plain git
  pathspec (a folder also matches with a trailing slash, so `dir/*` takes `dir/` itself); a name that exists literally still
  matches itself. A `:` pathspec is refused. A path outside the repository is `128`.
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

Differences from git: `:` pathspec magic is refused; the order of the
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
requires, and `--` ends the revisions. Pathspecs are relative to where you stand, take `*`, `?` and `[...]` globs (a `*`
crosses folders) but no `:` magic, and default to the current folder. The settings `grep.lineNumber`, `grep.patternType`
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
| Binary attributes | `.gitattributes` (`binary`, `-diff`, `diff`) and `textconv` are not consulted; a file is binary by its content only. |
| Revisions | Commits and tags that peel to one (`HEAD~2`, a branch, a hash); a tree such as `HEAD:sub` is not a revision here, so it is read as a path and fails with git's `ambiguous argument`. |
| Not built (usage error `129`) | `--untracked`, `--no-index` (so also searching outside a repository), `--recurse-submodules`, `--exclude-standard`, `-o`, `-L`, `-z`, `-f`, `-m`, `-P`, `-A`, `-B`, `-C`, `-p`, `-W`, `--column`, `--heading`, `--break`, `--color`, `--all-match`, `--max-depth`, abbreviated long options, and `:` pathspec magic. |

Covered against the real git, on SHA-1 and SHA-256 repositories, by `crates/pyn-cli/tests/git_grep.rs` (output, order and
exit status for the work tree, the index, revisions, binary files, subfolders, conflicts and a few hundred patterns of
both syntaxes) and `crates/pyn-cli/tests/git_exit_status.rs`, which compare only behaviour the git releases in use agree on.

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

### Inspection commands

`rev-parse`, `ls-files`, `ls-tree`, `cat-file`, `show-ref`, `shortlog`, `describe` and `name-rev` print what git prints, byte for
byte, with git's exit statuses; `crates/pyn-cli/tests/git_plumbing.rs` compares each with the real git on the same repository
(SHA-1 and SHA-256, loose and packed objects, from the root and from folders). They read the repository and never write it.
Options that are not listed here are a usage error (`129`), where git would accept them. Outside a git repository they fail
with `fatal: not a git repository (or any of the parent directories): .git` (`128`).

| Command | Supported |
| --- | --- |
| `rev-parse` | `<rev>` in any form `log` takes, plus `^{}`, `^{commit}`, `^{tree}`, `^{blob}`, `^{tag}`, `^{object}`, `<tree-ish>:<path>` (also `./` and `../` paths), `:<path>` and `:<n>:<path>` for the index, `^<rev>`, `<a>..<b>`, `<a>...<b>`, `<rev>^@`, `<rev>^!`; `--verify`, `-q`, `--short[=<n>]`, `--abbrev-ref[=strict\|loose]`, `--symbolic`, `--symbolic-full-name`, `--git-dir`, `--absolute-git-dir`, `--git-common-dir`, `--show-toplevel`, `--show-prefix`, `--show-cdup`, `--is-inside-work-tree`, `--is-inside-git-dir`, `--is-bare-repository`, `--show-object-format`, `--all`, `--branches`, `--tags`, `--remotes`. Options act on the revisions after them, as in git; a name that is no revision is printed back and must be a file. |
| `ls-files` | `-c`, `-d`, `-m`, `-o`, `-s`, `-u`, `-i` with `--exclude-standard`, `-z`, `--full-name`, `--error-unmatch`, and pathspecs (`*` and `?` are patterns; `:(...)` magic is a usage error). Paths are relative to the current folder, and only that folder is listed unless a pathspec says otherwise. `-m` lists deleted files too, so `-m -d` lists them twice, as git does. |
| `ls-tree` | `-d`, `-r`, `-t`, `-l`, `-z`, `--name-only`, `--name-status`, `--object-only`, `--full-name`, `--full-tree`, `--abbrev[=<n>]`, and paths. Paths are literal; `.` and `..` name a folder's contents. |
| `cat-file` | `-t`, `-s`, `-p`, `-e`, `<type> <object>` (tags are followed to the type), `--batch` and `--batch-check` with git's default format. |
| `show-ref` | `--head` (shown whatever the other options say, as git documents), `--heads`, `--tags`, `-d`, `-s`/`--hash[=<n>]`, `--abbrev[=<n>]`, `--verify`, `--exists`, `-q`, and patterns that match the end of a ref name on a `/`. |
| `shortlog` | `-n`, `-s`, `-e`, `-c`, a revision range and `-- <path>...`; with no revision it reads a `git log` on standard input unless that is a terminal, then it uses `HEAD`. Refused (`128`) when the repository has a `.mailmap` file or sets `mailmap.file` or `mailmap.blob`, which git would apply. |
| `describe` | `--tags`, `--all`, `--always`, `--long`, `--abbrev=<n>`, `--dirty[=<mark>]`, `--match`, `--exclude`, `--exact-match`, `--candidates=<n>`, and commit-ishes. The nearest tag is chosen by the walk git uses, including its limit of candidate tags. Describing a blob and `--first-parent`, `--contains` and `--broken` are not built. |
| `name-rev` | `--tags`, `--name-only`, `--no-undefined`, `--always` and commit-ishes. `--all`, `--stdin`, `--annotate-stdin`, `--refs` and `--exclude` are not built, and `name-rev` with no commit is a usage error where git prints nothing. |

Abbreviations (`--short`, `--abbrev`, `describe`) start at `core.abbrev` or 7 digits and grow until the id is unique among all
objects. git also grows the default length with the size of the repository (8 digits from about 16,000 objects); pyn does not,
so past that size a default abbreviation can be one digit shorter than git's.

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
anything is written, so a failure changes nothing (`1`; a malformed patch is `128`). It handles creations, deletions, renames,
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
`merge.conflictStyle` says, and no `Removing <path>` lines are printed (older git, which merges with merge-recursive, prints them). Maildirs, StGit and Mercurial formats, `-k`, `-b`, `-c`/scissors, `--ignore-date`, `--committer-date-is-author-date`,
`-i`, `--rerere-autoupdate`, `--allow-empty` and `-C`/`-p` are not built (a patch with no changes stops the session with
`Patch is empty.` and the hint to record an empty commit is left out). Hooks (`applypatch-msg`, `pre-applypatch`,
`post-applypatch`) are not run, with the usual warning when one exists.

### What pyn will not write

Any command that would write a work tree file stops first, and changes nothing, when git would convert what it writes: `core.autocrlf`
set to true, `core.eol=crlf` with a `text` attribute, `eol=crlf`, a `filter` other than Git LFS, `ident` or `working-tree-encoding`
(`.gitattributes` in the work tree, `.git/info/attributes` and the global attributes file are read; `-text`, `binary`, `eol=lf`,
symlinks and submodules are never converted). A Git LFS filter writes the pointer file as it is, with a warning (decision #245; see [Git LFS](#git-lfs)).
The same limit applies to `status`, which compares raw bytes (#273). Hooks are never run; one warning names the hooks that exist
for the command (`post-checkout`, `pre-merge-commit`, `commit-msg` and the like). Sparse checkouts, submodule updates, the split
index and `GIT_DIR` are not supported, and `--squash` and anything that talks to a remote other than `ls-remote`, `fetch`, `pull`, `push` and `clone` are not built yet (epic #301; see [Transports and credentials](#transports-and-credentials)).

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
fatal: `pyn lock` needs a pyn server, and this repository is a git repository; `pyn convert --home pyn` will move it to one (not built yet, #572)
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

`pyn convert` adopts the repository as a git home today; moving it to a pyn server is `pyn convert --home pyn` (#572, #95), which does not exist yet.

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
