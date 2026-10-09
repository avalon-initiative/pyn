# Repositories

A server hosts many repositories, addressed as `owner/name`, the way GitHub does. Everything that belongs to one
repository is kept apart from every other: its files and revisions, its locks, its policy, its audit log, who has which role,
and the tokens and invitations made for it. Account things (a user name, a password, SSH keys, sign-in) belong to the
server and work across repositories. Sections marked *provisional* are not settled and may change.

## Addresses and namespaces

An owner is a **namespace**: a user account or an [organization](#organizations), and its name is the account's user name or
the organization's name. Users and organizations share one namespace, so a name belongs to exactly one of them. A repository name is 1
to 100 lowercase letters, digits, `-`, `_` or `.`, starting with a letter or digit and not ending in `.`. Names are
unique per owner, so `alice/game` and `bob/game` are different repositories.

Anyone with an account can create repositories **in their own namespace** (restricting who may create them is a server
administration setting that comes with server administrators, a later step). The creator becomes the repository's `admin`.
Only an owner of an organization can create repositories in it.

**Reserved names.** `_`, `-`, `about`, `admin`, `api`, `assets`, `explore`, `healthz`, `help`, `login`, `logout`, `new`,
`notifications`, `openapi`, `orgs`, `pricing`, `register`, `search`, `settings`, `signup`, `static`, `user`, `users` and
`v1` cannot be a user name or an organization name, because the web app and the API use them as routes. The answer is
`400 reserved_name`. (A repository name cannot be `-` either, since it must start with a letter or digit, which is what
lets the web app keep `/:owner/-/...` for organization pages.)

## Organizations

An organization is a namespace that owns repositories, so a team can own a repository rather than one person's account.
It is an account of kind `org` in the same namespace as users, with the same name rules (2 to 39 lowercase letters, digits,
`-` or `_`). An organization **cannot sign in, and cannot hold keys, tokens or sessions**; it is never created by sign-up;
people act on its behalf, and a request that names an organization as the signed-in user is refused.

- **Create.** Any signed-in account may create an organization (`POST /v1/orgs`) and becomes its first **owner**. The person
  running the server can restrict this to server administrators with `PYN_ORG_CREATION=admins` (default `anyone`); anyone
  else then gets `403 server_admin_required`. A token needs `manage_roles` and no repository limit, as for creating a
  repository. A name already taken by a user or an organization is `409 user_exists`.
- **Repositories.** Only an owner creates repositories in the organization (`POST /v1/repos` with `owner` set to it); anyone
  else gets `403 not_org_owner`. An owner of the organization is an **implicit `admin` on every repository it owns**, with
  no per-repository grant, and always at least that: a weaker direct role never lowers an owner. The same rule applies
  everywhere a role is looked up (a repository's `me`, the repository list and its `role`, tokens and sessions, which
  tokens still narrow). Owners rename, reconfigure and delete the organization's repositories; another admin of the
  repository gets `403 not_org_owner` for those three.
- **Delete.** An owner can delete an organization only when it owns no repositories (`409 org_not_empty`); delete the
  repositories first. Its audit log is kept and its name is free again.
- **Audit.** Organization events (`org_created`, `org_deleted`) go to a separate organization log, readable by its owners
  at `GET /v1/orgs/{org}/audit`. They are not in any repository's log or the server log.

Members, teams and organization-level role grants are later steps: until then an organization has exactly one member, its
creator, and the owner is the only way anyone has access to its repositories. Leaving, removing or promoting owners, and the
rule that an organization always keeps an owner, arrive with member management.

| Where | Form |
| --- | --- |
| HTTP API | `/v1/repos/{owner}/{name}/...` |
| `pyn clone` | `http://host:7878/alice/game`, or `alice/game` on the configured server |
| CLI setting | `repo = "alice/game"`, `--repo alice/game` or `PYN_REPO` |

SSH addresses (`ssh://host/owner/name`) arrive with the SSH transport.

## What each repository has

| Setting | Meaning |
| --- | --- |
| `visibility` | `private` (the default) or `public`; see [Visibility](#visibility) |
| `lease_hours` | How long a lock lasts unless renewed, 1 to 720, default 8 |
| `max_locks_per_user` | How many locks one user may hold in the repository, 1 to 10000. Unset follows the server's `PYN_MAX_LOCKS_ALLOWED_PER_USER` (default 5). A limit in the repository's [`pyn.toml`](pyn-toml.md#lock-limit) overrides it |

The owner can change all three and rename the repository. The limit counts one user's live locks in this repository only; a
limit across repositories is not implemented. A rename keeps all of its data: internally a repository has a fixed
opaque id, and the name is only its address, so old addresses stop working and may be reused by a new repository without
touching the old data.

A repository reports `max_locks_per_user` (the limit in force), `max_locks_per_user_setting` (the stored setting, absent when it
follows the server) and `max_locks_set_by_policy` (true when `.pyn/pyn.toml` sets the limit, so the setting is read-only).

## Who can do what

- **See a repository.** Anyone, signed in or not, if it is public; otherwise only people who have a role in it. A private
  repository does not reveal that it exists to anyone else: every route answers `404 repo_not_found` for an outsider (or
  an anonymous request) and for a repository that is not there.
- **Create.** Any signed-in account, in its own namespace, or an owner of an organization, in the organization's. A token
  can create a repository only if it is not limited to specific repositories and carries `manage_roles`.
- **Rename, change settings, delete.** The owner, who must also hold `manage_roles` in the repository (an `admin`): the
  user in their own namespace, an organization owner in an organization's. A member who is not the owner gets
  `403 not_namespace_owner` (`not_org_owner` in an organization), even if they are an admin.
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

## Visibility

A repository is `private` (members only) or `public` (anyone may read it), as on GitHub. Registering creates an account,
not access: a new account has no role anywhere (there is no default role setting), so it can use its own settings (keys,
tokens, password), create repositories in its own namespace and read public repositories, and nothing else.

- **Public** grants the `read` permission to everyone, including requests with no credentials: the tree, files, file
  content at any revision, history, summary, locks and repository info. A role keeps the permissions it already has.
  Everything else needs a role: lock, checkin, restore, upload, force-unlock, audit, members, roles and invitations
  answer `403` to a signed-in person without the permission and `401` to an anonymous request.
- **Private** answers `404 repo_not_found` to anyone without a role, which is also what a repository that does not exist
  answers. A request that presents a credential that is wrong or expired is `401` either way.
- A token limited to other repositories, or a role without `read`, still reads a public repository (`read` only).
- `GET /v1/repos` and `GET /v1/me/locks` list only repositories where the caller has a role (or, for locks, can read);
  public repositories are reached by address.
- The owner, holding `manage_roles` in the repository, changes visibility with `PATCH` (`pyn repo visibility owner/name
  public|private`). It takes effect at once and is recorded as a `repo_updated` audit event whose detail reads
  `visibility private -> public`.

## API summary

| Route | Purpose |
| --- | --- |
| `GET /v1/repos` | repositories you belong to, including every one of an organization you own (`?owner=` narrows it), each with your role |
| `POST /v1/repos` | create `{name, owner?, visibility?, lease_hours?, max_locks_per_user?}` |
| `GET /v1/repos/{owner}/{name}` | one repository (`role` is null for a reader of a public repository) |
| `PATCH /v1/repos/{owner}/{name}` | rename or change settings `{name?, visibility?, lease_hours?, max_locks_per_user?}` (`null` clears the lock limit) |
| `DELETE /v1/repos/{owner}/{name}` | delete |
| `POST /v1/orgs` | create an organization `{name}`; you become its owner |
| `GET /v1/orgs` | organizations you belong to, each `{name, created_at, role}` |
| `GET /v1/orgs/{org}` | one organization; `role` is null when you have none (`404 org_not_found` for a user name too) |
| `DELETE /v1/orgs/{org}` | delete, when it owns no repositories |
| `GET /v1/orgs/{org}/audit` | the organization's audit log (owners; `?before=`, `?limit=`) |
| `GET /v1/repos/{owner}/{name}/me` | who you are and what you may do there |
| `GET /v1/repos/{owner}/{name}/{tree,summary}` | [folder listing and summary](browsing.md) |
| `/v1/repos/{owner}/{name}/{locks,files,content,checkout,release,checkin,restore,force-unlock,audit,objects,history}` | the file and lock operations |
| `/v1/repos/{owner}/{name}/{members,roles,invites,users}` | who has access |

Account routes do not name a repository: `/v1/register` (and `/v1/register/verify`, `/v1/register/resend`), `/v1/admin`, `/v1/login`, `/v1/session`, `/v1/me`, `/v1/me/password`,
`/v1/me/locks`, `/v1/keys` and `/v1/tokens`. The previous single-repository routes (`/v1/files`, `/v1/checkout` and so on) are gone.

`GET /v1/me/locks` lists the caller's live locks across every repository they can read (a token only sees the
repositories and permissions it carries), ordered by `owner/name` then path. Each entry is
`{owner, name, path, acquired_at, expires_at}`; remove one with the repository's own `POST .../release` (`pyn unlock`), which records
the audit event as usual. A lock in a repository the caller can no longer read is not listed.

On the command line: `pyn repo create [owner/]name [--visibility public|private] [--lease-hours N] [--max-locks N] [--policy FILE | --no-policy]`, `pyn repo list`,
`pyn repo visibility owner/name public|private`, `pyn repo delete owner/name`, `pyn locks --mine` (your locks in every repository: repository, acquired, expires, path), and `pyn clone <server>/owner/name [dir]`. A workspace records its repository in
`.pyn/local_only/config.toml`, so commands inside it need no flag; see [the `.pyn/` folder](workspace.md).
