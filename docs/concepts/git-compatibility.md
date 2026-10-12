# Git compatibility

Decision #218: where pyn has an equivalent of a git command it uses git's name, flags, argument order and output shape;
every deliberate difference is listed here. pyn owns what git lacks (`lock`, `unlock`, `policy`, modes, leases,
organizations and teams). This is a first, simple mapping; it is provisional until it is formalised (#220).

Conventions that hold everywhere:

- Exit codes follow git: `0` success, `128` for a failure (message prefixed `fatal: `), `129` for a usage error.
  An unknown or git-only command exits `1`, as git does for an unknown command.
- A git command pyn does not support yet fails with a message saying so, not a usage error. `checkout` is reserved
  for git's meaning (switching branches, restoring paths); taking a lock is `pyn lock`.
- Listings are aligned tables with the path or variable column last, not git's free-form blocks. This is a deliberate,
  repository-wide difference (it keeps columns such as mode, lock and revision readable).

| Git command | pyn command | Status | Reason |
| --- | --- | --- | --- |
| `git clone <repo> [<dir>]` | `pyn clone <source> [<dir>]` | same | Same arguments. The source is `<server>/owner/name` or `owner/name`; output is one summary line, not progress. |
| `git status` | `pyn status` | different | Table with mode, revision, state, lock and path plus a summary line, because lock and mode state have no place in git's layout. No `-s`, `--porcelain` or pathspecs yet. |
| `git commit -m <msg> [<path>...]` | `pyn commit` (alias `pyn checkin`) | different | Takes explicit paths and no staging area (`add` is unsupported), one changeset per call. `--file` names the content file for a single path (git's `-F` reads the message). `--base` is pyn-only. Output is `<path> is now at revision N`, not a commit summary. Releases locks. |
| `git add` | none | unsupported | pyn has no index; `commit` takes paths directly. |
| `git log [-n <n>] [<path>]` | `pyn log [-n <n>] [<path>]` (aliases `history`, `--limit`) | different | Newest first, as git. Output is a table of per-path revisions, not commits. `--filter <glob>` is pyn-only. Default 50 for the repository view. No `--oneline`, `--graph` or revision ranges. |
| `git restore --source=<rev> <path>` | `pyn restore --source <rev> <path>` | different | The revision may still be given positionally (`pyn restore <path> <rev>`). pyn's restore appends a new revision on the server, so it needs the lock, the `restore` permission and a typed confirmation (`--yes` skips it); git's only edits the working tree. `-s` is the short form. |
| `git show <rev>:<path>`, `git cat-file` | `pyn get <path> [--rev N] [-o file]` | different | Fetches content at a revision to stdout or a file. pyn has no commit-ish syntax yet. |
| `git pull`, `git fetch` | `pyn update [<path>...]` | different | Fast-forwards files without local changes and never overwrites edits. Neither command exists by name until branches and merge land. |
| `git ls-files`, `git ls-tree` | `pyn files`, `pyn ls [<path>]` | different | Tables with mode, head revision and lock holder. |
| `git config --list`, `--get`, `<key> <value>` | `pyn config list`, `get`, `set` | different | Subcommands instead of flags; `--global` and `--local` behave as in git. Keys are pyn's own. |
| `git reflog` (loosely) | `pyn audit` | pyn only | Append-only audit log of lock, revision and access events. |
| none | `pyn lock`, `pyn unlock`, `pyn locks`, `pyn policy` | pyn only | Leased locks and per-path modes. |
| none | `pyn login`, `logout`, `register`, `whoami`, `token`, `key`, `invite`, `user`, `member`, `role`, `org`, `team`, `repo`, `admin`, `setup`, `usage`, `summary` | pyn only | Server, account and access administration. |
| `git init` | none | unsupported | Repositories are created on the server (`pyn repo create`) and cloned. |
| `git diff [<c1> [<c2>]] [-- <path>...]` | `pyn diff` | different | Unified output, `-U<n>`, `--stat`, `--name-only`, `--name-status`, `--exit-code`, `-q`. Commits are `cN`, `HEAD`, `HEAD~N`. No staging area, so `--cached` and `--staged` fail with a message. See commits.md. |
| `git show [<commit>]` | `pyn show [<commit>]` | different | Header, message and the commit's diff, with the diff format flags. File content at a revision is `pyn get`. |
| `git branch`, `git switch`, `git checkout`, `git merge`, `git tag`, `git stash` | none | unsupported | Phase 1 has a single mainline. `checkout` stays reserved for git's meaning. |
| `git push`, `git remote` | none | unsupported | Check-in goes straight to the server; there is one remote, set by `--server` and `--repo`. |
| `git reset`, `git rm`, `git mv`, `git rebase`, `git cherry-pick`, `git revert`, `git grep`, `git clean` | none | unsupported | Not planned for the first scope. |

Not aligned and recorded as open: `pyn status`/`commit` output shape against git's, `pyn get` against `git show`, and
`-C <dir>` and other global git flags, which pyn does not take.
