# Diffs

A diff shows what a commit or branch changes, line by line. The server builds it in house from the stored revisions (decision #30:
no dependency on git); the web app and any client read the same two endpoints. The command line's `pyn diff` runs the same
line-diff engine (the `pyn-diff` crate) on the client and prints git's formats; the endpoints here return structured hunks.

## `GET /v1/repos/{owner}/{name}/diff`

Needs `read` (a public repository is readable without credentials). Query parameters:

| Parameter | Meaning |
| --- | --- |
| `head` | required: a branch, or a commit id or abbreviation |
| `base` | the other side, the default branch when omitted |
| `merge_base` | `true` (the default): measure from the merge base of the two, as a pull request shows its changes and as [compare](branches.md#comparing-branches) does; `false`: the two commits directly, so what only `base` changed shows as reversed |
| `context` | unchanged lines around each change, 0 to 100 (3 by default; more is `400 invalid_request`) |

The answer has `base`, `base_commit`, `head`, `head_commit`, `from_commit` (the commit the changes are measured from, null for
unrelated histories with `merge_base`), `truncated` and `files`, ordered by path. Each file has:

- `path`, `status` (`added`, `modified`, `deleted`, `renamed`) and, for a rename, `old_path`. Only an exact rename is
  detected: one deleted and one added path with identical content and no other candidate. A renamed file has no hunks.
- `additions` and `deletions` (lines), `binary` and `too_large`. A file with a NUL in its first 8000 bytes is binary; a file
  with a side over 1 MiB, or past the 16 MiB a single diff reads in all, is `too_large`. Neither has hunks, and a too large
  file reports 0 for the counts.
- `mode` (`shared` or `exclusive`, from the repository's current policy), `file_mode` (git's mode of the new side, of the old
  side for a deletion) and `old_file_mode` when the mode changed, and `old_content` and `new_content` (blob ids).
- `lock`: the live lock on the path, whoever holds it, in the shape of the [lock list](access.md#exclusive-paths-and-the-lock)
  (`path`, `owner`, `identity`, `acquired_at`, `expires_at`). Absent when nobody holds one.
- `hunks`: each with `header` (`@@ -a,b +c,d @@ section`), `old_start`, `old_lines`, `new_start`, `new_lines`, `lines` and the
  hidden-line counts below. A line is `{kind, old_no, new_no, text}`, `kind` being `context`, `add` or `delete`; `old_no` is absent on an added
  line and `new_no` on a deleted one; `text` has no newline, and `no_newline` is true on a last line that lacked one.

At most 1000 changed paths are listed (`truncated` otherwise), found as [compare](branches.md#comparing-branches) finds them.
Text diffs run off the request threads, at most `PYN_MAX_CONCURRENT_MERGES` at a time, and the request counts with the other
expensive reads ([limits](limits.md)).

## Hidden lines and `GET .../lines`

`hidden_before` on a hunk is the number of unchanged lines between the previous hunk (or the start of the file) and this one;
`hidden_after` on the last hunk is the number of unchanged lines to the end of the file. Both count the same lines on either side,
since unchanged lines are common to both. A client that shows an expand control fetches them:

`GET /v1/repos/{owner}/{name}/lines?path=<path>&rev=<ref>&start=<n>&end=<n>` returns `{path, commit, revision, total, start, lines}`:
the lines `start` to `end` (counting from 1, inclusive, `start` 1 by default; at most 2000 per request) of the path as of `rev`, a
branch or commit (the default branch when omitted). `lines` has no newlines and is empty past the end. Use the head commit and
the new-side numbers (`new_start` of the hunk and its neighbours) to fill the gap. A binary file or one over 1 MiB is `400
invalid_request`; a path that does not exist at the commit is `404 revision_not_found`.
