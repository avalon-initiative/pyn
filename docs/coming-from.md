# Coming from git, Perforce or Unity Version Control

What carries over, what changes, and how to move. Facts about other tools are as of writing and come from their public
documentation; check them if a decision depends on one.

## From git

pyn uses git's command names, flags and exit codes wherever it has an equivalent (`clone`, `status`, `add`, `commit`, `push`,
`pull`, `log`, `diff`, `restore`, `merge`, `rebase`, `stash`, `tag`), so `alias git=pyn` works for everyday flows. Every
difference is listed in [git compatibility](concepts/git-compatibility.md). What pyn adds is `lock`, `unlock`, `locks`, `policy`,
leases, and organizations and teams.

A repository has one home: a pyn server, or git. In a git repository pyn behaves as git (through its own implementation: git does
not need to be installed), and the commands that only a server provides say so.

| Goal | How |
| --- | --- |
| Keep a git repository and still use pyn's commands | `pyn convert` in the work tree adopts it as a git home. Nothing git sees changes except what a command does. |
| Add enforced locks to an existing git repository | `pyn convert --home pyn --server <url> --repo owner/name` moves the repository, with its history, to a pyn server. `--exclusive <glob>` (repeatable) makes those paths exclusive in the starter policy. |
| Use git LFS locks against pyn | A pyn server speaks the Git LFS Batch and Locks APIs, so stock `git lfs lock` and `git lfs push` work against it. See [Git LFS API](concepts/lfs.md). |
| Leave | `pyn convert --home git` turns a server workspace into a git home and keeps the history; the command lists what is lost first. |

Locks on a path that git-lfs marks `lockable` are advisory unless the path is `exclusive` in `.pyn/pyn.toml`; `pyn policy lint` lists
the paths where the two disagree.

## From Perforce (Helix Core)

The model is the same where it matters: exclusive files, a server that holds the truth, and large binaries. The differences are that
pyn uses a distributed workflow for everything not locked, and merges text like git.

| Perforce | pyn |
| --- | --- |
| `p4 sync` | `pyn pull` (`pyn clone` the first time) |
| `p4 edit <file>` (exclusive checkout) | `pyn lock <path>` |
| `p4 edit` on a file that merges | nothing: a `shared` path is just edited |
| `p4 add`, `p4 delete` | `pyn add <path>` (a deleted tracked file is staged the same way) |
| `p4 revert` | `pyn restore <path>`; `pyn unlock <path>` gives up the lock |
| `p4 submit` | `pyn commit`, then `pyn push` (publishing releases locks) |
| `p4 opened`, `p4 fstat` | `pyn status` |
| `p4 locks` | `pyn locks` (`--mine`, `--watch`, `--json`) |
| `p4 filelog`, `p4 changes` | `pyn log [<path>]` |
| `p4 diff` | `pyn diff` |
| `p4 unlock -f` | `pyn unlock --force --reason "..."` (needs the `force_unlock` permission) |
| Typemap (`+l` filetype) | `exclusive` paths in `.pyn/pyn.toml` |
| Streams and depots | Repositories and branches |

Not available yet: importing Perforce history (tracked as an open feature), partial or sparse workspaces on a pyn server, and the
editor integrations and build tool integrations Perforce teams are used to; see [engines](engines.md).

## From Unity Version Control (Plastic SCM)

| Unity Version Control | pyn |
| --- | --- |
| Check out an exclusive file | `pyn lock <path>` |
| Check in | `pyn commit` and `pyn push` |
| Lock rules (`lock.conf`) | `exclusive` paths in `.pyn/pyn.toml` |
| Branch, merge | `pyn switch -c`, `pyn merge` |

Importing a Unity Version Control repository is not available yet. Moving over today means checking in the working files into a new
repository, or going through git if the old repository can be exported to it.

## What is not the same

- No built-in editor plug-ins yet; see [engines](engines.md) for the state of each.
- Pull requests, reviews and CI are not built in yet.
- Releases are alpha: interfaces and the `--json` shapes may still change.
