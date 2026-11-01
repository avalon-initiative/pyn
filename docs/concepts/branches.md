# Branches

A **branch** is a name that points at a [commit](commits.md). Commits are git commits, so a commit's id covers its parents,
and a branch moves only by committing on it. Decisions #233 and #245 set the model; this page is how a pyn server holds it.
[Merging](merging.md) branches is described on its own page. Remotes are separate work, see [Remotes](git-compatibility.md#remotes).

## Branches and the default branch

- A repository has one **default branch**, `main`, created with the repository. Its `.pyn/pyn.toml` is the repository's
  policy (decision #38). The default branch cannot be deleted or renamed.
- Any other branch is created from a commit (a branch's head, or any commit by id) and starts there. A repository with no
  commits has nothing to branch from, so only the default branch exists until the first commit.
- A branch moves only by a commit made on it: there is no reset. Deleting a branch removes the name; its commits,
  revisions and trees stay and are still reachable by id.
- Without `force`, only a branch whose commits are all on the default branch can be deleted (`branch_not_merged`
  otherwise), so a delete never hides work by accident.
- Names follow git's ref rules in the subset pyn needs: 1 to 200 characters, no whitespace or control characters, none of
  `~ ^ : ? * [ \`, no `..`, `@{` or `//`, no leading `-` or `/`, no trailing `/` or `.`, no component starting with `.` or
  ending in `.lock`, and not `@` or `HEAD`. A name cannot also be a folder of another name (`a` and `a/b` do not coexist).
- Creating, renaming and deleting a branch need the `checkin` permission and are written to the repository's
  [audit log](access.md) as `branch_created`, `branch_renamed` and `branch_deleted`.

## Commits on a branch

Every write names a branch, the default branch when it names none. A checkin, a restore or a changeset becomes one commit
whose parent is the branch's head; its tree is the parent's tree with the written files replaced, and the branch then
points at it. Writes to one repository are serialized, so the head a commit builds on is the head at the moment it is
written. Reading the history, a snapshot, a folder listing, the file list or a file's content likewise names a branch, and
the default branch when it does not.

## Revisions and the tree of a path

Revisions stay one table per repository: a path's revisions are numbered `1, 2, 3, ...` in the order they were written,
whichever branch wrote them, and each [revision hash](integrity.md) chains to the path's previous revision in that order.
The v2 encoding is unchanged: it already covers the commit id, and the commit id covers the parents.

- The revisions of an **exclusive** path form a tree (decision #299, which supersedes #28). Each records the revision it
  was based on (`base`, stored beside the revision and not covered by its hash; `null` when the branch had none), and
  numbers stay global and increasing. A commit appends under the path's lock, which is one per path across branches, and
  it must be based on the committing branch's own pointer for the path, so a commit on one branch never moves another
  branch's pointer and carries no unreleased content into it. A request whose base is not the branch's pointer is
  refused with `stale_base`; a workspace that lacks the branch's newest revision is told to update first (`pyn merge`
  brings another branch's), or `pyn get <path> --rev N` fetches a revision by number to lock with `--base N`.
  A single branch therefore sees one line, exactly as before. Revisions written before bases were recorded are based on
  the revision before them.
- A **shared** path may be written on several branches at once. Its base is the revision the branch itself has, so two
  branches can both append a revision to the path; each branch sees the highest-numbered one in its own ancestry.

## The revision of a path at a commit

A path's revision at a commit is its highest-numbered revision made in that commit or in an ancestor of it (decision #245
item 9). The server keeps this as a cache per commit, `commit_pointers`: for each commit, the paths whose pointer differs
from its first parent. For an ordinary commit that is exactly the revisions it made; a [merge commit](merging.md) also has the
paths where the other parent's revision is higher, so a merge that takes the descendant pointer records it. A snapshot is read by walking the
first-parent chain and taking, per path, the pointer nearest the commit. Writing a commit costs one row per revision it
made, not one per file in the repository, and the cache is never rewritten: it is part of what `pyn verify` can recompute
from the revisions and their commits. Verify recomputes each exclusive revision's pointer at its commit's first parent, on every
branch, and reports `base_mismatch` where the stored base differs; it also checks each commit's cache against the revisions the
commit made and the merge adopted, and reports `pointer_mismatch`.

A snapshot, a folder listing, a file's content at the branch head and the base check of a shared commit all read through it.

## Policy and modes

The policy is the default branch's `.pyn/pyn.toml` (decision #38), and a path's mode changes only by a commit to that file
on the default branch (decision #233, owner comments). A branch cannot change a path's mode; it takes a change when it gets
the commit that made it, by [merge or update](merging.md) from the default branch (decision #195: the change "takes effect
for a branch when that branch has the policy commit").

### The effective policy of a branch

- A **policy commit** is a commit carrying a revision of `.pyn/pyn.toml` that has reached the default branch: it is the
  default branch's head or one of its ancestors. A commit to the file on another branch is validated (it needs the
  `edit_policy` permission, the file must parse and, as for any path, the file's own mode decides whether it needs a lock) but
  is only a file there. It becomes a policy commit when a merge or fast-forward lands it on the default branch, and releases
  no lock and converts no path until then.
- The **effective policy of a commit** is the policy of the highest-numbered revision of `.pyn/pyn.toml` made in a policy
  commit that is that commit or one of its ancestors; the server's fallback rules (or all-exclusive) when there is none.
  The **effective policy of a branch** is that of its head.
- On the default branch this is the latest policy commit, which is what the server enforces, so a policy commit applies to
  the default branch at once. Any other branch keeps the policy it last picked up, whatever the default branch has done
  since, until a merge or update brings it the later policy commit. A branch created at a commit starts with that commit's
  policy.

The effective policy is what the branch's own operations use, each by the branch it names:

- a checkin, changeset or restore on a branch records revisions under the mode the branch's policy gives the path, and a
  path that is exclusive there needs the caller's lock (`lock_required`) and a base on the branch's own revision of it,
  as a shared one has;
- a lock request names the branch it is for (`branch`, the default branch if omitted) and is refused with `not_exclusive`
  only when the path is shared under the branch's policy and under the default branch's, so a branch that is about to take
  a conversion can still take the lock that resolving it needs;
- a snapshot, a folder listing and the file list of a branch or of any commit show the modes of that commit's effective
  policy;
- a [merge](merging.md) is decided under the policy the result has: the later of the two sides' effective policies (the
  higher-numbered policy revision), so merging the default branch into a branch brings the policy commit and its
  conversions to that branch, and a merge into the default branch uses the policy in force there.

The per-commit tree records no mode (trees are plain git trees, decision #245 item 9), so a conversion landing on the
default branch changes no stored tree and nothing needs rewriting when a branch picks it up: the branch's next effective
policy simply reads the path under its new mode.

### What a stale branch does

Because a branch keeps its policy until it picks up a later one:

- **Shared to exclusive on the default branch.** The default branch records a conversion revision (the next revision of the
  path's line). A branch that has not picked it up still treats the path as shared: it may keep committing the path without a
  lock, and each of those revisions is a shared revision outside the default branch's history, a **diverged version**. When
  the branch picks the policy commit up, the diverged version meets the conversion revision and the path is a conflict that
  is never merged automatically ([merging](merging.md#exclusive-paths)). The data is in the revisions: each records the mode
  it was made under, so a shared revision that the default branch's history does not contain, of a path that is exclusive
  there, is a diverged version whatever its number (a stale branch's revision made after the conversion has a higher number
  than the conversion revision, one made before has a lower one).
- **Exclusive to shared on the default branch.** Live locks on the path are released. A branch that has not picked it up still
  sees the path as exclusive: it can lock the path again (for that branch) and commit it under the lock, extending the
  single line. When the branch picks the policy up the path merges by content like any shared path.

[Policy changes](pyn-toml.md#changing-policy) report the stranded versions of the first case.

## Existing data

Existing history is one mainline. A one-time, idempotent startup step (`migrate_branches`, after `migrate_history`)
creates the default branch of every repository pointing at the newest commit and fills the pointer cache for commits
migrated by an earlier version; `migrate_history` writes the pointers of the commits it creates itself. Repositories
created afterwards get their default branch with the repository, and a repository written to before it is registered
gets one on its first commit. The migration runs on the database the server is started against; tests run it in scratch
schemas only.

## API

| Request | Returns |
| --- | --- |
| `GET /v1/repos/{owner}/{name}/branches` | the branches, the default one first, then by name |
| `POST .../branches` `{name, start?}` | the new branch; `start` is a branch name, or a commit id or abbreviation, the default branch's head if omitted |
| `GET .../branches/{branch}` | one branch (`404 branch_not_found`); a slash in the name may be sent as is or as `%2F` |
| `PATCH .../branches/{branch}` `{name}` | the renamed branch |
| `DELETE .../branches/{branch}?force=` | `204`; `409 branch_not_merged` without `force` for a branch that is not on the default branch |

`checkin`, `changesets`, `restore` and `checkout` (the lock request) take an optional `branch` in the body.
The lock request is refused with `not_exclusive` unless the path is exclusive under the branch's effective policy or the
default branch's. The response to checking in or restoring `.pyn/pyn.toml` on the default branch carries `stranded` (so does a [push](commits.md#pushed-commits) with a policy commit), and
`policy/preview` and the merge report carry it too ([stranded versions](pyn-toml.md#stranded-versions)). `commits`, `snapshot`, `tree`, `files`,
`history` and `content` take an optional `branch` query parameter; `snapshot` takes either `commit` or `branch`. Every one
of them uses the default branch when none is named. `summary` describes the default branch and counts all branches. An
unknown branch is `404 branch_not_found`.

## Command line

A workspace is on one branch, recorded in `.pyn/local_only/state/branch` (the default branch when the file is absent).
Everything the workspace does, listing, `update`, `diff`, `log`, `commit`, `restore`, `get`, `lock`, is scoped to that branch.
`pyn status` starts with `On branch <name>`, and `pyn log` prints it on standard error.

| Command | What it does |
| --- | --- |
| `pyn branch` | list the branches, `*` marks the workspace's; `-v` adds each branch's commit and subject; `--show-current` prints the name |
| `pyn branch <name> [<start>]` | create a branch at the workspace's branch (or at `<start>`: a branch, `HEAD~N`, a commit hash) |
| `pyn branch -d <name>...`, `-D` | delete branches merged into the default branch, or any with `-D`; the workspace's own branch is refused |
| `pyn branch -m [<old>] <new>`, `-M` | rename (the workspace's branch by default); `-M` replaces an existing `<new>` that is not the workspace's branch (local store only) |
| `pyn switch <branch>`, `-c <new> [<start>]`, `-f` | bring the workspace to a branch's files |
| `pyn switch -` | back to the branch the workspace was on before this one |
| `pyn switch --detach [<commit>]`, `pyn checkout <commit>` | put HEAD on a commit with no branch (local store only, see below) |
| `pyn checkout <branch>`, `-b <new> [<start>]`, `-f` | the same, as in git |
| `pyn checkout -- <path>...` | put files back to the revision the workspace has; `.` is the folder you are in |
| `pyn checkout <branch\|commit> -- <path>...` | take the files as that branch or commit has them; they then show as modified |

A switch plans every path before touching one. A file the target branch has with other content, or does not have, and
that has local changes, and an untracked file in the way, stop the switch with a list of the paths and nothing changed;
files that are the same on both branches keep their local changes, as in git, and so do changes staged for them (they are
staged on the new branch's revision); `-f` discards what is in the way, staged changes included. After a
switch the workspace is clean.

### Detached HEAD

In a workspace with the [local store](workspace.md#the-local-store), `pyn switch --detach [<commit>]`, `pyn checkout --detach
[<commit>]` and `pyn checkout <commit>` (a hash, a tag, `origin/<branch>`, `HEAD~N`) bring the files to that commit and leave HEAD on it
with no branch, as in git. `pyn status` says `HEAD detached at <id>` and `pyn branch` lists `* (HEAD detached at <id>)`;
`--show-current` prints nothing. `pyn commit`, `reset`, `merge`, `cherry-pick` and `rebase` move only HEAD. The commit is kept
in the store under the reserved name `HEAD`, so no branch can be called that. `pyn push` without a branch, `pull` and `update`
refuse while detached. Leaving by a switch to a branch drops the detached commits and warns, naming the first one, when they
are on no branch; `pyn switch -c <name>` first keeps them. A workspace without the local store has no detached HEAD:
`pyn checkout <commit>` is refused; use `pyn checkout -b <name> <commit>` or `pyn update --revision <commit>`.
