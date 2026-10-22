# Commits, snapshots and diff

A **commit** is one atomic write to a repository: a single `pyn commit`, or a whole [changeset](README.md#exclusive-files-and-locks).
Commits are git commits (decision #245): each has git's commit id, the SHA-256 hash of the commit object, and is named by
that hash or an abbreviation of it. A rejected write makes no commit. A commit is made on a [branch](branches.md) and has the
branch's head as its parent; a [merge commit](merging.md) has two, the target's head first.

## The commit record

A commit carries what a git commit carries:

- the **author** and **committer**: name, email, time and timezone offset. The server records the signed-in account's
  name and email (an account without an email gets `<name>@pyn.invalid`), the time of the write, and offset `+0000`;
  the author and committer are the same person. The author's time and offset and the committer's are separate in the
  record (`authored_at` with `author.offset_minutes`, and `created_at`, the committer time), so a pushed or imported commit
  whose author and committer times differ keeps both;
- the **parents**: none for the first commit of a repository, else the head of the branch it was made on;
- the **message**;
- room for a **signature** (a `gpgsig` header), which signed commits (#60) will fill;
- the **tree**: a git tree of every file in the repository as of this commit, with nested trees and per-entry git file
  modes (`100644`, `100755`, `120000`) and blob ids. The tree is a plain git tree, built with `pyn-object`.

The commit id is the git commit id of exactly those fields, so the encoding is git's and the id would be the same in a git
repository of the same object format (SHA-256 for a pyn server). The id covers the tree and the parent, so changing any
file, mode or earlier commit changes every later id, and a [revision's v2 hash](integrity.md) covers its commit id.

New commits and tags are always written canonically. Objects read from a git repository can be non-canonical (a `-0000`
timezone, headers in another order, uppercase hex ids); `pyn-object` reads those leniently and keeps the original bytes,
so the object keeps the id it was stored under.
The exact commit bytes are stored with each commit and the id is their hash; the fields above are what those bytes say.
A server-created commit's bytes are the encoding of its fields, so every id is unchanged, and the bytes are what lets a
commit built elsewhere keep its id (provisional: no path ingests such a commit yet). Commits that predate the stored
bytes were re-encoded from their fields by a migration.
Existing history was migrated once into a single mainline, now the default branch; see [integrity](integrity.md#existing-data).

## The tree of a commit

The tree of a commit is stored with it, built from its parent's tree by replacing the entries the commit wrote, so only
the folders on the changed paths are rewritten. A repository's state as of a commit is read from the revisions:

- a path's **revision at a commit** is its highest-numbered revision whose commit is that commit or an ancestor of it
  (decision #245 item 9). The server caches it per commit and reads it by walking the commit's first-parent chain; see
  [branches](branches.md#the-revision-of-a-path-at-a-commit);
- a path's **mode** (`shared` or `exclusive`) is what `.pyn/pyn.toml` says at that commit: the policy file in the
  commit's own snapshot, or the server's fallback rules while the repository has none;
- an exclusive path is not special in the tree: its entry is the blob of the revision it has at that commit, like any
  file. The revision number is not stored in the tree, so trees stay plain git trees and rolling back or merging
  needs no pointer handling.

Each revision also records the git file mode of its content, so a snapshot shows a path as it was when it changed. Asking
for an old commit reads old revisions and never changes any file's history.

## API

| Request | Returns |
| --- | --- |
| `GET /v1/repos/{owner}/{name}/commits?branch=&before=&limit=&author=&since=&until=&grep=&path=&from=&to=` | the commits a branch contains (the default branch's if omitted), newest first (default 50, max 200), each with its revisions; `next_before` pages back; the filters below narrow it |
| `GET /v1/repos/{owner}/{name}/commits/{commit}` | one commit; `404 commit_not_found` for an id the repository does not have |
| `GET /v1/repos/{owner}/{name}/snapshot?commit=&branch=&after=&limit=` | the snapshot of a commit, or of a branch's head (the default branch's if neither is given), ordered by path, 200 per page (max 1000); `commit` is null for an empty repository |

A commit is given as its full id or as an abbreviation of at least 4 lowercase hex digits; an abbreviation that matches
more than one commit is `400 ambiguous_commit`. `HEAD~N` and `^` are resolved by clients, which walk `parents`.

### Filtering commits

The listing takes filters that the server evaluates before it pages, so a page is full whenever more matches exist and
`next_before` is the last commit of the page. They combine with each other (all must hold) and with `before` and `limit`.
`/history` is unchanged: it stays the per-path revision view.

| Parameter | Keeps the commits |
| --- | --- |
| `author` | whose author name or author email contains the text, ignoring case (the committer is not consulted) |
| `since`, `until` | authored at or after, or at or before, an RFC 3339 instant (`2026-10-08T09:00:00Z`, any offset); both bounds are inclusive and compare the author date |
| `grep` | whose message contains the text, ignoring case |
| `path` | that made a revision of this path, or of any path under it when it is a folder (`Source` matches `Source/a.cpp`, not `Sources/a.cpp`); a deletion counts, and a merge counts only for the revisions it wrote |
| `from`, `to` | reachable from `from` and not from `to`, like git's `to..from`: both are commits (an id or an abbreviation of at least 4 hex digits), `to` itself is left out, and every parent of a merge counts as reachable. `from` replaces the branch's tip and is refused together with `branch`; `to` alone ranges from the branch's tip |

A bad timestamp or path, an unknown or ambiguous `from` or `to`, or `from` with `branch` is `400` (`invalid_request`, or
`invalid_path` for a malformed `path`). `before` keeps its `404 commit_not_found`. The memory and Postgres stores answer the
same filters through `MetadataStore::commits_matching`.

File content at any revision is fetched with `GET .../content?path=&revision=`, so a snapshot entry's `revision` and `content` are
enough to fetch every file. Every revision in the API carries the `commit` it belongs to, its `file_mode` and, for revisions
made since the commit record landed, the structured `author_identity` its hash covers. A checkin or changeset entry takes an
optional `file_mode` (`100644` by default, `100755` or `120000`; a symbolic link's content is its target path). Reading is allowed to whoever may read the repository.

A changeset can also carry `deletions` (`path` and `base_revision`, the revision being removed; see [deleting a path](README.md#deleting-a-path)). A deletion is a revision with `deleted: true`
and `hash_version` 3 in the commit that removes the path from the commit's tree (a folder left empty goes with it, git having no empty folders), and its `content` and `file_mode` are those of the version removed. Snapshots,
files and trees leave deleted paths out; history and the commit's `revisions` list them. The response lists the entries' revisions in request order, then the deletions'.

## Pushed commits (provisional)

Decision #300 lets a client build commits itself and publish them. The core accepts them through `RepoService::push_commits` and
one atomic store step, `MetadataStore::push_commits_on`, and the server offers them as `POST /v1/repos/{owner}/{name}/push`
(below). The matching read side is `POST .../fetch` ([fetching](#the-fetch-route)). `pyn push`, `pyn fetch` and `pyn pull` use
them in a workspace with local commits, the default ([from the command line](#fetch-pull-and-push-with-a-local-store)); everything here
is provisional.

### The push route

`POST /v1/repos/{owner}/{name}/push` needs the `checkin` permission. The body is JSON:
`{branch?, commits: [{id, data}], trees: [{id, data}]}`. `data` is the base64 of an object's body, the bytes after git's
`<kind> <length>` header, and `id` is the SHA-256 git id it must hash to. Commits go oldest first; `trees` holds the trees the
commits need that the server may not have. Blobs are not in the body: they are uploaded first through the chunked
[object routes](storage.md), which is why a missing blob is `object_missing`. The caller is authenticated and authorized before the body is read, so a request without the permission is refused without its body being consumed. The body is at most 64 MiB (`object_too_large`, 413, beyond it); malformed JSON is `invalid_request`. The answer is
`{branch, head, revisions, stranded?, released_locks?}`: the branch's head after the push, the revisions the commits made, in commit order, and, when a policy commit in the push stranded shared versions, the same `stranded` list a direct policy commit reports; `released_locks` lists the live locks (path, owner, expiry) such a policy commit released because their paths became shared.

The route is additive: `changesets` and `checkin` remain the direct route (decision #300), and no existing field changed.

`GET /v1/capabilities` needs no credentials and answers `{local_commits: true}` on a server with these routes. A client checks it before it keeps commits locally (`pyn clone`, `pyn migrate`); a server without the route (404) or with `local_commits` false is too old, and the client refuses with a message to upgrade or opt out. The direct routes stay for the web app and for use without a workspace.

Errors, as `{code, message}`: `not_fast_forward` (409), `lock_required` and `lock_held` (409), `stale_base` (409),
`storage_limit_reached` (409), `push_invalid` (400, an id that does not hash, bytes that are not a git object, a missing tree, a merge of a commit the
repository lacks, or a commit already stored), `merge_conflict` (409, a merge commit that leaves a diverged exclusive path unresolved), `object_missing` (400), `invalid_request` (400, an empty push or bad base64), `invalid_rules` (400, a policy file on the default branch that does not validate), `push_unsupported` (422), `push_too_large` (413, a commit changing more than 10000 files or a push more than 50000, see [limits](limits.md#request-size-bounds)),
`branch_not_found` and `commit_not_found` (404), `forbidden` (403, also for a policy change without the `edit_policy` permission). No offline lock guarantee exists: a lock is judged when the
push arrives.

A push is audited like the checkins it makes: one `checkin` entry per revision, with the authenticated pusher as the actor and
`(push)` in the detail. The commit's author is recorded on the revision as its person, not as the actor.

### The fetch route

`POST /v1/repos/{owner}/{name}/fetch` lists what a client lacks, so it can keep commits and trees locally (decision #300).
It follows the repository's read access like `objects/{id}`: a public repository needs no credentials, and a private one is
`404` to anyone without a role. The body is JSON, every field optional:
`{branches?, haves?, cursor?, limit?}`.

- `branches` are the branches wanted; all of them when empty. An unknown one is `branch_not_found` (404).
- `haves` are commits the client already has, normally its branch heads. A commit is missing when it is reachable from a wanted
  branch and from no have. A have the repository does not know is ignored, as in git; a malformed id is `invalid_request`.
  At most 1000 haves.
- `limit` is the number of commits per page: 100 by default, at most 500.
- `cursor` is the opaque `next` of the previous page.

The answer is `{refs, commits, next?}`. `refs` lists each wanted branch with its head (null for a branch with no commit).
`commits` holds the page, parents before children, in the order the server stored them: each is
`{id, data, pointers, trees}`. `data` is the base64 of the commit's body and `trees` are `{id, data}` objects, both encoded as in
a push, so a client stores them under the ids it is given. `pointers` are the revisions the commit's pointer cache records
([branches](branches.md#the-revision-of-a-path-at-a-commit)): the revisions the commit wrote and, for a merge commit, the ones it adopted from the other parent. Each carries its `mode`, so a client
keeps the exclusive pointers it needs for a later push. The trees of a commit are those under its root that its first parent's
tree does not hold at the same place; a commit without a parent sends its whole tree. A client that has the first parent's trees
therefore has every tree it needs, and a tree may be sent again by a later commit that restores it. Blobs are not sent: they are
read on demand with `objects/{id}`.

A cursor pins the branch heads the first page saw, so a branch that moves between pages changes neither the walk nor the `refs`
of later pages; the client sends the same `haves` again, and a cursor with other haves or branches is `invalid_request`. A page continues after the last
commit the previous one sent, so its work follows the commits still to send rather than the pages already served. One request examines
at most 1,000,000 commits; a larger walk is `invalid_request`.

The route is additive and nothing existing changed. Provisional: the page limits and the examination cap may move.

### Reading a blob by id

`GET /v1/repos/{owner}/{name}/objects/{id}` returns a blob's bytes when a revision of that repository has it (`object_not_found`,
404, otherwise, including for a blob only another repository holds). It honours a single `Range: bytes=` header (`206`, or `416`
with `Content-Range: bytes */<length>`), sends the id in `x-pyn-content`, and follows the repository's read access (a public
repository is readable without credentials). A client that keeps commits and trees locally uses it to fetch blobs on demand.

- A push is a chain of commits, oldest first, plus the trees they need. Commits keep their ids: each is checked to hash to the id
  it is sent under, to be a SHA-256 git commit, and to be a child of the one before it.
- It must fast-forward: the first commit's parent is the branch's head or one of its ancestors (a first commit with no parent is
  for an empty branch), and the pushed history must contain the head, which only a [merge commit](#merge-commits-in-a-push-provisional)
  can arrange when the first parent is not the head; otherwise `not_fast_forward`. A commit already in the repository is
  `push_invalid`. The branch must exist.
- Each commit's tree is compared with its parent's: every added or changed file becomes one revision, with the commit's author as its
  person, the pusher as its account, the commit's time and message, and the path's mode under the branch's policy (an unlisted path
  is exclusive by default). The blob of each must already be in the object store (`object_missing`), and a tree the commit needs must
  be in the push or already stored (`push_invalid`).
- An exclusive path needs the pusher's live lock at the moment of the push (`lock_required`, or `lock_held` when someone else has
  it); a lapsed lease is refused. The lock is used up with the path's last revision in the push, so a push may carry several
  commits that edit it. Each revision is based on the branch's pointer for the path as the commits before it leave it
  ([branches](branches.md)), not on the newest revision of another branch.
- The whole push is accepted or refused as one: a refusal at any commit leaves no commit, revision, tree, lock change or branch
  move behind. Concurrent pushes to a branch are serialized, so one wins and the others get `not_fast_forward`.
- A commit whose tree lacks a path its parent had **deletes** it, a whole folder included, and a file turned into a folder or back
  deletes the old and adds the new. Each deletion is recorded as the changeset API records it ([deleting a path](README.md#deleting-a-path)):
  a revision with `deleted: true`, based on the branch's pointer for the path, with the content and mode of the version removed.
  An exclusive path needs the pusher's live lock, consumed like a write's (`lock_required`, `lock_held`); the audit log has a
  `delete` entry per deletion.
- A commit that changes `.pyn/pyn.toml` on the **default branch** is a policy commit, validated like a direct one
  ([policy](pyn-toml.md)): the pusher needs the `edit_policy` permission, and the file must parse and validate, or the whole push is
  refused with `invalid_rules` and nothing is recorded. Once the push is recorded the rules apply and conversions, lock releases and
  the stranded-version report run as for a direct policy commit, with the `stranded` list in the answer. The first policy file of a
  repository is the owner's bootstrap and needs no lock; later ones follow the policy file's own mode. The rules a policy commit
  brings apply to the commits after it in the same push, not to its own other files. Deleting the policy file from the default branch
  is `push_unsupported`. On any other branch the policy file is an ordinary file, not parsed, until a merge brings it to the
  default branch (decision #38). Provisional: several policy commits in one push are applied one after the other against the
  final head.
- Refused with `push_unsupported`, for now: a commit with more than two parents or the same parent twice; a commit with one
  parent that changes no file; a submodule entry; the policy file's removal from the default branch; and a message without its
  final newline.
  A name or message that is not UTF-8 is accepted: the exact bytes are stored and the commit keeps its id, and the name,
  email and message fields in the API are those bytes decoded lossily (invalid bytes and NUL become U+FFFD).
- The pusher needs the `checkin` permission. A commit's author need not match the pusher, as in git; the authenticated pusher is
  the account on the revisions and the actor in the audit log. A strict-match policy may be added later.

### Merge commits in a push (provisional)

A commit with two parents is accepted (decision #300, issue #338) and judged as the server's own [merge](merging.md) judges it,
so a rejected push is resolved by pulling, merging and pushing the merge. The first parent is the previous commit of the push
(or, for the first one, an existing commit that is the branch's head or an ancestor of it); the second parent must already be
in the repository, otherwise `push_invalid`. The push as a whole must contain the branch's head, or `not_fast_forward`.
Merging commits that exist only in the same push is not supported: push that history first.

The content of a merge is what its tree records. For each path the two parents' pointers differ on (or the merge tree changes),
the pointer is worked out from the pointers at the first parent (as the commits before it in the push leave them) and at the
second parent:

- **Shared** paths: the later of the two pointers wins when the tree has its content, and is adopted without a new revision. Any
  other content in the tree (a text merge, a resolution, a removal) is a new revision based on the first parent's pointer,
  appended like a checkin; the server does not recompute the text merge, it takes the pushed tree, as it takes a resolution.
- **Exclusive** paths: a pointer that descends from the other (through the recorded bases, or because its commit has the other
  in its history) is adopted by a tree that holds its content, with no lock and no new revision. Pointers that **diverged**
  need a new revision under the pusher's live lease: the tree's content for the path becomes a revision based on the first
  parent's pointer, and the usual lock rules apply (`lock_required`, `lock_held`, a lapsed lease refused). A diverged path whose
  tree keeps the first parent's content is accepted as an explicit choice only if the pusher holds the path's lock; otherwise the
  push is refused whole with `merge_conflict`, naming the paths. A shared version that met a conversion is diverged in the same
  way.
- A content the tree gives a path that is neither pointer's is a new revision, so an exclusive path needs the lock then too.
  A path the tree removes is a deletion revision.
- A policy file the merge adopts on the default branch is checked and applied like an arriving policy (`edit_policy` permission,
  `invalid_rules`), after the push is recorded.

The push is still all or nothing, and the commits keep their ids. A merge is audited as `branch_merged` (detail `(push)`), and
each new revision as a checkin. The wire is unchanged: the route accepts what it refused before, and the request and answer
have no new field.

### Fetch, pull and push with a local store

In a workspace with local commits, the default ([workspace](workspace.md#the-local-store)), the three commands use these routes
(decision #300); in a workspace cloned with `--no-local-store` they are unchanged (`fetch` and `pull` run `pyn update`, `push`
says there is nothing to push).

- `pyn fetch` sends the remote-tracking tip as the only have and stores the commits and trees of each page. Every commit and
  tree body is hashed and must equal the id it came with, and a commit's tree and parents must already be stored: on any
  mismatch the whole answer is refused (`... does not match its content`) and the remote-tracking ref does not move. It then
  moves `refs/remotes/origin/<branch>`; the local branch and the files are left alone. Output is git's:
  `From <server>/<repo>` and `   <old>..<new>  main -> origin/main`, nothing when there is nothing new. Pointers are not kept
  yet. A blob the store lacks is read on demand by id (`GET .../objects/{id}`), checked against the id and stored.
- `pyn pull` is `fetch` and then: nothing when the local branch already holds the fetched tip (`Already up to date.`); a
  fast-forward when the fetched tip descends from the local one, followed by the usual `update` of the files; otherwise a
  **merge commit** with the local tip and the fetched tip as parents (`Merge branch 'main' of <server>/<repo>`, ` into <branch>`
  unless the branch is `main` or `master`; `Merge made by the 'ort' strategy.`), as git does. The merge base is the lowest common
  ancestor (several of them are refused), and each path is merged like the server merges: a side that alone changed it wins, a
  shared file both sides changed is merged line by line, and a file both sides changed that cannot be merged that way, a mode
  change on both sides, or a deletion against an edit is a conflict. An exclusive path both sides changed has no automatic pick:
  the pull stops unless the person holds its lock, in which case their version is kept (`Kept your version of <path>`) and the
  server accepts it at push as the new revision. Any other conflict stops the pull **in progress**, as `pyn merge` does on a
  server ([conflicts in a pull](#conflicts-in-a-pull)). The files the merge changes are written, and `update` brings the rest.
  `pyn pull --rebase` replays the local commits one by one onto the fetched tip with the same per-file rules, stopping at
  the first commit that conflicts; a local merge commit cannot be replayed and is refused. A pull also refuses to
  run over uncommitted changes to a file the incoming commits change, and with staged changes. `--rebase=false` and
  `--no-rebase` mean merge; `--abort`, `--continue` and `--skip` conclude a pull in progress; other options are usage errors.
- `pyn push` first uploads every blob the unpublished commits added or changed through the chunked object routes (not the ones
  a merge took from the fetched side), then sends the commits and the trees they need in one `POST .../push`, merge commits
  included. The local history must contain the fetched tip, else `non-fast-forward`; a merge may only merge in commits the
  server already has. It is accepted or refused whole. On success the
  remote-tracking ref moves to the new head, so the commits are no longer unpublished; the files take the revisions the server
  made, and exclusive files go read-only again (a deleted path leaves the workspace's state). A commit that deletes a path or
  changes `.pyn/pyn.toml` is sent like any other ([deletions and policy commits](#pushed-commits-provisional)); when the
  server adds conversion commits after a policy commit, the next `fetch` brings them in, and the stranded versions it reports
  are printed after the push as for a direct policy commit, followed by a line per lock the policy released (`released bob's lock on <path>`). Output: `To <server>/<repo>` and `   <old>..<new>  main -> main`, or
  `Everything up-to-date`.
- A refused push prints a git-style `! [rejected]` line, the server's message, a hint and exits `1`:
  `not_fast_forward` and `stale_base` (also detected locally when the local branch does not build on the fetched tip) tell the
  person to run `pyn pull` and push again; `lock_required` (including a lapsed lease) says to lock the path; `lock_held`
  names that someone else holds it; `invalid_rules` (an invalid policy file, nothing pushed) and `forbidden` (also a policy
  change without the `edit_policy` permission) are named; so are `object_missing`, `push_invalid`, `push_unsupported`,
  `storage_limit_reached` and `merge_conflict` (a merge commit leaves a diverged exclusive path unresolved: lock it and
  pull again).

### Conflicts in a pull

The server is not involved: the client makes the merge, so a conflicted pull is recorded in the workspace, in the same
`.pyn/local_only/state/merge.toml` as a `pyn merge` ([the workspace](workspace.md#merging)), with the commits it joins and the
tree the clean paths gave. Only unpublished local commits are ever replayed; published history is never rewritten.

- **Merge.** A shared file both sides changed with overlapping lines gets diff3 markers (`<<<<<<< HEAD`, `||||||| base`,
  `=======`, `>>>>>>> origin/<branch>`). A binary file or one whose mode both sides changed keeps the local version, and a path
  removed on one side and changed on the other keeps the side that exists. An exclusive path both sides changed keeps the local
  version. Every clean change is in the working tree and the branch has not moved. `pyn status` says `You have unmerged paths`
  and `unmerged (...)` in its `STATE` column (`UU`, or `AA` for both added, in `--short`).
- **Resolving.** Edit the file, or take a side with `pyn checkout --ours|--theirs -- <path>`, then `pyn add <path>`; a file
  deleted in the working tree and staged with `pyn add` resolves a path by removing it. `pyn commit` (or `pyn pull --continue`)
  makes the merge commit, with the local tip and the fetched tip as parents, once every conflicted path is staged; a path that is
  not part of the merge cannot be staged meanwhile. A resolved exclusive path needs the person's live lock to be concluded
  (`pyn lock <path>`; the message names it); nothing is committed without it. `pyn fetch`, `pull`, `push`, `reset`, `switch` and
  `update` are refused while it is in progress.
- **Rebase.** The same record holds the replay: the commits done, the one being applied and those still to come. A conflict
  stops it at that commit, with its clean changes and the markers in the working tree, `--ours` being the fetched side and
  `--theirs` the commit being applied. `pyn commit` or `pyn pull --continue` records the resolved commit (with its author and
  message, unless `-m` is given) and replays the next ones, stopping again at the next conflict; a commit that becomes empty is
  dropped. `pyn pull --skip` drops the commit being applied. The local branch moves only when the whole replay is done.
- **Abort.** `pyn pull --abort` (or `pyn merge --abort`) puts the files the pull touched back to the local tip, forgets the
  record and leaves the branch, the local commits and the store as they were; a change in the working tree to any other file is
  never touched.

### Cherry-pick

`pyn cherry-pick <commit>...` applies the change each named commit made as a new local commit on the workspace's branch, with
the commit's author and message and the person as committer. A commit is a hash or abbreviation, `HEAD~N`, or a branch name of
the local store, and `<a>..<b>` and `<a>...<b>` name ranges, applied oldest first; the commits may be on another branch or
already published. Nothing is sent: the result is an unpublished commit like any other, concluded by `pyn push`.

- **The merge.** Each path is decided like a rebase step ([conflicts in a pull](#conflicts-in-a-pull)) with the commit's parent as
  the base, the branch as `HEAD` and the picked commit as the other side: a side that alone changed a path wins, a shared file
  both changed is merged line by line, and a binary file, a mode change on both sides or a deletion against an edit is a
  conflict. An exclusive path only the picked commit changed is taken (the server asks for the lock at push, as for any local
  commit). An exclusive path both sides changed is always a conflict, even when the person holds its lock: they choose a side
  with `pyn checkout --ours|--theirs`, take the lock with `pyn lock <path>`, stage it and conclude. A commit that changes nothing
  here is dropped and said so.
- **Stopping.** A conflict stops the run with the clean changes and the markers in the working tree, recorded in the same
  `.pyn/local_only/state/merge.toml` as a conflicted pull; the branch moves only when the whole run is done. `pyn cherry-pick
  --continue` (or `pyn commit`) records the resolved commit and goes on, `--skip` drops it and goes on, and `--abort` (or
  `pyn merge --abort`) puts the files back and leaves the branch as it was. `pyn status` says `You are cherry-picking`, and
  `pull`, `reset`, `switch`, `update` and a second `cherry-pick` are refused while it is in progress.
- **-x.** Adds the line `(cherry picked from commit <id>)` to each message, as git does.
- **Refused.** `-n`, `-s`, `-m` and `--quit` need a git repository (`--abort` is the way out); a merge commit cannot be picked,
  only the commits it merged; staged changes, uncommitted edits to a file the commits change, and a workspace without a
  fetched branch stop the command before anything changes.

### Revert

`pyn revert <commit>...` undoes the change each named commit made with a new local commit on the workspace's branch. The
message is `Revert "<subject>"` (`Reapply "<inner>"` when the subject is itself a revert) with `This reverts commit <id>.`, the
person is author and committer, and nothing is sent: the result is an unpublished commit, concluded by `pyn push`. Commits and
ranges are named as for [cherry-pick](#cherry-pick); a range is reverted newest first.

- **The merge.** The same rules as cherry-pick with the roles turned round: the commit is the base, the branch is `HEAD`, and
  the commit's parent is the other side. A shared file the branch changed since is merged line by line, a binary file, a mode
  change on both sides or a deletion against an edit is a conflict. An exclusive path only the reverted commit changed goes
  back to its earlier revision; one both the commit and the branch changed is always a conflict, even when the person holds its
  lock, resolved with `pyn checkout --ours|--theirs`, `pyn lock <path>`, `pyn add` and `pyn revert --continue`. A commit
  already undone is dropped and said so.
- **Stopping.** As cherry-pick: `pyn revert --continue` (or `pyn commit`), `--skip` and `--abort` (or `pyn merge --abort`),
  `pyn status` says `You are reverting`, and `pull`, `reset`, `switch`, `update`, `cherry-pick` and a second `revert` are refused
  while it is in progress.
- **Refused.** `-n`, `-s`, `-m` and `--quit` need a git repository; a merge commit cannot be reverted, only the commits it
  merged; staged changes, uncommitted edits to a file the commits change, and a workspace without a fetched branch stop the
  command before anything changes.

### Rebase

`pyn rebase [-i] [--autosquash] [-x <cmd>] [--no-ff] [--autostash] [--onto <newbase>] [<upstream> [<branch>]]` rewrites the
**unpublished** local commits of the workspace's branch: with `-i` they are reordered, reworded, edited, squashed, fixed up or
dropped before `pyn push`. `<upstream>` defaults to the
fetched tip, so the commits replayed are exactly the unpublished ones; a hash, `HEAD~N` or a branch name of the local store may
name a later commit to rewrite only the newest ones. Published history is never rewritten.

- **The sheet.** `-i` opens the editor (`GIT_SEQUENCE_EDITOR`, then `GIT_EDITOR`, `VISUAL`, `EDITOR`) on git's instruction
  sheet and reads `pick`, `reword`, `edit`, `squash`, `fixup`, `drop`, `exec`, `break` (and `noop`) lines, in any order; a line left out loses that
  commit. `reword` and the end of a `squash` chain open the editor on the message (`GIT_EDITOR`); a chain of only `fixup` keeps
  the first message. `--autosquash` moves `fixup!` and `squash!` commits under the commit they name, with or without `-i` (`amend!`
  commits stay where they are, and `fixup -C` and `-c` are refused). A
  commit that is not changed and still follows the same parent is reused as it is. Without `-i` and `--autosquash` the command
  says `Current branch <b> is up to date.`, since the replay would change nothing.
- **Published commits.** A range that includes a published commit (an `<upstream>` older than the fetched tip, or a sheet line
  naming one) is refused, naming the commit, before anything changes. So is a new base that does not contain the fetched tip,
  and a merge commit. `--rebase-merges`, `--root`, `--fork-point`, `--update-refs` and the sheet commands `label`, `reset`, `merge`
  and `update-ref` are refused as `not available in a workspace`.
- **Stopping on purpose.** `edit` replays the commit and stops with it made; `break` stops where it stands; `exec <cmd>` (and
  `-x`/`--exec`, which adds the line after every commit) runs `<cmd>` with `sh -c` in the workspace root, and a command that fails,
  or leaves uncommitted changes, stops the rebase. At such a stop the working tree holds the files of the commit reached and the
  branch points at it, so `pyn status` says `You are rebasing (stopped at edit)`, and `pyn add` and `pyn commit` add commits to
  the branch as usual (there is no `--amend`). `pyn rebase --continue` replays what is left after the commits made since; it
  refuses staged or uncommitted changes. `--skip` puts uncommitted changes back and goes on, `--abort` returns the branch and
  the files to where they were before the rebase, and `--quit` forgets the rebase and leaves the branch where the replay had
  got it. `--edit-todo` opens the editor on the commands still to do, at any stop, and applies the same checks as the first
  sheet. An `edit` whose commit conflicts stops for the conflict first, then again once it is resolved.
- **Autostash.** `--autostash` puts the uncommitted changes of tracked files aside before the rebase and applies them over
  the result when it ends (or is aborted or quit), merging them with what the rebase changed; a file that does not merge keeps
  both sides with conflict markers and the command says so. Staged changes are still refused. The changes are kept in the local
  store and in the rebase record, so a stop does not lose them.
- **`--no-ff` and `<branch>`.** `--no-ff` replays commits even when they already follow their parent, which makes new commits
  with a new committer time. `<branch>` is `pyn switch <branch>` first, so it is refused while the current branch has unpublished
  commits.
- **The merge.** Each commit is applied like a [cherry-pick](#cherry-pick) over the commits made so far, with the same per-file
  rules: an exclusive path only the replayed commit changed is taken, and one both sides changed (the commit and the commits
  before it in the new order) is always a conflict, even when the person holds its lock; it is resolved with `pyn checkout
  --ours|--theirs`, `pyn lock <path>`, `pyn add` and `pyn rebase --continue`. A commit that changes nothing in the new order is
  dropped and said so.
- **Stopping.** A conflict stops the run in the same `.pyn/local_only/state/merge.toml` as a conflicted pull; the branch moves
  only when the whole run is done. `pyn rebase --continue` (or `pyn commit`) records the resolved commit (a squashed one with
  the combined message) and goes on, `--skip` drops it, and `--abort` (or `pyn merge --abort`) puts the files back and leaves the
  branch as it was. `pyn status` says `You are rebasing`, and `pull`, `reset`, `switch`, `update`, `cherry-pick`, `revert` and a
  second `rebase` are refused while it is in progress.
- **Not built.** Staged changes, uncommitted edits to a file the commits change (without `--autostash`), and a workspace
  without a fetched branch stop the command before anything changes. `pyn pull --rebase` ([above](#fetch-pull-and-push-with-a-local-store)) is the replay onto newly fetched commits.

## Command line

The CLI follows git where it has an equivalent; the differences are listed below.

- `pyn log --commits [--limit n]` lists commits (`COMMIT`, `AUTHOR`, `WHEN`, `FILES`, `MESSAGE`); `COMMIT` is the abbreviated hash. `pyn log` keeps listing revisions.
- A commit is written as its hash or an abbreviation of at least 4 hex digits (`pyn log --commits` shows 10), `HEAD` (the
  head of the workspace's [branch](branches.md)), or either followed by `~N` (N commits back along the first parent) or `^` (the
  parent; `^^` is two back). Only the first parent is followed, so `^2` names no commit. `cN` numbers no longer exist. An argument that is also the name of something in the current
  directory is read as a path by `pyn diff`.
- `pyn clone --revision <commit> <source> [dir]` creates a workspace with every file as of that commit.
- `pyn status --revision <commit>` compares the workspace with that commit instead of the newest: `behind (<commit> has rM)` for a
  file whose revision there is newer, `ahead (<commit> has rM)` or `ahead (not in <commit>)` for one that is newer than, or missing from, it.
- `pyn update --revision <commit>` brings the workspace to that commit, older or newer: files are fetched at the commit's
  revision and files it does not have are removed. A file with local changes is never touched and is reported as skipped.
- `pyn commit` takes `-m`, `-F`, `-a` and, so scripts written for `git commit` run unchanged, `-q`/`--quiet` (no summary; errors and
  warnings still print), `-n`/`--no-verify`, `--verify`, `-v`/`--verbose`, `--status`, `--no-status` and `--no-edit`, the last six
  accepted and ignored because pyn runs no hooks and opens no editor. `--author=<Name <email>>` and `--date=<date>` (git's formats) are
  taken on a local-store commit of staged changes: the client builds the commit, so they set the author and the author date,
  while the committer stays the signed-in account and the audit actor is the pusher. They survive `pyn push` and show in
  `pyn log --commits`, `log --format=%an` and `show`. A workspace without the local store, a path commit and a merge
  conclusion refuse them. The rest of git's flags (`-s`, `--amend`, `--allow-empty`,
  `--allow-empty-message`, `--cleanup`, `--reset-author`, `--trailer`, `-C`, ...) are a usage error (`129`) naming the flag;
  [the full list](git-compatibility.md#commit-flags-on-a-git-home) says which a git repository takes.
- `pyn show [<commit>]` prints `commit <full hash>`, `Author: <name> <<email>>`, `Date:` and the message, then what the commit changed against its parent (the newest if none is named).
- `pyn diff` prints a [unified diff](#diff-output).

### `pyn diff`

| Form | Compares |
| --- | --- |
| `pyn diff [-- <path>...]` | the workspace with the revisions it was last updated to (tracked files only) |
| `pyn diff <commit> [-- <path>...]` | the workspace with that commit |
| `pyn diff <commit> <commit>` or `<commit>..<commit>` | two commits; works outside a workspace |

Arguments before `--` that are commits (`HEAD`, `HEAD~N`, `HEAD^`, a hash) are taken as such, anything after is a path, relative to the
current directory inside a workspace. Options: `-U<n>` / `--unified=<n>` lines of context (default 3), `--stat`, `--name-only`,
`--name-status` (`A`, `M`, `D` and a tab), `--exit-code` (exit 1 when there are differences, 0 when none) and `--quiet` /
`-q` (no output, exit 1 when there are differences). Without `--exit-code` a diff exits 0, as in git.

#### Diff output

Output is the unified format git prints: `diff --git a/<path> b/<path>`, `new file mode <mode>`, `deleted file mode <mode>` or `old mode`/`new mode`, an `index
<old>..<new>` line with seven characters of each blob id, `--- a/<path>` and `+++ b/<path>` (`/dev/null` for a created or
deleted file), then `@@ -a,b +c,d @@` hunks (followed, as in git, by the nearest earlier line that starts with a letter, `_` or `$`), with `\ No newline at end of file` where it applies. A file with a NUL byte in its
first 8000 bytes is binary and prints `Binary files a/<path> and b/<path> differ` (`Bin <old> -> <new> bytes` in `--stat`); only that first block of it is fetched for a patch.
The diff is Myers' algorithm, built in with no dependency on git (decision #30), computed in the CLI. For inputs with more than
2000 changed lines the changed region is shown as one replacement instead of a minimal diff, to bound memory.

#### Differences from git

- `pyn diff` compares the work tree with the staged content, as git does (the last update where nothing is staged), and `--cached` or
  `--staged` compares the staged content with the last update; see [staging](workspace.md#staging).
- File modes are tracked: created and deleted files print their git mode, and a changed mode prints `old mode` and `new mode`.
- `status --revision`, `update --revision` and `log --commits` have no git equivalent; they are pyn's way to reach a past commit of the
  workspace's branch. Another line of work is a [branch](branches.md): `pyn switch`.

