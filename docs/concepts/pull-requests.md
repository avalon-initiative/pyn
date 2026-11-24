# Pull requests

A **pull request** (a change request) proposes merging one [branch](branches.md) into another and holds the discussion
about it. It lives in the repository next to branches and [merges](merging.md), under the repository's roles; a self-hosted
server has it with no extra setup (decisions #680, #97). Provisional: reviews, approvals and required checks are not built yet
(see [what is not here yet](#not-here-yet)).

## The record

A pull request has a number per repository (1, 2, ...), a `title` (one line, up to 256 characters), a `body` (Markdown, up to
64 KiB), a `draft` flag, the `base` branch that moves when it merges, the `head` branch whose commits it proposes, its author,
its times, and a state:

| State | Meaning |
| --- | --- |
| `open` | proposed; it can be commented on, edited, closed and, unless it is a draft, merged |
| `merged` | the head was merged into the base; final |
| `closed` | withdrawn; it can be reopened |

Rules at opening: the branches must exist and differ, the head must have commits the base lacks (`400 invalid_request`
otherwise), and no other **open** pull request may have the same two branches (`409 pull_request_exists`, naming the
number). The base defaults to the default branch. Renaming a branch keeps its pull requests (they follow the new name).
A pull request keeps pointing at branch names, so while it is open it follows the branches as they move: new commits on the
head appear in it at once.

When a pull request closes or merges, the two branch heads are **frozen** on it (the base as it was before the merge), so its
commits and diff stay readable after the branches move or are deleted. Reopening unfreezes it.

## Who may do what

The rules reuse the repository's [roles and permissions](access.md); nothing new is added.

| Action | Needs |
| --- | --- |
| open a pull request | `checkin` |
| comment | `read` (a signed-in account that can read the repository; a public repository's anonymous readers cannot comment) |
| edit title, body or the draft flag; close; reopen | the pull request's author, or `checkin` |
| merge | `checkin` (the same permission any [merge](merging.md) needs), and `edit_policy` when the merge brings a different policy file into the default branch |
| read, list, see the commits and diff | `read` |

An author who has lost their access can still edit and close their own pull request while they can read the repository.

## Mergeability

For an open pull request the server previews the merge of the head into the base ([merging](merging.md#the-operation)) and
reports its `outcome`: `up_to_date` (the head is already in the base), `fast_forward`, `merge` or `conflicted`, with the
conflicting paths and why. A pull request is never merged with conflicts: merge the base into the head branch (`pyn merge`)
and resolve them there, then push; the pull request follows. Merging a pull request whose head is already in the base marks it
merged with no commit.

## Merging

Merging writes the merge as `merge` does and then records the pull request as merged, with who merged it and the commit
(`merge_commit`: the merge commit, or the head's own commit for a fast-forward). The request chooses the method through the
existing `fast_forward` setting: `allow` (the default: fast-forward when possible, a merge commit otherwise), `never` (always a
merge commit) or `only` (refuse with `not_fast_forward` unless it fast-forwards). A `message` sets the merge commit's message
(`Merge pull request #N from <head>` by default). `expect_target` and `expect_head` name the commits the caller saw as the
heads of the base and head branches; if either moved, nothing is written and the answer is `409 merge_stale`.

A merge is refused when:

- the pull request is a draft (`409 pull_request_draft`) or not open (`409 pull_request_not_open`);
- it has conflicts (`409 merge_conflict`);
- the base moved since `expect_target` (`409 merge_stale`), or the method cannot be honoured (`409 not_fast_forward`);
- another account holds a live lock on a path the pull request changes (`409 pull_locks_held`, listing the paths and holders).
  A lock held by the merger does not count. An account with the `force_unlock` permission may merge over other people's locks;
  the locks are left as they are (unlocking stays the separate, audited `unlock --force`).

### Locks

The [merge](merging.md#exclusive-paths) of an exclusive path needs no lock when one side descends from the other. A pull
request goes further, because merging it lands someone's work while another person is editing the same exclusive file: each
pull request reports the live locks (path, holder, lease) on the paths it changes, whoever holds them, and refuses the merge
while others hold any. A path counts when it differs between the merge base and the head (a path changed and changed back does
not).

## Comments and events

Anyone who can read may comment, on a pull request in any state. Each comment, and each state change, is an **event** in one
ordered timeline: `comment` (with the `body`), `merged` (with the `commit`), `closed` and `reopened`, each with the actor and
time. Events are never edited or removed. A pull request counts its comments.

## Not here yet

Left for the work that depends on rulesets (#153) and statuses (#155): reviews, approvals and requested changes, required
approvals, line comments and threads, check results and requiring them, merge by squash or rebase, resolving conflicts in the
browser, and audit and webhook events for pull request activity. The wire types carry no review fields until then, rather
than stubs.
