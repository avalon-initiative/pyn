# Access: roles, permissions and tokens

Every request is authenticated and checked against a permission. Health and the OpenAPI document are the only open
endpoints.

## Permissions and roles

| Permission | Allows |
| --- | --- |
| `read` | list files and locks, fetch any revision, see history |
| `lock` | check out and release exclusive files |
| `checkin` | upload content and check in |
| `restore` | make an older revision the head again |
| `force_unlock` | release someone else's lock |
| `edit_policy` | change `.pyn/pyn.toml` |
| `manage_users` | add members, assign roles, list or revoke other users' tokens |
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
session is a token limited to your role, and it lasts 30 days by default (`PYN_SESSION_DAYS`).

Five wrong passwords for a user name lock that name out of signing in for 15 minutes, and the error never says whether the
name or the password was wrong. `pyn password` changes your own password.

## SSH keys

SSH keys are how people sign in from the command line and, when the SSH transport arrives, how clone and push authenticate,
as with git. Link a public key to your account with `pyn key add` (it uses `~/.ssh/id_ed25519.pub`, `id_ecdsa.pub` or
`id_rsa.pub` if you do not name a file), see them with `pyn key list`, and remove one with `pyn key remove <id>`.

As on GitHub, a key belongs to exactly one account: its fingerprint, the same `SHA256:...` that `ssh-keygen -l` prints,
is unique across the server, and linking a key that another account has is refused. Ed25519, ECDSA, security keys and RSA
of at least 2048 bits are accepted; DSA and small RSA keys are not. Administrators can list and remove other people's keys.

## Joining a server

The person running the server chooses how people join with `PYN_REGISTRATION`:

| Mode | Who can create an account |
| --- | --- |
| `invite` (default) | anyone with an invitation from a member who has `manage_users` |
| `open` | anyone, as a `reader` (or `PYN_DEFAULT_ROLE`); meant for a public server, and not recommended on the internet until sign-up protection exists |
| `closed` | nobody on their own; an administrator adds people with `pyn user add` |

Administrators can add people by hand in every mode. An invitation is a one-time code (`pyn invite create --role writer`)
that carries the role the new person will get and an expiry; it is shown once, can be revoked, and cannot grant a role
above the level of the person creating it. The new person runs `pyn register <name> --invite <code>`.

## Tokens

A token belongs to a user. It carries a chosen subset of permissions (never more than the user holds), an optional
expiry, and the repository it is valid for. It is shown once when created; only a hash is stored. A token authenticates
with the intersection of its permissions and its owner's current role, so demoting a user narrows their tokens, and a
revoked token stops working at once. Tokens are meant for automation and CI; SSH keys and OIDC are planned for signing
people in, and resolve to the same users and permissions.

```bash
pyn token create ci --permissions read,checkin --expires-days 30
pyn token list
pyn token revoke <id>
```

## First administrator and development

Start the server with `PYN_BOOTSTRAP_ADMIN=<name>` to create that user as an admin; the server prints an admin token
once at startup. For local work only, `PYN_DEV_AUTH=true` lets any request name itself with an `X-Pyn-User` header and
grants it every permission.

## Audit

Adding a member (by an admin or by registration), changing a member's role, changing what a role grants, and creating
or revoking a token are recorded in the audit log with the actor and what changed, and are visible to `view_audit`.
Token secrets are never recorded, and neither are sign-in sessions.
