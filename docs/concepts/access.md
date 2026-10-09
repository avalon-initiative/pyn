# Access: roles, permissions and tokens

Every request is checked against a permission. Health, the OpenAPI document and registration are open, and so is reading
a [public repository](repositories.md#visibility); everything else needs credentials. Accounts belong to the server, but roles, permissions, tokens' reach and invitations are per
[repository](repositories.md): being an `admin` of `alice/game` says nothing about `bob/tools`.

## Permissions and roles

| Permission | Allows |
| --- | --- |
| `read` | list files, folders and locks, see the repository summary, fetch any revision, see history |
| `lock` | check out and release exclusive files |
| `checkin` | upload content and check in |
| `restore` | make an older revision the head again |
| `force_unlock` | release someone else's lock |
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

A repository owner can change what any role grants. The `admin` role always keeps `manage_users` and `manage_roles`, and
nobody can grant a role containing a permission they do not hold themselves.

## Accounts and signing in

People have accounts: a unique user name (2 to 39 lowercase letters, digits, `-` or `_`) and a password of at least ten
characters, stored only as a salted argon2id hash. `pyn login <name>` checks the password and saves an expiring session
for that server in your user configuration directory (`~/.config/pyn/credentials.toml`, readable only by you), the way a git
credential helper would; the CLI uses it when no token is given, and `pyn logout` ends the session and removes it. A
session is a token that reaches every repository you belong to, with your role there as its limit, and it lasts 30 days by
default (`PYN_SESSION_DAYS`). Signing in needs no role anywhere: a new account with no repository can sign in and create its own.

## Web sessions

The web signs in with `POST /v1/session` (user name and password, throttled like `pyn login`). The server keeps the session
(only a hash of its cookie value is stored) and sets an `HttpOnly`, `SameSite=Lax` cookie, `pyn_session`, that page scripts
cannot read; it is also `Secure` when the request arrived over HTTPS (`X-Forwarded-Proto: https` from a TLS-terminating
proxy). A session lasts `PYN_SESSION_DAYS` (30 by default) and resolves to the user's current role in the repository being
used on every request, so a role change applies at once. `GET /v1/session` returns the signed-in user and a CSRF token
(what the user may do in one repository is `GET /v1/repos/{owner}/{name}/me`); `DELETE /v1/session` signs out and removes
the session on the server.

A request authenticated by the cookie alone must carry the CSRF token in an `X-Pyn-CSRF` header on every POST, PUT and
DELETE, or the server answers 403 `csrf_failed`. Requests with a bearer token never use the cookie and need no CSRF token.
The account pages (SSH keys, personal access tokens, password) are the same endpoints used with a session.

Five wrong passwords for a user name lock that name out of signing in for 15 minutes, and the error never says whether the
name or the password was wrong. `pyn password` changes your own password.

## SSH keys

SSH keys are how people sign in from the command line and, when the SSH transport arrives, how clone and push authenticate,
as with git. Link a public key to your account with `pyn key add` (it uses `~/.ssh/id_ed25519.pub`, `id_ecdsa.pub` or
`id_rsa.pub` if you do not name a file), see them with `pyn key list`, and remove one with `pyn key remove <id>`.

As on GitHub, a key belongs to exactly one account: its fingerprint, the same `SHA256:...` that `ssh-keygen -l` prints,
is unique across the server, and linking a key that another account has is refused. Ed25519, ECDSA, security keys and RSA
of at least 2048 bits are accepted; DSA and small RSA keys are not. Someone with `manage_users` in a repository can list and
remove the keys and tokens of its members. *Provisional:* until server administrators exist, that is the only way to manage
another person's account.

## Joining a server

The person running the server chooses how people join with `PYN_REGISTRATION`:

| Mode | Who can create an account |
| --- | --- |
| `invite` (default) | anyone with an invitation from a member who has `manage_users` in a repository |
| `open` | anyone; the account has no role in any repository (it can read public ones) until someone adds it. Meant for a public server, and not recommended on the internet until sign-up protection exists |
| `closed` | nobody on their own; someone with `manage_users` adds people with `pyn user add` |

In every mode, someone with `manage_users` in a repository can add people to it by hand. An invitation is a one-time code
(`pyn invite create --role writer`) for one repository that carries the role the new person will get there and an expiry; it
is shown once, can be revoked only in the repository it belongs to, and cannot grant a role above the level of the person
creating it. The new person runs `pyn register <name> --invite <code>` and gets that role in that repository.

## Tokens

A token belongs to a user. It carries a chosen subset of permissions (never more than the user holds in each repository
it names), an optional expiry, and the repositories it is valid for (`--repos owner/name`, at least one; the sign-in
tokens from `pyn login` are valid everywhere). It is shown once when created; only a hash is stored. A token
authenticates with the intersection of its permissions and its owner's current role in the repository the request is
for, so demoting a user narrows their tokens, and a revoked token stops working at once. A token used on a repository it
was not made for is told the repository does not exist. A token limited to specific repositories cannot create, rename or
delete repositories. Tokens are meant for automation and CI; SSH keys and OIDC are planned for signing
people in, and resolve to the same users and permissions.

```bash
pyn token create ci --permissions read,checkin --repos alice/game --expires-days 30
pyn token list
pyn token revoke <id>
```

## First administrator and development

Start the server with `PYN_BOOTSTRAP_ADMIN=<name>` to create that account; the server prints a token for it once at
startup; add `PYN_BOOTSTRAP_PASSWORD=<password>` to sign in with `pyn login` or on the web. The account owns nothing yet: it
creates repositories like anyone else (`pyn repo create`) and is the admin of those. `make dev` runs this setup with open
registration and a seeded demo; see the [README](../../README.md#run-it).

`PYN_DEV_AUTH=true` is a test-only escape hatch: it lets any request name itself with an `X-Pyn-User` header and grants it
every permission. Never set it on a server others can reach.

## Audit

Adding a member (by an admin, by registration or as the creator of a repository), changing a member's role, changing what a
role grants, creating or revoking a token, and creating, changing or deleting the repository itself are recorded in that
repository's audit log with the actor and what changed, and are visible to `view_audit`. A token's events go to the log of
each repository it is limited to. Token secrets are never recorded, and neither are sign-in sessions.
