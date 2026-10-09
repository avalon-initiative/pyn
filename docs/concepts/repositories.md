# Repositories

A server hosts many repositories, addressed as `owner/name`, the way GitHub does. Everything that belongs to one
repository is kept apart from every other: its files and revisions, its locks, its policy, its audit log, who has which role,
and the tokens and invitations made for it. Account things (a user name, a password, SSH keys, sign-in) belong to the
server and work across repositories. Sections marked *provisional* are not settled and may change.

## Addresses and namespaces

An owner is a **namespace**; today that is a user account, and its name is the account's user name. A repository name is 1
to 100 lowercase letters, digits, `-`, `_` or `.`, starting with a letter or digit and not ending in `.`. Names are
unique per owner, so `alice/game` and `bob/game` are different repositories.

Anyone with an account can create repositories **in their own namespace** (restricting who may create them is a server
administration setting that comes with server administrators, a later step). The creator becomes the repository's `admin`.
Organizations as owners come later; until then only an account can own a repository.

| Where | Form |
| --- | --- |
| HTTP API | `/v1/repos/{owner}/{name}/...` |
| `pyn clone` | `http://host:7878/alice/game`, or `alice/game` on the configured server |
| CLI setting | `repo = "alice/game"`, `--repo alice/game` or `PYN_REPO` |

SSH addresses (`ssh://host/owner/name`) arrive with the SSH transport.

## What each repository has

| Setting | Meaning |
| --- | --- |
| `visibility` | `private` (the default) or `public`. Stored and returned today; public access without signing in is not implemented yet, so every repository still needs credentials. *Provisional* |
| `lease_hours` | How long a checkout lasts unless renewed, 1 to 720, default 8 |

The owner can change both and rename the repository. A rename keeps all of its data: internally a repository has a fixed
opaque id, and the name is only its address, so old addresses stop working and may be reused by a new repository without
touching the old data.

## Who can do what

- **See a repository.** Only people who have a role in it. A private repository does not reveal that it exists to anyone
  else: every route answers `404 repo_not_found` for an outsider and for a repository that is not there.
- **Create.** Any signed-in account, in its own namespace. A token can create a repository only if it is not limited to
  specific repositories and carries `manage_roles`.
- **Rename, change settings, delete.** The owner, who must also hold `manage_roles` in the repository (an `admin`). A
  member who is not the owner gets `403 not_namespace_owner`, even if they are an admin.
- **Everything inside** is decided by the caller's role in that repository; see [access](access.md).

## Deleting a repository

Deleting removes the repository's files and history, its locks, its members, role changes and invitations. Two things
stay: the audit log, which is append-only (the deletion is its last event), and the stored content, which is shared across
the server and is not garbage-collected yet. Tokens that were limited to the repository simply stop matching anything.

## The policy

Each repository enforces the `.pyn/pyn.toml` at its own head, read when the server first opens the repository and replaced
the moment a new revision of the file lands; see [Changing policy](pyn-toml.md#changing-policy). `pyn repo create` checks in a
first policy file for the new repository (everything exclusive, or the file given with `--policy`; `--no-policy` skips it).
Until a repository has a policy file, the server's `PYN_CONFIG` file applies to it, or everything is exclusive if that is
unset.

## Moving from a single-repository server

A server that ran before repositories existed kept everything under one repository id, `default`. When it first starts with
the new version, PostgreSQL storage registers that data as the repository `<owner>/default`, where the owner is the oldest
admin of the old repository (or the oldest member, or `default` if there were no members). Nothing is copied: the locks,
revisions, roles and tokens keep working under the same id, and existing tokens still reach it. The new repository is
private. A server with no earlier data starts with no repositories. In-memory storage never has earlier data.

## API summary

| Route | Purpose |
| --- | --- |
| `GET /v1/repos` | repositories you belong to (`?owner=` narrows it), each with your role |
| `POST /v1/repos` | create `{name, owner?, visibility?, lease_hours?}` |
| `GET /v1/repos/{owner}/{name}` | one repository |
| `PATCH /v1/repos/{owner}/{name}` | rename or change settings `{name?, visibility?, lease_hours?}` |
| `DELETE /v1/repos/{owner}/{name}` | delete |
| `GET /v1/repos/{owner}/{name}/me` | who you are and what you may do there |
| `GET /v1/repos/{owner}/{name}/{tree,summary}` | [folder listing and summary](browsing.md) |
| `/v1/repos/{owner}/{name}/{locks,files,content,checkout,release,checkin,restore,force-unlock,audit,objects,history}` | the file and lock operations |
| `/v1/repos/{owner}/{name}/{members,roles,invites,users}` | who has access |

Account routes do not name a repository: `/v1/register`, `/v1/login`, `/v1/session`, `/v1/me`, `/v1/me/password`,
`/v1/keys` and `/v1/tokens`. The previous single-repository routes (`/v1/files`, `/v1/checkout` and so on) are gone.

On the command line: `pyn repo create [owner/]name [--visibility public|private] [--lease-hours N] [--policy FILE | --no-policy]`, `pyn repo list`,
`pyn repo delete owner/name`, and `pyn clone <server>/owner/name [dir]`. A workspace records its repository in
`.pyn/local_only/config.toml`, so commands inside it need no flag; see [the `.pyn/` folder](workspace.md).
