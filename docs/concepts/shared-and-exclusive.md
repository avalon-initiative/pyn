# Shared and exclusive paths

The idea behind pyn on one page. The rest of the [concepts](README.md) fill in the detail.

Version control assumes two people can change the same file and the system can combine the results. That holds for source code. It
fails for a `.uasset`, a texture, a 3D model, a scene saved as a binary, a design file: there is no merge, so the second person
to save either loses work or has to redo it by hand.

pyn gives each path one of two modes, set in the repository's [`.pyn/pyn.toml`](pyn-toml.md):

| Mode | Meaning | Who may change it |
| --- | --- | --- |
| `shared` | Edited concurrently and merged, as in git. | Anyone with write access, any time. |
| `exclusive` | One editor at a time, enforced by the server. | Only the holder of the lock. |

The mode is a policy choice, not a file-type rule: a text scene can be exclusive, and a binary can be shared if the team accepts
whole-file conflicts. The file is versioned, so the rules change through a reviewed commit like code.

## A worked example

A team keeps a game level (`Content/Maps/Arena.umap`, exclusive) and its gameplay code (`Source/Arena.cpp`, shared) in one
repository.

1. Ana runs `pyn lock Content/Maps/Arena.umap`. The server grants a lease (8 hours by default) and the file becomes writable in her
   workspace. Everyone else's copy stays read-only.
2. Ben runs the same command. The server refuses with `lock_held`, naming Ana and when the lease ends. He opens the level in
   a viewer, or works on `Source/Arena.cpp` instead.
3. Ben and Ana both edit `Source/Arena.cpp`. Each commits; the second `pyn pull` merges the two, like git. A true conflict is resolved
   in the usual way.
4. Ana runs `pyn commit` and `pyn push`. The new level version is uploaded and her lock is released. Ben can now lock the level, and
   starts from her version, not an older one: a lock is only granted when the caller's copy is at the current head.
5. If Ana had left for a holiday without unlocking, the lease would run out by itself. An administrator can also remove a lock at
   once with `pyn unlock --force --reason "..."`; the reason is recorded in the audit log.

## What the server enforces

The server is the only enforcer. Committing an exclusive path is rejected unless the caller holds a live lock and the change is
based on the current head, whichever client sent it. The base is checked when the lock is taken: the client names the
revision its copy is based on (for a local commit, the revision that commit changed, not the server's head), and the lock is
refused with `stale_base` unless it is the head. In a pushed merge, an exclusive path both sides changed is accepted
only under the pusher's lock, as the person's resolution ([merges pushed from a local store](merging.md#merges-pushed-from-a-local-store)). The read-only bit on disk is a convenience that stops an editor saving the
file in the first place; deleting it does not skip the lock, it only means the rejection comes at commit time instead of save time.
A rejected change leaves nothing behind.

## Choosing a mode

- Mergeable text (source, configuration, documents, text scenes): `shared`.
- Binary or generated files that two people cannot combine: `exclusive`.
- Unsure: a repository whose policy file is missing or empty treats everything as exclusive, the safe side. A repository made with
  `pyn repo create` starts with a [starter policy](pyn-toml.md#the-starter-policy) that is `shared` and locks about 35 well-known
  binary kinds.

For game projects see [Using pyn for a game project](../engines.md). For a team that already uses git, see
[Coming from git, Perforce or Unity Version Control](../coming-from.md), which also covers keeping git and adding locks.
