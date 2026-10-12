# Merging

Merging brings the commits of one [branch](branches.md) (or any commit) into another. It follows git's model, extended only
where exclusive files need it: shared paths are merged by content, exclusive paths take the later revision of their single
line (decision #28), and a mode conversion that met parallel work is a conflict the person decides (decision #195).
Remotes (#237) are separate work. In a git repository `pyn merge` follows git instead, see [Merging on a git home](git-compatibility.md#merging).

## The operation

`merge(source, target)` takes a **source** (a branch name, or a commit id or abbreviation) and a **target** branch (the
default branch when none is named). The target moves; the source does not. The server works out one of four outcomes and
reports it, and a **preview** reports the same without changing anything:

| Outcome | When | What changes |
| --- | --- | --- |
| `up_to_date` | the source is the target's head or one of its ancestors | nothing |
| `fast_forward` | the target's head is an ancestor of the source | the target moves to the source's commit, no new commit |
| `merge` | the histories diverged, or a merge commit was asked for | one new commit with two parents, the target's head first, and the target moves to it |
| `conflicted` | some paths cannot be merged without a decision | nothing (preview only; a merge that is asked for is refused with `merge_conflict`) |

A fast-forward can be refused (`not_fast_forward`) or a merge commit insisted on (a no-fast-forward merge) with the
request's `fast_forward` setting: `allow` (the default), `never` or `only`. A merge into a branch with no commits yet (only
the default branch can be empty) is a fast-forward.

The server is stateless between a preview and the write. The write names the commit the caller saw as the target's head
(`expect_target`); if the target moved since, nothing is written and the answer is `409 merge_stale`. Writes to one
repository are serialized, so the merge is computed against exactly the head it commits on.

## The merge base

The **merge base** of two commits is their lowest common ancestor, taken over every parent: a common ancestor that no
other common ancestor descends from.

- **One base**: the usual case. The three-way merge compares each path in the base, in the target ("ours") and in the
  source ("theirs").
- **No base**: the histories share nothing. The base is empty, so a path both sides added with different content is a
  conflict, and a path one side added is taken. (pyn does not refuse unrelated histories; two branches of one repository are
  always related unless the history was imported.)
- **Several bases** (a criss-cross merge, where two branches merged each other in both directions): the same strategy as
  git's recursive merge. The bases are merged with one another into a **virtual base**, which is never committed: they are
  taken in order of commit id, the first is merged with the second against the bases of those two (recursively), the result
  with the third, and so on. A conflict inside the virtual base is resolved the way git does, with the conflict-marker
  text (or the first base's version for a whole-file conflict), so the real merge sees the disagreement and does not
  silently pick a side. Recursion is capped at 4 levels; deeper than that the virtual base of a pair is empty, which can
  only make the real merge report more conflicts, never fewer. When a conflict is reported against a virtual base, the
  report carries the base's content inline, since no revision holds it.

## Per path

The merge looks at every path whose revision differs between the two heads. A path's mode is the one the policy the merge result has gives it: the later of the two sides'
[effective policies](branches.md#the-effective-policy-of-a-branch) (the higher-numbered policy commit), so merging the
default branch into a branch brings its policy commits, and a merge into the default branch is decided under the policy in
force there. A branch cannot change a path's mode.

### Shared paths

Three-way by content, on the base, ours and theirs versions of the path:

- same content on both sides, or changed on one side only: that side's content;
- changed on both sides, as text: a line-based three-way merge ([`pyn-merge`](../../crates/pyn-merge/src/lib.rs), diff3
  over Myers diffs). Changes whose ranges overlap or touch are merged only when identical, otherwise the region is a
  conflict, as in git;
- changed on both sides and binary (a NUL in the first part of either, a symbolic link, or larger than 8 MiB): a
  **whole-file conflict**; a binary file is never merged;
- one side removed the path and the other edited it: a whole-file conflict. A pyn server does not record deletions yet, so
  this cannot arise today; the rule is in place for when it can;
- a path that is a file on one side and a folder on the other cannot merge (`invalid_request`): resolving it means removing
  one, and a pyn server does not record deletions yet;
- the git file mode (regular, executable, symbolic link) merges the same way, independently of the content; both sides
  changing it differently is a conflict.

### Exclusive paths

An exclusive path has one linear history shared by every branch, so there is no content merge: the merge takes the
**later revision** (the higher-numbered one) of the path (decision #28 and decision #245 item 9: a path's revision at a
commit is the highest-numbered revision in its ancestry, so a merge takes the later pointer by construction). It needs no
lock and writes no revision.

The one exception is a path that was **converted** from shared to exclusive while another branch kept changing it as a
shared path (decision #195 item 4). A version of it that was made under the shared mode and that the other side's history
does not contain is a **diverged version**, whether its number is below the conversion revision or above it (a branch that
had not picked the policy up kept committing the path without a lock). When one side holds a diverged version and the other
a different one, the path is a conflict that is never merged automatically, and it blocks only itself: the rest of the
merge is computed as usual. Two sides with the same content never conflict. It is reported as `exclusive_conversion`, with
both versions and the base, and with **what happened and when** (`conversion`): the conversion revision, the policy
revision in force and who made the change and when, and the diverged version that was stranded with its author and time.
`pyn merge` prints it as one sentence per path. The person chooses the content that becomes the next revision of the path's
single line, which may be either version or something new, and takes the lock as for any exclusive edit (`lock_required`
otherwise; the lock request may name the branch being merged into even though that branch has not picked the policy up yet);
the new revision is based on the newest revision of the path on any branch. Choosing the content the later revision already
has needs no lock and writes nothing.

The same report was given when the conversion happened: see [stranded versions](pyn-toml.md#stranded-versions).

Exclusive-to-shared (decision #195 item 3) never conflicts: the old line is ordinary ancestry, and the path merges as a
shared path from then on.

## What a merge commit holds

- **Parents**: the target's head, then the source's commit.
- **Tree**: the target's tree with every path whose merged content differs from the target's replaced. Trees are plain git
  trees (decision #245 item 9).
- **Revisions**: a path whose merged content equals the later revision's (the usual case: only one side changed it)
  writes no revision; the commit records a **pointer** to that revision, as every commit does ([branches](branches.md#the-revision-of-a-path-at-a-commit)).
  A path whose merged content is new (a text merge, or a resolution) or is not the content of the later revision gets a
  new **merge revision**, appended to the path's line like any checkin and covered by the commit's id and its own hash. A
  shared path's new revision is based on the target's revision; an exclusive path's on the newest of the line.
- **Message**: `Merge branch '<source>' into <target>` unless one is given. The author and committer are the person who
  merged.
- **Policy**: a merge into the default branch that brings a different `.pyn/pyn.toml` is a policy commit: the file must
  parse, the merger needs the `edit_policy` permission, and the new policy applies and converts paths exactly as a direct
  policy commit does (decision #38, [policy](pyn-toml.md)), and the report of the merge lists the
  [stranded versions](pyn-toml.md#stranded-versions) it caused (`stranded`). On any other branch the file is just a file.

## Conflicts and resolution

The conflict state lives in the **client**; the server is stateless and stays the authority.

1. The client asks for a preview: the outcome, the paths the merge changes (each with the content it ends up with, and
   when that is new text the three inputs to merge it), and the conflicts. A conflict names the path, its mode, why it is a
   conflict (`content`, `both_added`, `binary`, `delete_modify`, `file_mode`, `exclusive_conversion`) and the **base**,
   **ours** and **theirs** versions: revision, blob id, file mode.
2. The client writes the working tree: clean changes applied, text conflicts as diff3 markers, binary and other
   whole-file conflicts left as ours. The merge is recorded locally so that `status`, `add`, `commit` and `merge --abort`
   know about it.
3. The person resolves each conflicted path in the working tree and stages it with `pyn add`. Until then the path is
   **unmerged**.
4. `pyn commit` completes the merge: the staged content of each conflicted path is uploaded and sent as the merge's
   **resolutions** together with the commit the merge started from. The server recomputes the merge on those commits,
   requires a resolution for every conflict (`merge_conflict` otherwise, naming the paths), refuses one for a path the
   merge does not touch, applies the lock rules above, and writes the merge commit. The client never supplies the clean
   results, so it cannot smuggle content past the merge; a resolution may also replace a cleanly merged path's result, as a
   checkin could.
5. `pyn merge --abort` puts the files the merge touched back to the target's revisions and forgets the merge. Nothing was
   written to the server, so there is nothing to undo there.

## API

| Request | Returns |
| --- | --- |
| `POST /v1/repos/{owner}/{name}/merge/preview` `{source, target?, fast_forward?}` | the merge report, changes nothing; needs read access |
| `POST /v1/repos/{owner}/{name}/merge` `{source, target?, fast_forward?, message?, expect_target?, resolutions?}` | the merge report with the resulting `commit`; needs the `checkin` permission |

Errors: `merge_conflict` (409, unresolved conflicts, paths named), `merge_stale` (409, the target moved),
`not_fast_forward` (409, a fast-forward only merge that cannot fast-forward), `branch_not_found`, `commit_not_found`,
`lock_required` and `stale_base` for a resolution on an exclusive path, `forbidden` and `invalid_rules` for a policy file.
A merge is audited as `branch_merged`, and each new revision as a checkin.

## Command line

`pyn merge <branch>` merges the branch into the workspace's branch, with git's flags (decision #218): `-m <message>`,
`--no-ff`, `--ff-only`, `--no-commit` (stop before committing, leaving the merge in progress) and `--abort`. `--preview` is
pyn's own: it prints what would merge and what would conflict and changes nothing. See [the workspace](workspace.md#merging)
for what the workspace shows.
