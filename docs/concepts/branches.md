# Branches

A **branch** is a name that points at a [commit](commits.md). Commits are git commits, so a commit's id covers its parents,
and a branch moves only by committing on it. Decisions #233 and #245 set the model; this page is how a pyn server
holds it. Merging branches (#236), remotes (#237), per-branch policy and stranded-version reporting (#238) are separate
work and are not described here as done.

## Branches and the default branch

- A repository has one **default branch**, `main`, created with the repository. Its `.pyn/pyn.toml` is the repository's
  policy (decision #38). The default branch cannot be deleted or renamed.
- Any other branch is created from a commit (the default branch's head unless another branch or a commit is named) and
  starts at that commit. A repository with no commits has no commit to branch from, so only the default branch exists.
- A branch is moved only by a commit made on it: there is no reset. Deleting a branch removes the name; its commits,
  revisions and trees stay.
- Names follow git's ref rules in the subset pyn needs: 1 to 200 characters, no whitespace or control characters, none
  of `~ ^ : ? * [ \`, no `..`, `@{`, `//`, no leading `-` or `/`, no trailing `/`, `.` or `.lock`, and not `@` or `HEAD`.
  A name cannot also be a folder of another name (`a` and `a/b` do not coexist).

## Commits on a branch

Every write names a branch (the default branch when it does not). A checkin or a changeset becomes one commit whose
parent is the branch's head, its tree is the parent's tree with the written files replaced, and the branch then points at
it. Writes to one repository are serialized, so the head a commit builds on is the head at the moment it is written.
Reading the history, snapshot, tree or a file's content likewise names a branch, and the default branch when it does not.

## Revisions and the single line of a path

Revisions stay one table per repository: a path's revisions are numbered `1, 2, 3, ...` in the order they were
written, whichever branch wrote them, and each [revision hash](integrity.md) chains to the path's previous revision in that
order. The v2 encoding is unchanged: it already covers the commit id, and the commit id covers the parents.

- An **exclusive** path has one linear history shared by every branch (decision #28). A commit on any branch appends to it
  under the path's lock, and it must be based on the path's newest revision, not just on the branch's own, so the
  revisions form one line and never conflict. A branch that has not yet taken the newest revision (no merge exists yet)
  cannot commit that path; the error is `stale_base`.
- A **shared** path may be written on several branches at once. Its base is the revision the branch itself has, so two
  branches can both append a revision to the path; which one the branch sees is a matter of ancestry, below.

## The revision of a path at a commit

A path's revision at a commit is its highest-numbered revision made in that commit or in an ancestor of it (decision
#245 item 9). The server keeps this as a cache per commit, `commit_pointers`: for each commit, the paths whose pointer
differs from its first parent, which for a commit today is exactly the revisions it made (a merge commit will add the
paths where the other parent's pointer is newer, so a merge takes the later pointer by construction). A snapshot is read
by walking the first-parent chain and taking, per path, the pointer nearest the commit; writing a commit costs one row
per revision it made, not one per file in the repository. `GET .../snapshot`, the tree and file listings, a file's
content at the branch head and the base check of a commit all read through it.

## Policy and modes

The policy is the default branch's `.pyn/pyn.toml` and applies to every branch (decision #233, owner comments). A
branch cannot change a path's mode: a commit to `.pyn/pyn.toml` on another branch is allowed (it needs the
`edit_policy` permission and the file must parse) but is only a file there; it does not apply and moves no lock until a
merge lands it on the default branch. A snapshot's modes come from the policy revision that is on the default branch's
history; a policy revision made only on another branch is ignored. Trees are plain git trees and carry no mode beyond
the git file mode, so a conversion applied on the default branch needs no change to any stored tree.

Until per-branch policy (#238), a mode change made by a policy commit on the default branch applies at once to the
checkin rules on every branch. A path that became exclusive can no longer be committed on a branch that has not taken
the conversion: its base is older than the path's newest revision, so the commit is `stale_base`. The data the stranded
report (#238) needs is kept: each revision records the mode it was made under, and a shared revision on another branch
with a number lower than the conversion revision is a diverged version of that path.

## Existing data

Existing history is one mainline. A one-time, idempotent startup step (`migrate_branches`, after `migrate_history`)
creates the default branch of every repository pointing at the newest commit, and fills the pointer cache from the
revisions' commits. Repositories created afterwards get their default branch with the repository.
