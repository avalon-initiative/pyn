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
| `git clone <repo> [<dir>]` | `pyn clone <source> [<dir>]` | same | Source is `<server>/owner/name`, or `owner/name` on the configured server. Output is one summary line, not progress. |
| `git clone --depth`, `-b`, `--branch`, `--bare`, `--mirror`, `-o`, `-n`, `--recurse-submodules` | none | unsupported | Usage error (`129`). `-b` and `--branch` are possible now that [branches](branches.md) exist, and are not built yet. Every clone has full history on the server; there is no local object store to shallow. |
| `pyn clone --revision <commit>` | | pyn only | Clone as of a commit (a hash, `HEAD~N`). Git does this with `clone` then `checkout`. |
| `git status` | `pyn status` | different | Header `On branch <name>`, then a table with mode, revision, staged, state, lock and path plus a summary line, because mode and lock state have no place in git's layout. `STAGED` is `added` or `modified` for a staged path, and `STATE` then describes what changed after it was staged. On a git home the table is `STAGED`, `UNSTAGED`, `PATH` with git's states. |
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
| `git log [-n <n>] [<path>]` | `pyn log [-n <n>] [<path>]` (alias `history`) | different | Without a path, newest first, 50 by default. With a path, oldest first and all revisions (git prints newest first). It lists the workspace's branch (`On branch <name>` goes to standard error). Output is a table of per-path revisions, not commit blocks. `-n` is also `--max-count`; `--limit` is an alias. On a git home it is git's output (see below). |
| `git log --oneline`, `--graph`, `--stat`, `-p`, `--author`, `--since`, `--grep`, `--format`, `--follow`, `<rev>..<rev>`, `<rev> -- <path>` | none | unsupported | Usage error (`129`). A revision range is read as a path. `pyn log` is of the workspace's branch; another branch's log needs a switch (`pyn log <branch>` is not built). |
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
| `git restore [-s <rev>] [-S] [-W] <path>...` (the work tree) | `pyn restore ...` on a git home | git home only | Takes the paths from the index (or `--source`) into the work tree, and with `--staged --worktree` into both. See [Branches, switching and the work tree](#branches-switching-and-the-work-tree). On a pyn server `--worktree` is a usage error (`129`): a restore there makes an older revision the new head. |
| `git restore -p`, `--ours`, `--theirs`, `--merge` | none | unsupported | Usage error (`129`). On a git home `pyn checkout --ours -- <path>` and `--theirs` take a side of an unmerged path. |
| `git branch`, `git switch`, `git checkout` | `pyn branch`, `pyn switch`, `pyn checkout` | supported | See [branching and merging](#branching-and-merging). On a git home they work on `.git` as git does, see [Branches, switching and the work tree](#branches-switching-and-the-work-tree). |
| `git merge` | `pyn merge` | supported | See [branching and merging](#branching-and-merging). On a git home it merges as git does, see [Merging](#merging). |
| `git fetch`, `git pull`, `git push` | none | planned | Git-command message. Remotes and sync #237 under epic #205 (worked with #223). Today a commit goes straight to the server and `pyn update` brings the workspace forward. |
| `git remote` | `pyn remote` | git home; pyn server partly | On a git home it lists and edits the remotes in `.git/config`, see [Remotes](#remotes). On a pyn server `pyn remote` and `pyn remote -v` show the one remote, `origin`: the server and repository set by `--server` and `--repo` or `pyn config set server` and `repo`; the editing subcommands say so and exit `1`. |
| `git tag [-a] [-m <msg>] [-f] [<name> [<commit>]]`, `-d`, `-l` | `pyn tag ...` on a git home | git home only | See [Tags](#tags). On a pyn server it prints the git-command message (a server's commits are named by hash and have no tags yet, #223). |
| `git stash [push] [-u] [-m <msg>]`, `list`, `pop`, `apply`, `drop`, `clear` | `pyn stash ...` on a git home | git home only | See [Stashing](#stashing). On a pyn server it prints the git-command message. |

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

Branches and merge exist on a pyn server ([branches](branches.md), [merging](merging.md)); remotes on a pyn server are the one server and repository (see [Remotes](#remotes)). The model is
decision #233 (commit graph, per-commit trees, branch heads); policy per branch is [branches](branches.md#policy-and-modes); exclusive paths keep one linear
history across branches (decision #28).

| Git | pyn | Status | Notes |
| --- | --- | --- | --- |
| `git branch [<name> [<start>]]`, `-d`, `-D`, `-m`, `-l`, `-v`, `--show-current` | `pyn branch ...` | supported | `-d` refuses a branch that is not on the default branch (git: not merged into `HEAD`). `-a` and `-r` need remotes (#237) and `-M`, `-c` and patterns are not built; they are usage errors. The workspace's branch cannot be deleted. |
| `git switch <branch>`, `-c <new> [<start>]`, `-f` | `pyn switch ...` | supported | Local changes carry over where the target branch has the same file, and stop the switch (nothing changed, the paths listed) where it does not; `-f` discards them. `-`, `--detach` and `-C` are not built. Prints `Switched to branch '<name>'` on standard error, as git. |
| `git checkout <branch>`, `-b <new> [<start>]`, `-f`, `[<tree-ish>] -- <path>...` | `pyn checkout ...` | supported | Reserved for git's meaning. `-- <path>` restores from the revision the workspace has (git: the index; a staged path must be unstaged first), `<branch\|commit> -- <path>` from that branch or commit. There is no detached HEAD: `checkout <commit>` is refused with what to use instead. Taking a lock is `pyn lock`. |
| `git merge <branch\|commit>`, `-m`, `--no-ff`, `--ff-only`, `--no-commit`, `--abort`, `--continue` | `pyn merge ...` | supported | Shared files merge three-way with diff3 conflict markers; a binary conflicts as a whole file. Exclusive files take the later revision, and conflict only where a path became exclusive while a branch edited it (a person chooses, taking the lock). A conflicted merge leaves the workspace as git does and is concluded with `pyn add` and `pyn commit` (no `-m` needed), or given up with `--abort`. Differences: `--preview` (pyn's own) reports what would merge and conflict and changes nothing; a merge needs a clean index and no local edits to the files it touches; `--squash`, `-s`, `-X`, `--no-verify`, `--edit`, `--signoff`, `-S` and `--ff` are usage errors. `git checkout --ours` and `--theirs` are `pyn checkout --ours\|--theirs -- <path>`. `pyn status` marks unmerged paths in its `STATE` column. Recursive merge of criss-cross histories as git does, with the base of a conflict shown as `merged common ancestors`. |
| `git rebase`, `git cherry-pick`, `git revert` | none | unsupported | See [Not planned](#not-planned). |

## Remotes

| Git | pyn | Status | Notes |
| --- | --- | --- | --- |
| `git remote [-v]`, `add`, `remove`, `rename`, `set-url`, `get-url` | `pyn remote ...` | git home: supported | Configuration only, no network. Output, exit statuses and the resulting `.git/config`, refs and reflogs match git. See [Remotes on a git home](#remotes-on-a-git-home). On a pyn server: `remote` and `remote -v` only, see the table above. |
| `git ls-remote [--heads] [--tags] [--refs] [--symref] [--exit-code] [<repository> [<pattern>...]]` | `pyn ls-remote ...` | same | Works on a git home and anywhere with a URL. Output is git's: `<id>`, a tab and the name, with `<tag>^{}` for a peeled tag and `ref: <target>`, a tab and `HEAD` for `--symref`. `<repository>` is a remote name (default `origin`, which prints `From <url>` on standard error as git does) or a URL; see [Transports and credentials](#transports-and-credentials). `--get-url`, `--sort`, `-o`, `-q` and `--upload-pack` are usage errors (`129`). |
| `git remote show`, `add -f`, `prune`, `update`, `set-head`, `set-branches` | none | `show` and `add -f` need fetch (#306); the rest unsupported | `show` and `add -f` exit `1` and name #306 and change nothing; the others are usage errors (`129`). |
| `git fetch [<remote>]` | none (`pyn update`) | planned | #237. |
| `git pull [<remote> <branch>]` | none (`pyn update`) | planned | #237. |
| `git push [<remote> <branch>]` | none | planned | #237. Today `pyn commit` already writes to the server, so `push` has nothing to publish until local-only commits exist. |
| `git clone` | `pyn clone` | same | See the daily set. |

### Remotes on a git home

`pyn remote` reads and edits `remote.<name>.*` and `branch.<name>.*` in the repository's `.git/config` and the refs under
`refs/remotes/`. It never contacts a remote.

| Command | Does |
| --- | --- |
| `pyn remote` | The remote names, sorted, one per line. |
| `pyn remote -v` | `<name>`, a tab and `<url> (fetch)` (the first URL), then one `<url> (push)` line for each push URL (`pushurl`, else each `url`). A remote with no URL prints `<name>` and a tab. |
| `pyn remote add <name> <url>` | Writes `url` and the default `fetch = +refs/heads/*:refs/remotes/<name>/*`. An invalid name is `fatal: '<name>' is not a valid remote name` (`128`), an existing one `error: remote <name> already exists.` (`3`). `-f` needs fetch (#306). |
| `pyn remote remove\|rm <name>` | Deletes the `[remote]` section, every ref and reflog under `refs/remotes/<name>/`, and the `branch.<b>.remote`, `merge` and `pushRemote` of branches that tracked it, and `remote.pushDefault` if it named it. A missing remote is `error: No such remote: '<name>'` (`2`). |
| `pyn remote rename <old> <new>` | Renames the section, moves `refs/remotes/<old>/*` (and its reflogs, with a `remote: renamed ...` line) to `<new>`, rewrites fetch refspecs whose destination is under `refs/remotes/<old>/`, and repoints `branch.<b>.remote`, `pushRemote` and `remote.pushDefault`. A refspec that does not fit is kept and reported as git does (`warning: Not updating non-default fetch refspec`). |
| `pyn remote set-url [--push] <name> <new> [<old>]` | Sets the URL, or replaces the URLs that contain `<old>`. `--add` appends one, `--delete` removes the URLs that contain `<url>` (refusing to remove every non-push URL). |
| `pyn remote get-url [--push] [--all] <name>` | The first (or every) URL; with `--push` the push URLs. |

Editing keeps every other line of `.git/config`, with its comments and order, and quotes values and subsection names as git does.
Differences from git: `<old>` and the `--delete` argument match as plain substrings, not regular expressions; `url.<base>.insteadOf` is not applied
to the URLs shown; legacy `.git/remotes/` and `.git/branches/` files are not read; `remove` deletes the refs under
`refs/remotes/<name>/` rather than those the fetch refspecs map.

## Transports and credentials

Decisions #245 and #255. pyn reaches a git remote without git: the protocol, the pack handling and the credential flow are
implemented in `pyn-gittransport`, `pyn-gitwire` and `pyn-git`. **git does not need to be installed.** pyn never starts a `git`
program; `make boundary-check` fails when non-test source under `crates/` spawns one. The programs pyn can start are the system
`ssh` client, and the credential helpers and askpass programs your own configuration names.

Today `pyn ls-remote` uses them; `fetch`, `pull`, `push`, `clone <git url>` and `remote add -f` arrive on top of the same
connections (epic #301). The library functions `fetch_objects` and `push_objects` (thin packs and offset deltas on fetch, a
streamed pack file on push) are what those commands will call.

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
  `http.<url>.<key>`. Not read: client certificates, `http.cookieFile`, `http.lowSpeedLimit`, `http.version`.
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

| Helper | Behaviour |
| --- | --- |
| `store [--file <path>]` | In process. Reads `~/.git-credentials` then `$XDG_CONFIG_HOME/git/credentials` (or only `<path>`), one URL per line with percent-encoded user and password, as git's `store` does; writes the first file that exists (else `~/.git-credentials`) with mode `0600`. |
| `cache` | There is no daemon in process; `git-credential-cache` is used through the helper protocol when it is on `PATH` and skipped otherwise. |
| `!<command>` | Run through `sh` with `get`, `store` or `erase` appended. |
| `/absolute/path [args]` | Run through `sh` the same way. |
| any other name, such as `osxkeychain` or `manager` | The program `git-credential-<name>` through the helper protocol (`key=value` lines ended by a blank line on standard input; `get` replies on standard output), when it is on `PATH`. A helper that is not installed is skipped silently; one that fails is ignored, as git does. |

Differences from git: no `credential.<url>.*` keys other than `helper`, `username` and `useHttpPath`; no `credential.interactive`,
`credentialCache`, partial `capability` negotiation or `authtype`; no `include` directives in config files.

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

**Nothing git sees changes except what you asked for.** Hooks, notes, configuration and line endings are never touched, and
the work tree changes only where a command you ran says it does (`switch`, `checkout`, `restore`, `merge`, `stash`), never
over local changes you have not saved unless you forced it. Reading commands change nothing in `.git/`. `pyn init` writes
`info/exclude` once. Each writing command writes what git writes for the same command (objects, refs, reflog lines, the index,
`ORIG_HEAD` and the `MERGE_*` files), and the sections below list exactly what. A test asserts that the `.git` directory and
the work tree are byte-identical before and after every reading command and after every refusal, and the writing commands
are compared with what git left on disk for the same commands.

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
| `branch`, `switch`, `checkout`, `restore` | See [Branches, switching and the work tree](#branches-switching-and-the-work-tree). |
| `merge` | See [Merging](#merging). |
| `tag` | See [Tags](#tags). |
| `stash` | See [Stashing](#stashing). |
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
- `merge` and `stash apply` detect no renames, write diff3 conflict markers whatever `merge.conflictStyle` says, merge only when
  there is one merge base, and do not write `AUTO_MERGE`; `switch` and `checkout` do not warn about commits left behind by
  leaving a detached `HEAD`, and print their own wording where git's differs (`Fast-forward` without a diffstat, the status
  after `stash pop` in pyn's table).

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
none; an existing log is appended to either way. The lines are `commit: <subject>` (`commit (initial)`, `commit (merge)`),
`checkout: moving from <old> to <new>`, `branch: Created from <start>`, `Branch: renamed <old> to <new>`, `merge <name>:
Fast-forward` or `Merge made by the 'ort' strategy.`, `reset: moving to HEAD` and the stash message. A line names the committer
identity (with the committer date from `GIT_COMMITTER_DATE`); with no identity configured a reflog line falls back to the login name
so branching and switching still work. A moved branch is also logged in `HEAD`'s log, as git does.

### What pyn will not write

Any command that would write a work tree file stops first, and changes nothing, when git would convert what it writes: `core.autocrlf`
set to true, `core.eol=crlf` with a `text` attribute, `eol=crlf`, a `filter` other than Git LFS, `ident` or `working-tree-encoding`
(`.gitattributes` in the work tree, `.git/info/attributes` and the global attributes file are read; `-text`, `binary`, `eol=lf`,
symlinks and submodules are never converted). A Git LFS filter writes the pointer file as it is, with a warning (decision #245).
The same limit applies to `status`, which compares raw bytes (#273). Hooks are never run; one warning names the hooks that exist
for the command (`post-checkout`, `pre-merge-commit`, `commit-msg` and the like). Sparse checkouts, submodule updates, the split
index and `GIT_DIR` are not supported, and `rebase`, `cherry-pick`, `revert`, `--squash` and anything that talks to a remote other than `ls-remote` are not built yet (`fetch`, `pull`, `push` and `clone`, epic #301; see [Transports and credentials](#transports-and-credentials)).

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
| `fetch`, `pull`, `push`, `update` | Git transport, #255. |
| `get` | Per-path revisions, which git has no equivalent of; not scheduled. |

`pyn convert` fails with `pyn: 'convert' is not available yet` and names #95.

### Packs (library, provisional)

`pyn-git` holds the pack plumbing the git transport uses (see [Transports and credentials](#transports-and-credentials));
`pyn-gittransport` calls it, and no command other than the tests exercises fetch or push yet.

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
