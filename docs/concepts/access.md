# Access: roles, permissions and tokens

Every request is checked against a permission. Health, the OpenAPI document and registration are open, and so is reading
a [public repository](repositories.md#visibility); everything else needs credentials. Accounts belong to the server, but roles, permissions, tokens' reach and invitations are per
[repository](repositories.md): being an `admin` of `alice/game` says nothing about `bob/tools`.

## Permissions and roles

| Permission | Allows |
| --- | --- |
| `read` | list files, folders and locks, see the repository summary, fetch any revision, see history |
| `lock` | lock and unlock exclusive files |
| `checkin` | upload content (recorded for the repository it goes to) and check in; create, rename and delete [branches](branches.md) |
| `restore` | make an older revision the head again |
| `force_unlock` | remove someone else's lock (`unlock --force`) |
| `edit_policy` | change `.pyn/pyn.toml` |
| `manage_users` | add members, assign roles, invite people, list or revoke tokens and keys of the repository's members |
| `manage_roles` | change what a role grants |
| `view_audit` | read the audit log |

Roles are named bundles. Each default role includes everything below it:

| Role | Adds |
| --- | --- |
| `reader` | `read` |
| `writer` | `lock`, `checkin` |
| `maintainer` | `restore`, `force_unlock`, `edit_policy`, `view_audit` |
| `admin` | `manage_users`, `manage_roles` |

A repository owner can change what any role grants. An owner of the [organization](repositories.md#organizations) that
owns a repository is an implicit `admin` of it. The `admin` role always keeps `manage_users` and `manage_roles`, and
nobody can grant a role containing a permission they do not hold themselves.

Changing a member's role also needs every permission the member's current role holds, so a manager cannot change the
role of a member who ranks above them. The account that owns a user-owned repository keeps its `admin` role there: only
that account or a server administrator changes it. A server administrator and an owner of the owning organization are
exempt from the rank check. The same rule applies to changing or removing a team's role in a repository.

## Effective role

What someone may do in a repository is decided by one function in the server: the highest role among the sources below.

1. A direct grant on the repository (members, invitations, the creator of a user's repository, or of an organization's
   repository when they are not an owner).
2. A grant to a [team](repositories.md#teams) the person is in (the highest, when several teams hold a role).
3. Owning the organization that owns the repository: `admin`.

A token or session then narrows the result. Every lookup (the repository's `me`, the repository list, token creation, role
checks) uses this one function. Being a plain organization member grants nothing by itself; a direct grant on an
organization's repository goes only to a member of that organization (see
[organizations](repositories.md#organizations)). The repository list (`GET /v1/repos`) includes those reached through a team. Nothing caches a role: it is read on every request, so adding or removing a team member, changing a team
grant or demoting an organization owner applies at once, to tokens as well.

`GET /v1/repos/{owner}/{name}/roles` lists what each role grants and is for members only: someone who can merely read a
[public repository](repositories.md#visibility) and holds no role gets `403 not_repo_member`.

`GET /v1/repos/{owner}/{name}/members` (needs `manage_users`) lists everyone with access, one row per person:
`{user, role, source}`, where `role` is the effective role and `source` is what decides it: `direct`, `team` or
`org_owner` (on a tie, direct comes first, then team). The teams that hold roles are listed at
`GET /v1/repos/{owner}/{name}/teams`.

On the command line, `pyn member list` prints these as `ROLE SOURCE USER`, and `pyn team access [<owner>/<repo>]` lists the
teams. Teams are managed with `pyn team ...` (see [teams](repositories.md#teams)). Who may create repositories in an organization is managed with `pyn org policy ...` (see
[repository creation policy](repositories.md#repository-creation-policy)).

## Accounts and signing in

People have accounts: a unique user name (2 to 39 lowercase letters, digits, `-` or `_`; not one of the
[reserved names](repositories.md#addresses-and-namespaces)) and a password of at least ten
characters (at most 256), stored only as a salted argon2id hash. `pyn login <name>` checks the password and saves an expiring session
for that server in your user configuration directory (`~/.config/pyn/credentials.toml`, readable only by you: the directory is `0700`, the file `0600`, written atomically), the way a git
credential helper would; the CLI uses it when no token is given, and `pyn logout` ends the session and removes it. A
session is a token that reaches every repository you belong to, with your role there as its limit, and it lasts 30 days by
default (`PYN_SESSION_DAYS`). Signing in needs no role anywhere: a new account with no repository can sign in and create its own.

## Web sessions

The web signs in with `POST /v1/session` (user name and password, throttled like `pyn login`). The server keeps the session
(only a hash of its cookie value is stored) and sets an `HttpOnly`, `SameSite=Lax` cookie, `pyn_session`, that page scripts
cannot read; it is also `Secure` when the request arrived over HTTPS (`X-Forwarded-Proto: https` from a TLS-terminating
proxy) or when `PYN_PUBLIC_URL` (or the address recorded by setup) is `https://`. The server warns at startup when that address
is not https and it is bound beyond loopback. A session lasts `PYN_SESSION_DAYS` (30 by default) and resolves to the user's current role in the repository being
used on every request, so a role change applies at once. `GET /v1/session` returns the signed-in user and a CSRF token
(what the user may do in one repository is `GET /v1/repos/{owner}/{name}/me`); `DELETE /v1/session` signs out and removes
the session on the server.

A request authenticated by the cookie alone must carry the CSRF token in an `X-Pyn-CSRF` header on every POST, PUT and
DELETE, or the server answers 403 `csrf_failed`. The check is skipped only when the request is authenticated by a bearer
token, or by the `X-Pyn-User` header on a server with development auth on; other `Authorization` schemes or a header the
server does not honour do not skip it.

Sign-in and every cookie-authenticated POST, PUT, PATCH and DELETE also have their origin checked. The browser's `Origin`
header (the `Referer`'s origin when there is none) must be the origin of `PYN_PUBLIC_URL` (or the address recorded by
setup) or an entry of `PYN_ALLOWED_ORIGINS`, such as the Vite dev server, or the server answers 403 `origin_not_allowed`.
A cookie write with neither header, or with `Origin: null`, is refused too. A sign-in with neither header is accepted,
because browsers always send one on a cross-site POST and a header-less sign-in is a script such as the CLI, which holds
no cookie to abuse. Requests authenticated by a bearer token or the development header are not checked, and `GET`
requests never are. Origins compare by scheme, host and port, ignoring case and a default port.
The account pages (SSH keys, personal access tokens, password) are the same endpoints used with a session.

Failed sign-ins are throttled twice: five wrong passwords for a user name (which need not exist) lock that name out for
15 minutes, and 30 failures from one client address lock that address out for the same time, whatever names it tries.
Every attempt is counted before the password is checked, so attempts past a limit are refused without any password work,
even when they arrive together. A success clears the name's count and gives the address's count back. The window is fixed:
attempts made while locked out do not extend it. User names and addresses longer than 64 characters are keyed by their
SHA-256 digest. The error never says whether the name or the password was wrong, and
a user name that does not exist costs the same work as one that does. Only a right password learns that the account cannot
sign in yet (`email_not_verified`, `approval_pending`, `account_disabled`, all 403). `pyn password` changes your own password and ends your other web sessions (the one making the change stays signed in) and the sign-in tokens that `pyn login` issued, including the one the CLI itself was using: the CLI then says so, forgets the saved sign-in and tells you to run `pyn login` again; tokens you created and SSH keys keep working. An operator's password reset ends all sessions and sign-in tokens too. The current password is always required when the account has one; an account with no password yet (created through an external provider) can set its first only from a signed-in web session.

A server can also sign people in through an external OpenID Connect provider, and turn password sign-in off (server
administrators keep it); see [signing in with an external provider](external-sign-in.md).

## SSH keys

SSH keys are how people sign in from the command line and, when the SSH transport arrives, how clone and push authenticate,
as with git. Link a public key to your account with `pyn key add` (it uses `~/.ssh/id_ed25519.pub`, `id_ecdsa.pub` or
`id_rsa.pub` if you do not name a file), see them with `pyn key list`, and remove one with `pyn key remove <id>`.

As on GitHub, a key belongs to exactly one account: its fingerprint, the same `SHA256:...` that `ssh-keygen -l` prints,
is unique across the server, and linking a key that another account has is refused. Ed25519, ECDSA, security keys and RSA
of at least 2048 bits are accepted; DSA and small RSA keys are not. Someone with `manage_users` in a repository can list and
remove the keys and tokens of its members, except those of a server administrator or of the repository's owner (the owning
account, or an owner of the owning organization), which only a server administrator can. [Server administrators](#protecting-open-registration) approve and disable
accounts but do not gain that.

Token management for another person is bounded by the same reach. A manager lists and revokes only the tokens limited to
repositories where the manager holds `manage_users`, and the repositories shown are those. A token limited to any other
repository, or one covering every repository (such as a sign-in token), is left out of the listing and revoking it reads
as `404 token_not_found`; only a server administrator reaches those. A token id that does not exist, or that belongs to
someone the caller cannot manage, gets the same answer. A bearer token with an unknown id and one with a wrong secret get
the same `401`.

## Joining a server

The person running the server chooses how people join with `PYN_REGISTRATION`:

| Mode | Who can create an account |
| --- | --- |
| `invite` (default) | anyone with an invitation from a member who has `manage_users` in a repository |
| `open` | anyone; the account has no role in any repository (it can read public ones) until someone adds it. Meant for a public server; see [protecting open registration](#protecting-open-registration) before exposing it to the internet |
| `closed` | nobody on their own; someone with `manage_users` adds people with `pyn user add` |

Someone with `manage_users` in a repository can add people to it by hand with `POST /v1/repos/{owner}/{name}/users`, but
that creates an account, so it follows the registration policy: on a `closed` or `invite` server, or an `open` one that
requires approval or email verification, only a server administrator may (everyone else gets `403 forbidden`). On a plain
`open` server it is allowed and rate limited like sign-up. Adding an existing person to a repository is not account
creation and needs only `manage_users`. An invitation is a one-time code
(`pyn invite create --role writer`) for one repository that carries the role the new person will get there and an expiry of 1 to 8760 hours; it
is shown once, can be revoked only in the repository it belongs to, and cannot grant a role above the level of the person
creating it. The new person runs `pyn register <name> --invite <code>` and gets that role in that repository.

## Protecting open registration

An open server attracts bots and password guessing, so it layers these protections. Each is server-side; clients only
display what the server says. The mode default stays `invite` for now.

**Email verification** (`PYN_EMAIL_VERIFICATION`, on unless set to `false`). An open sign-up must carry an `email`. The
account is created `pending_verification` and cannot sign in. The server mails a link,
`{PYN_PUBLIC_URL}/verify-email?token=...`, that works once for 24 hours; the web page behind it calls
`POST /v1/register/verify {token}`. A pending account that never verifies, and has no live link, gives its name back after
24 hours. Addresses are lowercased; an address can be verified by only one account. Invite-only and closed servers do not
verify: the invitation or the administrator vouches for the person.

**Approval** (`PYN_REQUIRE_APPROVAL=true`, off by default). After the address is verified (or at sign-up when verification
is off) the account waits as `pending_approval` until a server administrator approves it.

**Rate limits** (fixed windows, per client address and per target; limits are counted before any password work):

| What | Limit | Setting |
| --- | --- | --- |
| failed sign-ins per user name | 5 per 15 minutes | `PYN_RATE_SIGN_IN_ACCOUNT` |
| failed sign-ins per client address | 30 per 15 minutes | `PYN_RATE_SIGN_IN_CLIENT` |
| sign-up and resend attempts per client address | 10 per hour | `PYN_RATE_REGISTER_CLIENT` |
| verification emails per recipient address | 3 per hour | `PYN_RATE_MAIL_PER_EMAIL` |

A refused request is 429 `too_many_attempts` with a `Retry-After` header in seconds. The per-recipient limit never
changes the response: past it a sign-up still answers as usual and simply sends nothing. The client address is the
connection's peer address; behind a reverse proxy set `PYN_TRUST_FORWARDED_FOR=true` to use the last entry of
`X-Forwarded-For` instead (only when the proxy sets it; otherwise anyone could pick their own address). IPv6 addresses are
limited per /64. Counters are in memory, or in PostgreSQL when `PYN_DATABASE_URL` is set, so restarts and several servers
do not reset them.

**No user enumeration.** Sign-in answers the same for a wrong password and an unknown user, with the same hashing work.
Signing up with an email address that already belongs to a verified account gets the same `201` as a fresh one (and the
hashing work is the same); the address's owner gets a notice instead of a link, and no account is created.
`POST /v1/register/resend` always answers `202`. A taken user name is `409 user_exists`, as on GitHub, since names are public.

**Server administrators** approve, disable and enable accounts. The account created by [first-run setup](#first-run-setup) is the first; a token
acts as one only if it is a [full-scope credential](#full-scope-credentials) of an administrator account. An administrator grants the flag to another active account and revokes it from
any account, their own included; the last active administrator cannot be revoked (`409 last_server_admin`), so the server is
never left without one; disabling the last active administrator is refused the same way. A service credential cannot grant or revoke (`403 server_admin_required`). Grants and revokes are
recorded with who and whom in the audit log. A disabled account cannot sign in, its sessions end at once, and its tokens
and SSH keys stop working (403 `account_disabled`) until it is enabled; its email address stays reserved. Administrators
cannot disable themselves. Approvals, disables and enables are recorded with the actor and reason in a server-wide audit
log, read at `GET /v1/admin/audit`.

**Password hashing** runs on a blocking thread pool, at most `PYN_PASSWORD_HASHES` at a time (default: the number of CPUs;
each argon2id hash uses about 19 MiB), so heavy sign-in or sign-up traffic cannot stall other requests.

**Email delivery.** The only sender today (`PYN_EMAIL=log`) records that a message was queued, with the recipient's domain and
the subject, and writes nothing else, which suffices for development and tests, not for real users. A developer who needs the
verification link sets `PYN_EMAIL_LOG_BODIES=true`: bodies are then logged in full and the server warns at startup. The server logs a warning at startup when registration is open and verification
is off, or on but nothing is delivered.

### Contract for clients

| Route | Notes |
| --- | --- |
| `GET /v1/registration` | `{registration, email_verification, approval}`; the last two are true only for `open` servers that use them |
| `POST /v1/register` | `{username, password, email?, invite?}` gives `201 {user, status}`; `status` is `active`, `pending_verification` or `pending_approval` |
| `POST /v1/register/verify` | `{token}` gives `200 {user, status}`; `400 invalid_verification` for an unknown, used or expired link |
| `POST /v1/register/resend` | `{email}` gives `202`, always |
| `POST /v1/login`, `POST /v1/session` | `403` `email_not_verified`, `approval_pending` or `account_disabled` after a right password |
| `GET /v1/me` | `{user, admin}` |
| `GET /v1/admin/users?status=&limit=` | accounts oldest first: `{user, email, email_verified, status, disabled_at, disabled_reason, admin, created_at}` |
| `POST /v1/admin/users/{user}/approve` | `{}`; the updated account |
| `POST /v1/admin/users/{user}/disable` | `{reason?}`; the updated account. `409 last_server_admin` if the account is the only active administrator |
| `POST /v1/admin/users/{user}/enable` | the updated account |
| `PUT /v1/admin/users/{user}/admin` | grants the flag; the updated account. `400 invalid_request` if the account is not active; granting again changes nothing |
| `DELETE /v1/admin/users/{user}/admin` | revokes the flag; the updated account. `409 last_server_admin` if no other active administrator would remain; revoking from a non-administrator changes nothing |
| `GET /v1/admin/audit?before=&limit=` | `account_approved`, `account_disabled`, `account_enabled`, `admin_granted`, `admin_revoked`, `owner_limits_changed`, `server_setup_completed`, `external_account_created`, `external_identity_linked` and `external_identity_unlinked` events, same page shape as a repository's |

On the command line (server administrators; each listing is an aligned table with the name last):

| Command | Does |
| --- | --- |
| `pyn admin user list [--status S] [--limit N]` | columns `STATUS ROLE CREATED EMAIL USER` |
| `pyn admin user approve <user>`, `disable <user> [--reason R]`, `enable <user>` | the account routes above |
| `pyn admin grant <user>`, `pyn admin revoke <user>` | grant or revoke the administrator flag |
| `pyn admin service-credential create <name> --scopes manage_accounts,manage_organizations,manage_limits` | prints the secret alone on stdout; it is shown once |
| `pyn admin service-credential list`, `revoke <name>` | columns `STATE CREATED LAST USED BY SCOPES NAME`; revoked ones are listed |
| `pyn admin limits list`, `show <owner>`, `set <owner> [--repos N] [--members N] [--storage SIZE]` | optional [limits](limits.md); `default` drops an owner's own value |
| `pyn admin org create <name> --owner <user>`, `pyn admin org delete <name> [--yes]` | on behalf of an owner; delete asks to type the name |

Automation signs in with `--token` or `PYN_TOKEN` set to a service credential's secret; it is limited to its scopes. A `--token`
value is visible in the process list, so prefer `PYN_TOKEN` (the same goes for `pyn setup --setup-token` and `PYN_SETUP_TOKEN`).

The admin routes answer `403 server_admin_required` to anyone else and `404 user_not_found` for an unknown account.
`pyn register --email <address>` signs up on a server that verifies.

## Tokens

A token belongs to a user. It carries a chosen subset of permissions (never more than the user holds in each repository
it names), an optional expiry, and the repositories it is valid for (`--repos owner/name`, at least one; the sign-in
tokens from `pyn login` are valid everywhere). It is shown once when created; only a hash is stored. A token
authenticates with the intersection of its permissions and its owner's current role in the repository the request is
for, so demoting a user narrows their tokens, and a revoked token stops working at once. A token used on a repository it
was not made for is told the repository does not exist. A token limited to specific repositories cannot create, rename or
delete repositories. Tokens are meant for automation and CI; SSH keys and [OpenID Connect](external-sign-in.md) are for signing
people in, and resolve to the same users and permissions.

```bash
pyn token create ci --permissions read,checkin --repos alice/game --expires-days 30
pyn token list
pyn token revoke <id>
```

## Error responses

`storage` and `history_corrupt` errors answer `500` with a generic message and their stable `code`; the detail is written to
the server log with the method and path of the request. Every other error keeps its message.

## Full-scope credentials

Account management is limited to a full-scope credential: a web session, or a token that names no repository and holds every
permission (the sign-in tokens from `pyn login`). It covers SSH keys (add, list, remove), tokens (create, list, revoke), the
password, and linking or unlinking an external identity. A narrower token (read-only, limited to repositories, or with fewer
permissions) is refused with `403 forbidden`; it keeps working for its own scope. A token credential cannot create a token that
outlives it: the new token's expiry is capped at its own, and it never holds more permissions or repositories than the creator.
A web session may create any token its owner could.

The organization-wide reads need the same credential, because they expose data beyond one repository: the organization list
(`GET /v1/orgs`, `GET /v1/me/orgs`), members, audit log, teams and the repository-creation policy. A narrower token or a service
credential is refused (`403`). A team's `repos` hold only repositories the caller could open (public ones, or any in which they hold a
role), so a member never learns the name of a private repository they cannot see.

## Service credentials

A service credential lets a trusted external service administer a server without being a person. A server
administrator creates it with a unique name and a list of scopes; it has no account, no password, no tokens and no
repository roles, so it cannot sign in, appears in no member list and cannot read or change repository content. Its
secret (`pyns_<id>_<secret>`) is shown once and only a hash is stored. Revoking it ends it at once; the revoked record
stays listed and its name is never reused.

| Scope | Allows |
| --- | --- |
| `manage_accounts` | list, approve, disable and enable accounts (`/v1/admin/users*`) |
| `manage_organizations` | create an organization for an existing user and delete an empty one (`/v1/admin/orgs*`) |
| `manage_limits` | set per-owner [limits](limits.md) and read any owner's limits and usage (`/v1/admin/limits`, `/v1/admin/owners/{owner}/limits`, `/v1/owners/{owner}/limits`, `/v1/owners/{owner}/usage`) |

Scopes are only ever added, and a credential holds exactly the scopes it was given. Server administrators pass every
scope check. A credential is accepted only on the routes its scopes cover (`/v1/admin/*`, and the owner limits and usage reads); elsewhere it gets
`403 service_credential_not_allowed`. It can neither manage service credentials nor read the server log
(`403 server_admin_required`).

Creating and revoking a credential, and every action taken with one, go to the [server audit log](#protecting-open-registration)
with the actor `@service:<name>`; organization actions also appear in that organization's log.

| Route | Notes |
| --- | --- |
| `POST /v1/admin/service-credentials` | `{name, scopes}` gives `201 {secret, info}`; `400 invalid_request` for a bad name, no scope or an unknown scope; `409 service_credential_exists` |
| `GET /v1/admin/service-credentials` | `[{name, scopes, created_by, created_at, revoked_at, last_used_at}]`, oldest first |
| `DELETE /v1/admin/service-credentials/{name}` | `204`; `404 service_credential_not_found` |
| `POST /v1/admin/orgs` | `{name, owner}` gives `201`; the owner must be an existing active user (`404 user_not_found`); `409 user_exists` |
| `DELETE /v1/admin/orgs/{org}` | `204`; `404 org_not_found`, `409 org_not_empty` |

A credential missing the scope gets `403 service_scope_required`; a revoked or unknown one gets `401 unauthenticated`.

## First-run setup

A new server is uninitialised. Until setup completes it serves only `GET /v1/setup`, `POST /v1/setup` and `GET /healthz`;
every other route answers `503 not_initialised`. Setup creates the first administrator and records the essentials, and it
can never run again (`409 already_initialised`).

To stop whoever reaches a fresh server first from claiming it, setup needs a one-time **setup token**. On first start the server prints
one to standard error in a delimited block (never through the log), or an operator sets `PYN_SETUP_TOKEN` (at least 16 characters, no
whitespace) ahead of time for unattended installs. The token is compared in constant time, wrong tries are rate limited
per client and for the whole server (`429 too_many_attempts`), and it is discarded when setup completes, after which it is never printed again. Attempts are counted before the comparison and a right token gives its count back. A server that is
already set up ignores `PYN_SETUP_TOKEN` and warns about it. With several server copies on one database, set the same
`PYN_SETUP_TOKEN` on all of them; otherwise each prints its own token and any one of them completes setup. Server name and
public address are re-read from the database every minute, so a copy picks up changes made through another.

| Route | Notes |
| --- | --- |
| `GET /v1/setup` | `{initialised, server_name?, public_url, registration}`; no secrets. Before setup `public_url` and `registration` are the configured defaults |
| `POST /v1/setup` | `{token, username, password, email?, server_name?, public_url?, registration}` gives `201 {user}`; `403 invalid_setup_token`, `400 invalid_request`, `409 already_initialised`, `429 too_many_attempts` |

`registration` is `open`, `invite` or `closed`. What setup records replaces `PYN_REGISTRATION` and `PYN_PUBLIC_URL`; the
other registration settings (`PYN_EMAIL_VERIFICATION`, `PYN_REQUIRE_APPROVAL`) stay environment settings. The
administrator signs in with the password it set (`pyn login`, or on the web) and creates repositories like anyone else.
Setup is written to the [server audit log](#protecting-open-registration) as `server_setup_completed` with the
administrator as actor. A server created before setup existed counts as set up if it already has accounts.

On the command line: `pyn setup <username> [--setup-token T] [--email E] [--server-name N] [--public-url U] [--registration
open|invite|closed] [--password-stdin]`. The token also comes from `PYN_SETUP_TOKEN`, or is asked for (hidden) on a terminal; the
password is asked for twice, or read from standard input. `--registration` defaults to what the server reports. Setup does not
sign the administrator in: run `pyn login <username>` next. On a server that is already set up it stops before asking anything.

`make dev` starts a server with a fixed demo `PYN_SETUP_TOKEN`, and the demo script completes setup with it before
seeding; see the [README](../../README.md#run-it).

`PYN_DEV_AUTH=true` is a test-only escape hatch: it lets any request name itself with an `X-Pyn-User` header and grants it
every permission. Never set it on a server others can reach.

## Audit

Organization events (created, deleted, members added, removed or changed, teams and their members, repository-creation policy changes) are kept in a separate [organization log](repositories.md#organizations) for its
owners (`pyn org audit <org>`). Adding a member (by an admin, by registration or as the creator of a repository), changing a member's role, changing what a
role grants, giving a team a role or taking it away, creating or revoking a token or an invitation (never its code), and creating, changing or deleting the repository itself are recorded in that
repository's audit log with the actor and what changed, and are visible to `view_audit`. Server-wide account actions and service credential changes go to
the [server audit log](#protecting-open-registration). A token's events go to the log of
each repository it is limited to. Token secrets are never recorded, and neither are sign-in sessions.
