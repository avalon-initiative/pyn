# Branches

A **branch** is a name that points at a [commit](commits.md). Commits are git commits, so a commit's id covers its parents,
and a branch moves only by committing on it. Decisions #233 and #245 set the model; this page is how a pyn server holds it.
[Merging](merging.md) branches is described on its own page. Remotes (#237), per-branch policy and the stranded-version report
(#238) are separate work and are not done here.

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

## Revisions and the single line of a path

Revisions stay one table per repository: a path's revisions are numbered `1, 2, 3, ...` in the order they were written,
whichever branch wrote them, and each [revision hash](integrity.md) chains to the path's previous revision in that order.
The v2 encoding is unchanged: it already covers the commit id, and the commit id covers the parents.

- An **exclusive** path has one linear history shared by every branch (decision #28). A commit on any branch appends to it
  under the path's lock, and it must be based on the path's newest revision on any branch, so the revisions form one line
  and never conflict. The branch's own pointer to the path does not have to be the newest: the commit moves it. A
  workspace on a branch that lacks the newest revision is told to update before it can lock (`stale_base`); merging the
  branch that has it (`pyn merge`) brings it, or `pyn get <path> --rev N` fetches it by number to lock with `--base N`.
- A **shared** path may be written on several branches at once. Its base is the revision the branch itself has, so two
  branches can both append a revision to the path; each branch sees the highest-numbered one in its own ancestry.

## The revision of a path at a commit

A path's revision at a commit is its highest-numbered revision made in that commit or in an ancestor of it (decision #245
item 9). The server keeps this as a cache per commit, `commit_pointers`: for each commit, the paths whose pointer differs
from its first parent. For an ordinary commit that is exactly the revisions it made; a [merge commit](merging.md) also has the
paths where the other parent's revision is higher, so a merge takes the later pointer by construction. A snapshot is read by walking the
first-parent chain and taking, per path, the pointer nearest the commit. Writing a commit costs one row per revision it
made, not one per file in the repository, and the cache is never rewritten: it is part of what `pyn verify` can recompute
from the revisions and their commits.

A snapshot, a folder listing, a file's content at the branch head and the base check of a shared commit all read through it.

## Policy and modes

The policy is the default branch's `.pyn/pyn.toml` and applies to every branch (decision #233, owner comments). A
branch cannot change a path's mode:

- A commit to `.pyn/pyn.toml` on another branch is allowed (it needs the `edit_policy` permission, the file must parse and,
  as for any path, the file's own mode decides whether it needs a lock) but it is only a file there. It applies nothing and
  releases no lock until a [merge](merging.md#what-a-merge-commit-holds) lands it on the default branch.
- A snapshot's modes come from the policy revision in the snapshot when that revision is on the default branch's history;
  a policy revision made only on another branch is ignored, and a commit that is not on the default branch's history takes
  the default branch's current policy.
- Trees are plain git trees and carry no mode beyond the git file mode, so a conversion applied on the default branch
  changes no stored tree.

Until per-branch policy (#238), a mode change made by a policy commit on the default branch applies at once to the checkin
rules on every branch. Converting a path to exclusive records its content on the default branch as the next revision of
the path's line, as before. A branch that diverged keeps its shared revision, which has a lower number than the conversion
revision, and cannot commit the path any more without the lock and a base on the newest revision. The data the stranded
report (#238) and the shared-to-exclusive conflict rule need is kept: each revision records the mode it was made under,
and a shared revision outside the default branch's ancestry with a number below the conversion revision is a diverged
version of that path.

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

`checkin`, `changesets` and `restore` take an optional `branch` in the body. `commits`, `snapshot`, `tree`, `files`,
`history` and `content` take an optional `branch` query parameter; `snapshot` takes either `commit` or `branch`. Every one
of them uses the default branch when none is named. `summary` describes the default branch and counts all branches. An
unknown branch is `404 branch_not_found`.

## Command line

A workspace is on one branch, recorded in `.pyn/local_only/state/branch` (the default branch when the file is absent).
Everything the workspace does, listing, `update`, `diff`, `log`, `commit`, `restore`, `get`, is scoped to that branch.
`pyn status` starts with `On branch <name>`, and `pyn log` prints it on standard error.

| Command | What it does |
| --- | --- |
| `pyn branch` | list the branches, `*` marks the workspace's; `-v` adds each branch's commit and subject; `--show-current` prints the name |
| `pyn branch <name> [<start>]` | create a branch at the workspace's branch (or at `<start>`: a branch, `HEAD~N`, a commit hash) |
| `pyn branch -d <name>...`, `-D` | delete branches merged into the default branch, or any with `-D`; the workspace's own branch is refused |
| `pyn branch -m [<old>] <new>` | rename (the workspace's branch by default) |
| `pyn switch <branch>`, `-c <new> [<start>]`, `-f` | bring the workspace to a branch's files |
| `pyn checkout <branch>`, `-b <new> [<start>]`, `-f` | the same, as in git |
| `pyn checkout -- <path>...` | put files back to the revision the workspace has; `.` is the folder you are in |
| `pyn checkout <branch\|commit> -- <path>...` | take the files as that branch or commit has them; they then show as modified |

A switch plans every path before touching one. A file the target branch has with other content, or does not have, and
that has local changes, and an untracked file in the way, stop the switch with a list of the paths and nothing changed;
files that are the same on both branches keep their local changes, as in git, and so do changes staged for them (they are
staged on the new branch's revision); `-f` discards what is in the way, staged changes included. After a
switch the workspace is clean. `pyn checkout <commit>` is refused: pyn has no detached HEAD, use `pyn checkout -b <name>
<commit>` or `pyn update --revision <commit>`.
