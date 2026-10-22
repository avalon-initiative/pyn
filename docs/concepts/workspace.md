# The `.pyn/` folder

Everything pyn keeps in a workspace lives in one folder at the workspace root, `.pyn/`, the way a git repository keeps
its files in `.git/`. A developer's project directory stays free of pyn files, and there is one obvious place for
anything related to pyn.

```text
.pyn/
  pyn.toml          the repository's collaboration policy (shared)
  ignore            paths pyn leaves alone (shared)
  local_only/       never tracked
    ignore          personal ignore patterns, added to the shared ignore
    config.toml     workspace settings: server, repository, identity
    repo.toml       the repo_tool marker: whether this checkout acts as git or pyn (optional)
    state/          which revision each local file is at, and which branch the workspace is on
    cache/          downloaded content
```

The ignore file is `.pyn/ignore`, not a `.pynignore` in the project root.

## Shared and local files

Everything in `.pyn/` is shared and tracked as part of the repository, except the `local_only/` folder (a reserved name: the server refuses new writes under it, see [path rules](pyn-toml.md#path-rules-for-new-writes)), which is never
tracked. A file's place in the tree says which it is.

## Overriding settings

The defaults are all under `.pyn/`, and every setting and location can be overridden. The first of these that has a value
wins:

1. command-line flags
2. environment variables (`PYN_*`)
3. the workspace configuration, `.pyn/local_only/config.toml`
4. the user configuration, `~/.config/pyn/config.toml` (`$XDG_CONFIG_HOME/pyn/config.toml` when set)
5. built-in defaults

`pyn config get` and `pyn config set` read and write settings, with `--local` for the workspace and `--global` for the
user, like `git config`. `PYN_DIR` relocates the whole folder, and individual files such as the ignore file can be
pointed elsewhere.

## The policy file

The collaboration policy is `.pyn/pyn.toml`, shared like any other file in the folder and described in
[pyn.toml](pyn-toml.md). It is not called `config.toml`, because the server enforces it and a local file cannot override
it, whereas the local settings layer in the order above.

## In a git repository

In a git repository `.pyn/` holds only pyn's own state, in `local_only/`, and `pyn init` hides it from git by adding `.pyn/` to
`.git/info/exclude`, so git never sees it. The policy file is not read there: a git repository has no exclusive paths. See
[git compatibility](git-compatibility.md#on-a-git-home) for how pyn tells a git repository from a pyn server workspace.

## The `repo_tool` marker

`.pyn/local_only/repo.toml` holds one key, `repo_tool`, with the value `git` or `pyn`:

```toml
repo_tool = "git"
```

It says which tool the checkout acts as. It is local to the checkout and never versioned, so a teammate's checkout of the same
git repository is unaffected. `pyn init` in a git repository records `git`, and `pyn convert` will set it to `pyn`, after
which pyn's configs and server features apply even though a `.git` remains. A repository created or cloned from a pyn server
needs no marker: absence means `pyn`. A present marker wins over inferring the home from `.git` and the clone's state files,
and an unknown value is an error. The nearest folder that makes a claim decides; the full rule is in
[which home](git-compatibility.md#which-home).

## Working in a workspace

`pyn clone <server>/owner/name [dir]` (or `pyn clone owner/name` on the configured server) creates the folder, which defaults to
a folder named after the repository, downloads every file at its head revision (or as of a commit with `--revision`, see [commits](commits.md)), records the repository in
`.pyn/local_only/config.toml`, and records in `.pyn/local_only/state/` which revision each file is at and a hash of its content. After that, commands work from wherever
you are inside the workspace and take paths relative to your current directory.

- **Read-only exclusive files.** Exclusive files are read-only until you hold the lock. `pyn lock` makes the file
  writable; `pyn commit` and `pyn unlock` make it read-only again. Shared files are always writable.
- **No base revisions to remember.** `lock` and `commit` use the revision the workspace has, so `--base` is only for
  working outside a workspace. If the server has moved on, the command tells you to run `pyn update`.
- **Branches.** A workspace is on one [branch](branches.md), `main` until `pyn switch` or `pyn checkout` changes it, and every
  command works on that branch. `pyn status` starts with `On branch <name>`.
- **`pyn status`** prints a table of what differs, one row per file, with the columns `MODE`, `REV`, `STAGED`, `STATE`,
  `LOCK` and `PATH`, then a one-line summary of counts (staged, modified, behind, new, untracked, locked by you, locked by
  others). `STAGED` is `added`, `modified` or `deleted` for a path in the staging index and `-` otherwise. `STATE`
  is `modified` (an edit after what was staged, or since the last update when nothing is staged), `deleted locally` (a deletion not staged yet), `behind (head rN)`, `new` (on the server, not downloaded yet),
  `untracked`, or `clean` for a file that is only listed because it is locked. `LOCK` shows the holder (`you` for your
  own locks) and the time left, for example `alice · 7h 59m`. A path that is locked but has no revision yet shows
  `locked, not checked in yet`; `pyn update` has nothing to fetch for it. Clean, unlocked files are left out, and a
  workspace with no differences and no locks prints `everything is up to date`.
- **`pyn update`** downloads new and newer files. A file with local changes is never overwritten; it is reported as skipped.
  `--revision <commit>` brings the workspace to a past (or newer) commit instead, and `pyn status --revision <commit>` compares
  with it; `pyn diff` shows the changes themselves. See [commits](commits.md).
- **File modes and symbolic links.** The executable bit (`100755`) and symbolic links (`120000`) are tracked as in git.
  `pyn commit` records a file's mode with its content; a symbolic link is committed as its target path and is never
  followed. `pyn clone` and `pyn update` recreate both (on systems with an executable bit and symlinks), and a mode change
  alone shows as `modified` in `pyn status` and as `old mode`/`new mode` in `pyn diff`. The mode is stored with each
  revision and in `.pyn/local_only/state/files.toml` (`file_mode`).
- **`pyn add`, `pyn commit`** stage changes and record them as one commit; see [Staging](#staging). **`pyn commit <path>...`** still
  uploads the files at the given paths as one atomic commit with one message (`-m`), whatever is staged; with several paths,
  all succeed or none do. Outside a workspace, one path and `--file <file>` name the upload.

## Staging

Shared files follow git (decision #245, issue #253): `pyn add <path>...` stages a file, a folder or `.`, and `pyn commit -m
<message>` records everything staged as **one** commit, which can span many files and goes to the server as one changeset
(all paths succeed or none do). `-u` and `--update` stage only files already tracked, `-A` and `--all` also take new ones (with
no path, from the whole workspace), `-n` and `--dry-run` list what would be staged, `-v` prints each path, and `-f` stages
files `.pyn/ignore` hides, which are otherwise refused with exit status `1`. `pyn commit -a` (and `-am "message"`) stages every
changed tracked file first; new files are not included. `pyn restore --staged <path>...` takes paths out of the staged set and
leaves the files alone. `pyn diff --cached` shows what is staged against the last update, and `pyn diff` shows the edits made
since. A tracked file that was deleted is staged as a removal (`add <path>`, `add -u`, `commit -a`; `-n` prints `remove '<path>'`),
and `pyn commit` sends it with the rest as a deletion in the changeset; the commit then prints `<path> was deleted at revision N` and the
workspace stops tracking the path. `pyn diff --cached` shows it as a deleted file. Deleting an exclusive file is staged like
changing one, only with the lock held, and the commit consumes the lock. `pyn update` removes a file the server deleted when the
workspace has no changes to it, and reports it as skipped when it has. See [deleting a path](README.md#deleting-a-path).

Exclusive files are different. `pyn add` on an exclusive path fails early, staging nothing, unless you hold its lock, and
the message names `pyn lock <path>`. Lock, edit, commit stays the exclusive flow, and the commit releases the lock. The check
is a convenience: the server still enforces the lock when it receives the commit. A new file's mode is only known to the
server, so `add` cannot refuse it, and the commit is where an unlocked exclusive path is turned away.

`add` copies the file's content aside when it stages it, so an edit made afterwards stays unstaged and the commit sends what
was staged. The staging index is `.pyn/local_only/state/index.toml`:

```toml
version = 1

[entries."Source/a.cpp"]
hash = "<blob id of the staged content>"
file_mode = "regular"          # regular, executable or symlink
base_revision = 3              # the revision the workspace was at; the commit is rejected if the server moved on
```

and the staged content is `.pyn/local_only/staged/<blob id>` (a symbolic link's target for a link). Both live in
`local_only/`, so they are never shared; an empty index has no file, and committing or unstaging removes content nothing
refers to. `pyn update` skips a staged file and says so. An index in a newer version than this pyn understands is refused.

## Merging

`pyn merge <branch>` merges into the workspace's branch ([merging](merging.md) has the rules). A merge that fast-forwards or merges
cleanly is done when the command returns, and the workspace follows it. A merge with conflicts, or one stopped with
`--no-commit`, is **in progress** until `pyn commit` concludes it or `pyn merge --abort` gives it up. While it is in progress:

- The files that merged cleanly are in the working tree, text conflicts are in it with diff3 markers (`<<<<<<< HEAD`,
  `||||||| base`, `=======`, `>>>>>>> <branch>`), and a binary conflict or a conflict on an exclusive path leaves our version in the
  file. `pyn checkout --ours -- <path>` and `--theirs` put either side's version in the file.
- `pyn status` says `You have unmerged paths`, and its `STATE` column says `unmerged (both modified)` (or `both added`, `deleted
  on one side`, `converted to exclusive`) for each path still to resolve, and `merged` for the paths that merged cleanly.
- `pyn add <path>` marks a conflicted path resolved by staging what is in the file, even when it still equals our version, and warns
  if conflict markers remain. An exclusive path does not need its lock to be staged, only to be concluded with a new revision
  (a diverged path always needs one; the server asks, and the message names `pyn lock --base N <path>`).
- `pyn commit` (with or without `-m`) concludes the merge once every conflicted path is staged. Anything else staged must be a
  path of the merge. A partial `pyn commit <path>`, `pyn merge`, `pyn switch` and `pyn update` are refused.

A `pyn pull` that stops on conflicts in a workspace with local commits is in progress the same way, concluded with `pyn commit` or
`pyn pull --continue` and given up with `pyn pull --abort` (or `pyn merge --abort`); see
[conflicts in a pull](commits.md#conflicts-in-a-pull).

The record is `.pyn/local_only/state/merge.toml`:

```toml
version = 1
branch = "main"                  # the branch being merged into
source = "feature"               # what was typed; the commit it named is source_commit
source_commit = "<commit id>"
target_commit = "<commit id>"    # the branch's head when the merge started; the merge fails with merge_stale if it moved
message = "Merge branch 'feature' into main"

[paths."Source/a.cpp"]
mode = "shared"
conflict = "both modified"       # absent for a path that merged cleanly
[paths."Source/a.cpp".ours]
revision = 3
file_mode = "100644"
```

Nothing is sent to the server until the merge is concluded, so aborting changes nothing there.

## Listing output

Every command that lists things (`status`, `locks`, `files`, `ls`, `log`, `audit`, `summary`, `repo list`, `repo explore`, `key list`,
`invite list`, `token list`, `member list`, `role list`, `config list`) prints one aligned table with a header row and
no colour. Columns are separated by two spaces and fixed-width columns come first; the column that grows most (path,
message, detail) comes last and is not padded, so a long value never pushes the others around. A cell with nothing to
show is `-`, and an empty list prints a short sentence such as `no locks` instead of a bare header. Scripts should read
columns by position from the header rather than split on tabs; revisions are written `rN`.

Dates and times follow one format everywhere the CLI prints them, in tables and in messages. The server sends UTC; the
CLI converts to the operating system's timezone (set `TZ` to override it). A date is `Mon DD YYYY` with a three-letter
month (`Jun 09 2026`), a time is `HH:MM` on a 24-hour clock with no seconds or fractions, and a timestamp is both
(`Jun 09 2026 14:05`). A raw UTC timestamp inside a server message, such as the end of a lock in "locked by alice until
...", is rewritten the same way. The time left on a lock stays relative (`7h 59m`).

## The ignore file

`.pyn/ignore` lists paths that `pyn status` should not report as untracked, one pattern per line, with `#` for comments.
A pattern ending in `/` ignores that folder wherever it appears (`node_modules/`), a pattern without a `/` matches at any
depth (`*.log`), and a pattern containing a `/` is relative to the workspace root (`Saved/cache.bin`). The file is shared
like the rest of `.pyn/`, so a team keeps one list.

### Personal ignore

`.pyn/local_only/ignore` has the same syntax and is for exclusions that are yours alone, as `.git/info/exclude` is in
git. It is applied in addition to `.pyn/ignore`: a path is ignored if either file matches, and there are no negations,
so neither file can un-ignore what the other lists. It is never versioned or pushed and affects only that workspace.
`pyn status` and commit read both files. `.pyn/local_only/` itself is always ignored and never needs an entry.

The split elsewhere in `.pyn/` follows the same rule: the policy (`pyn.toml`) and the shared ignore are repository
content, while `config.toml`, `state/` and `cache/` are per-machine and live only in `local_only/`.

## Paths written to the work tree

Every command that writes, removes or renames a file in a workspace checks the whole set of paths first and, if any is
refused, stops with an error that names it and changes nothing. A path is refused when it is empty or has an empty, `.` or `..` component,
is absolute or starts with a drive or share prefix, contains a backslash or a control character, has more than 1024 components or 4096
bytes, or has a first component of `.pyn` (other than the shared `.pyn/pyn.toml` and `.pyn/ignore`) or any component of `.git`.
Names are compared case-insensitively and after the folding that macOS and Windows apply (ignorable Unicode characters, short
names such as `git~1`, trailing dots and spaces). On Windows, device names such as `con` and the characters `: < > " | ? *` are refused
too. The same check applies in a git repository.

A workspace never writes through a symbolic link. A command stops when a folder above a path is a link or a file, and a file
that is a link where a new version arrives is replaced rather than followed. A downloaded symbolic link whose target is
absolute, or climbs above the workspace root, is refused. In a git repository links are made as git makes them, but nothing is written through a link that is already there.

## Settings

`pyn config get|set|list` reads and writes settings (`pyn config init` runs the first-run prompt). For now there are four: `server`, `user` (the development
identity), `repo` (`owner/name`, set by `pyn clone`) and `local_store` (`true` or `false`, see [the local store](#the-local-store)). `--repo` or `PYN_REPO` beats the workspace's `repo`, so one
workspace can still reach another repository for a single command. Outside a workspace, repository commands need one of them. `set` writes the workspace's file by default and your user file with `--global`; `get` shows the effective value,
or one layer with `--local` or `--global`. A `PYN_SERVER` or `--server` always wins, and `PYN_DIR` points at a `.pyn`
folder kept somewhere else.

## The local store

On a pyn server a workspace keeps its commits locally (decision #300, epic #313): `pyn commit` records a commit in a
partial local store and `pyn push` publishes it. A new clone has the store from the start. The `local_store` setting
controls it:

- `pyn clone <source>` turns it on for the new workspace (`--local-store` says the same and is accepted for scripts);
- `pyn clone --no-local-store <source>` opts out: the workspace has no store, `pyn commit` goes straight to the server and
  `pyn push` says there is nothing to push, as before local commits existed;
- `pyn config set --global local_store false` makes `pyn clone` opt out unless `--local-store` is given;
- `pyn config set local_store true` turns it on in an existing workspace, and the store is filled by the next `pyn update`
  (`pyn fetch` and `pyn pull` then fill it from the server's commits instead, see below); `pyn migrate` is the safe way;
- `pyn config set local_store false` stops further updates of the store; the folder stays until you delete it.

A server too old for local commits is detected from `GET /v1/capabilities` (`local_commits`); a clone that wants the store
and `pyn migrate` refuse against it with a message to upgrade the server or opt out. The web app and workspaceless use keep
the direct routes (`POST .../changeset` and `.../files/{path}`), which the server still serves.

### Migrating a workspace

A workspace made before local commits has no `local_store` setting. `pyn migrate` turns it on and builds the store
(history, and the blobs of the checked-out files). It changes nothing and fails with the reasons listed when the workspace
has staged changes, files changed in the working tree, a merge in progress, or is not up to date with the server (run
`pyn update` first). A workspace that already keeps local commits prints that and does nothing, so the command can be
repeated. If building the store fails the setting is put back.

The store is `.pyn/local_only/store/`, a bare repository in git's on-disk format using the SHA-256 object format that a
pyn server uses, so its ids are the server's own ids:

```text
store/
  HEAD                         symbolic ref to refs/heads/<branch>
  config                       repository format (SHA-256, bare)
  objects/                     loose objects: commits, trees, and the blobs described below
  refs/remotes/origin/<branch> the server's tip of the branch as of the last update
  refs/heads/<branch>          the local branch; moved to the server's tip while it has no local commits
```

What it holds:

- **Commits and trees for all history.** Every commit reachable from the branch, with its trees, under the id the server
  gives it. A commit and its trees are rebuilt from what the API already returns and checked against the server's ids; a
  mismatch fails the update instead of storing something different from the server.
- **Blobs only for what is checked out and for local work.** After a clone or an update the store holds the blobs of the
  files in the workspace. Every other blob is absent and is fetched on demand by id (`GET .../objects/{id}`), checked
  against the id and stored.

The store is partial by design, so a tool that walks the whole object graph reports missing objects: `git fsck` on the store,
or on a later git clone made from it, lists the blobs of files that were never checked out or edited here as missing
(commits and trees are all present). That is not corruption; the server holds every blob, and `pyn verify` checks the
server's side.

### Local commits (provisional)

With the store on, `add`, `commit`, `status` and `branch` work against the store (decision #300, slice #336):

- `pyn add` stages as before; an exclusive path still needs your lock to be staged.
- `pyn commit` records one commit on the local branch in `store/` and publishes nothing: the commit is git-encoded with
  SHA-256 ids, so the id is the one the server will accept on push, and the blobs of the staged files go into the store.
  The output says the commit is local and not yet published. The author and committer are your account name with the
  placeholder email `<name>@pyn.invalid` the server uses for an account without one.
- Before it commits, `pyn commit` checks that every exclusive path it changes is locked by you and refuses otherwise.
  This is a convenience for the client: the server enforces the lock when the commits are pushed (decision #300 item 5),
  and a lock is a lease that can lapse while you work offline. Shared paths need no lock.
- `pyn status` adds `added|modified|deleted in a local commit` to the paths the unpublished commits changed, and a
  closing line with the number of local commits the server lacks.
- `pyn update` brings the server's tip and its history into the store but leaves the local branch and the files that
  local commits changed alone (it reports them as skipped).
- `pyn log --commits` lists the unpublished commits above the server's (the hash is marked `(local)`), and `pyn show [<commit>]`
  shows one (`HEAD` or a hash of an unpublished commit; anything else is read from the server). `pyn log` and
  `pyn log <path>` list the unpublished commits first, newest first, one row per changed path with `local` as the `REV`
  and `(deleted)` after the message when the commit removes the path; `--filter` and `-n`/`--limit` apply to them too.
- `pyn commit <path>` on a file that has not changed since its last revision says `nothing to commit` (exit `1`).
- `pyn status` shows `local` as the revision of a file only a local commit has added.
- A file stays writable after a local commit while you still hold its lock; it goes read-only when you unlock.
- `pyn branch -v` shows the local tip, and how many commits are unpublished, for the branch the workspace is on.
  Creating, renaming and deleting branches still act on the server.
- `pyn switch` and `pyn checkout <branch>` refuse to leave a branch that has unpublished local commits.
- `pyn reset [--soft|--mixed|--hard] [<commit>]` moves the local branch back to `HEAD~N`, a hash on its unpublished chain, or
  the last published commit (never further), which is the way out of a commit the server will not accept. `--soft` keeps the
  dropped changes staged, `--mixed` (the default) keeps them in the files unstaged and clears the staging area, `--hard` puts
  the files back to the commit and asks you to type the branch name (`--yes` skips that). Untracked files are left alone.
- `pyn lock <path>` for a path a local commit changed takes the server's head revision as the base, so the lock the pull hint
  asks for works after another person pushed a newer revision of it; `pyn pull` then keeps your version.

Unchanged by the store: `pyn commit <path> --file ... [--base ...]` and `pyn merge` still publish straight to the
server, and the server stays the only authority over commits, locks and policy. Only the current branch is stored.

### Fetch, pull and push (provisional)

Slice #337 of the same move. With the setting on, `pyn fetch` stores the server's new commits and trees (every object checked
against its id) and moves `refs/remotes/origin/<branch>`; `pyn pull` is a fetch and then a fast-forward or, when the
histories diverged, a merge commit as in git (`pyn pull --rebase` replays the local commits onto the fetched tip instead); a conflict stops either in progress for the person to resolve;
`pyn push` uploads the blobs and publishes the local commits, merge commits included, in one request, after which none of
them is unpublished. Details, output and the refusals are in
[commits](commits.md#fetch-pull-and-push-with-a-local-store). Deletions and policy-file commits are pushed like any other
commit. A merge commit is pushed too (issue #338): a descendant exclusive pointer is adopted without a lock, a shared
file is merged by content, and an exclusive path both sides changed needs the person's lock (provisional, see
[merge commits in a push](commits.md#merge-commits-in-a-push-provisional)).

### Limits on what a server may send

The CLI reads at most 256 MiB of JSON from one API response, waits at most 15 seconds to connect and 120 seconds for a response
or for the next chunk of a body (a long download is not cut off while data keeps arriving), and follows a redirect only on
the same host, so credentials never go elsewhere. One fetch stores at most 1,000,000 commits or 10,000 pages of history, and
nesting deeper than 1024 folders is refused.

## The user configuration file

The user file is `~/.config/pyn/config.toml` (`$XDG_CONFIG_HOME/pyn/config.toml`, or `$PYN_CONFIG_DIR/config.toml`) and
is read on every run. `PYN_CLI_CONFIG=/path/to/file.toml` makes the CLI read and write that TOML file instead; it is
separate from the server's `PYN_CONFIG`, which names a policy file. The keys are the settings above (`server`, `user`,
`repo`, `local_store`). Sign-in tokens never go in it: `pyn login` keeps them in `credentials.toml`, and no prompt asks for a password
or token.

The first time a command needs the server and none is set by a flag, `PYN_SERVER`, the workspace or a user file, and
the user file does not exist:

- in a terminal (stdin and stdout both), pyn offers to create the file: it asks for the server URL and the dev
  identity, shows the recommended or current value in brackets, and a blank answer accepts it;
- anywhere else (a pipe, CI, a script) the command fails with a message that names the missing `server` setting and the
  ways to set it, and never waits for input.

`pyn config init` runs the same prompt again at any time (terminal only), with the current values as the defaults.
`pyn config get|set|list` keep editing single values. When the file exists, a missing `server` falls back to the
built-in default, `http://127.0.0.1:7878`.
