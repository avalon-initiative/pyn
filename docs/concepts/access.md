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

## Exclusive paths and the lock

An exclusive path is published only by an account that holds a live lock on it (the `lock` permission takes one). Another
holder always blocks a push, and so does no lock at all, even when the push would be a clean fast-forward; the refusal is
`lock_held` or `lock_required`. The CLI is a convenience on top of that rule: `pyn add`, `pyn commit` and `pyn edit` work
on an exclusive file without the lock, so work can happen offline, `pyn status` marks what will be refused, and `pyn push`
checks the locks before sending and hints at `pyn lock <path>`. The server's own refusals stay the guarantee ([workspace](workspace.md#working-in-a-workspace)).

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
for that server in your user configuration directory (`~/.config/pyn/credentials.toml`, readable only by you: the directory is `0700`, the file `0600`, written atomically under an advisory lock file, `.credentials.lock`, so concurrent commands lose no entry), the way a git
credential helper would; the CLI uses it when no token is given, and `pyn logout` ends the session and removes it. A
session is a token that reaches every repository you belong to, with your role there as its limit, and it lasts 30 days by
default (`PYN_SESSION_DAYS`). Signing in needs no role anywhere: a new account with no repository can sign in and create its own.

## Deleting an account

An account can be deleted three ways (decisions #975 and #976). All three end in one **purge**.

**Self-service.** `pyn account delete` (`POST /v1/account/deletion {password}`) asks for the password again, then disables the account
and schedules its purge after a **recovery window** of 30 days (`PYN_ACCOUNT_DELETION_WINDOW_DAYS`); the answer carries `purge_at`. Every status answer (including `GET /v1/account/deletion` before anything is scheduled) carries `window_days`, the effective recovery window, so a regular user sees it without admin settings.
An account with no password (external sign-in only) is deleted from a signed-in web session, which needs no password. While the
window runs the account is *pending deletion*:

- Signing in still works (`POST /v1/login`, `POST /v1/session`, external sign-in), so the owner can come back.
- The credentials of a pending account reach only `GET /v1/account/deletion` (status: `{user, pending, scheduled_at, purge_at, window_days}`),
  `POST /v1/account/deletion/restore` and `GET /v1/session` (so a web app can load the session and its CSRF token). Every other route,
  Git/SSH access included, answers `403 account_pending_deletion` with the deadline in the message.
- `pyn account restore` (`POST /v1/account/deletion/restore`) cancels the deletion and enables the account again; it is refused once
  `purge_at` has passed. `pyn account status` shows the deadline.
- An administrator's disable is not a pending deletion: that account cannot sign in (`account_disabled`) and only an administrator
  enables it.

**Administrators** delete a *disabled* account (`409 account_not_disabled` otherwise) with a reason that goes to the audit log:
`pyn admin user delete <user> --reason <text>` (`POST /v1/admin/users/{user}/delete {reason, immediately?}`) schedules the purge after
the same window and `--now` (`immediately`) purges at once. An administrator cannot delete their own account this way, and
`pyn admin user enable <user>` cancels a scheduled deletion. A service credential with `manage_accounts` may do the same.

An account that owns repositories, or is the only owner of an organization, cannot be deleted (`409 account_owns_resources`). The error
body carries `blockers: {repositories: ["owner/name"], organizations: ["org"]}` next to the message. The account list at
`GET /v1/admin/users` gives each disabled account's `purge_at`, and `GET /v1/admin/settings` the window and retention in days, plus how many administrators are active.

**Automatic deletion.** An account disabled for `PYN_DISABLED_ACCOUNT_RETENTION_DAYS` days (default 90, `0` turns it off) is purged
by the server. A background task runs at start and then hourly; it keeps no state of its own (schedules and disable times are in the
database), so a restart loses nothing, a pass can run twice, and an account it cannot purge is logged and tried again on the next pass.
The same pass purges accounts whose recovery window ended, drops a schedule left behind by an account that is enabled again, and
settles name reservations. Automatic purges are audited under the actor `@account-deletion`.

**What a purge does.** It removes the account's sessions, tokens, SSH keys, linked identities, invitations (those it created and those
addressed to it), memberships, team seats and per-user creation rules, and releases its email address. The server refuses (`409`) while:

- the account owns repositories, or is the only owner of an organization: `account_owns_resources`, the message lists them; transfer
  or delete each first (an organization with another owner does not block, the account just leaves it);
- the account is the last active administrator (`last_server_admin`; an administrator account is also kept while no other active
  administrator exists);
- the account is a frozen owner (`account_frozen`): unfreeze it first.

Self-service deletion is refused for the same reasons when requested. A refusal changes nothing.

**Names and history.** History and audit entries keep the user name they were written with; there is no tombstone name, and authors are
identified by account id internally. So a purged name is **reserved** and nobody can register, create, or be renamed to it
(`409 name_reserved`; registration, administrator creation, organization creation and the name picked for an external sign-in all check it):

- for 90 days after the purge, whatever the name appears in;
- after that, for as long as it appears as the author of a revision or the actor of an audit entry (the reservation then becomes
  permanent); a name found in neither is released, by the first check that needs it or by the background task.

Because deleting an account is itself audited under the account's name (self-service) the name of anyone who deleted themself, or ever
acted on the server, stays reserved; an account an administrator removed that never acted is free again after 90 days.

Webhook events: `account.deletion_scheduled`, `account.deletion_restored` and `account.deleted` ([events](events.md)). Audit actions
(`GET /v1/admin/audit`): `account_deletion_scheduled`, `account_deletion_restored`, `account_deleted`.
## Your email addresses

An account can hold several email addresses, and only its owner ever sees them (no other user, no organization, no public
profile; server administrators see an account's primary address in the admin account list, as before). One is the **primary**:
the address the server uses for the account. The rules:

- **Adding** an address (`POST /v1/me/emails`) leaves it **pending**: the server sends the same `{PYN_PUBLIC_URL}/verify-email?token=...`
  link as at sign-up, valid once for 24 hours, and the web page behind it calls the same `POST /v1/register/verify {token}`
  (the answer is the account's status). Delivery goes through the same sender as sign-up, so today the link reaches the
  log only (see [email delivery](#protecting-open-registration)). A pending address is not usable: it cannot be the primary and
  is never written anywhere. `POST /v1/me/emails/{address}/resend` mails a fresh link (`404 email_not_found` once the address is
  verified); it counts against the same per-recipient mail limit as sign-up.
- **Unique across accounts**, ignoring case (addresses are lowercased). An address another account holds, or has pending, is
  refused with `409 email_unavailable`, the one generic answer, so it never says whose address it is or whether it is
  verified. An address already on your own account is `409 email_refused`. An account holds at most 10.
- **The primary must be verified** (`PUT /v1/me/emails/{address}/primary`, `409 email_refused` otherwise). The old primary stays on
  the account as an additional verified address. The first address an account verifies becomes its primary if it has none.
- **Removing** (`DELETE /v1/me/emails/{address}`): removing the primary promotes the oldest other verified address. Where the
  server requires a verified address to sign up (open registration with `PYN_EMAIL_VERIFICATION`), the last verified address
  cannot be removed (`409 email_refused`); elsewhere it can.
- Adding, verifying, choosing and removing an address are each recorded in the server audit log
  (`email_added`, `email_verified`, `email_primary_changed`, `email_removed`).

On the command line: `pyn account email list` (columns `PRIMARY STATUS ADDED ADDRESS`), `add <address>`, `primary <address>`,
`remove <address>`, `resend <address>`.

## Changing your username

`PUT /v1/me/username {username, password}` renames your account (`pyn account rename <new-name> [--yes]`), as on GitHub.
It needs the current password again (an account with none can confirm from a signed-in web session), counted like the
password check of `PUT /v1/me/password`. The name follows the [username rules](repositories.md#addresses-and-namespaces)
and must be free: a user or organization holding it gives `409 user_exists`, and an organization name can never be taken
by renaming. Renaming organizations is not supported. An account that is [frozen](limits.md#freezing-an-owner) cannot rename
(`403 owner_frozen`).

**Warning, shown by the CLI and the web app before the rename is confirmed.** The old name is not reserved. Anyone may take it
right away. Your repositories move to the new name and each old `old-name/repo` address keeps working, but a redirect stops,
repository by repository, as soon as someone under the old name creates a repository with the same name: that live
repository always wins over the redirect, and links and remotes using the old name then reach the new holder's repository.
Update remotes and links after renaming. The old profile address is not redirected, and neither are `@mentions` of the old name.

What follows the rename: the account id (which is how integrations should key accounts), sessions, tokens (every token
stays valid), SSH keys, memberships and roles in repositories, organizations and teams, held locks, repository ownership,
and pending invitations. [Redirects](repositories.md#renamed-owners-and-address-redirects) are kept per repository.
Revisions and commits keep the author name they were written with, because their hashes cover it; every history response
carries the account's name now: `author_current` on revisions, commits and stranded versions (history, tree and folder
`last_change`, push, fetch, merge and policy responses) and `actor_current` on activity and audit entries, shown by clients so one
person appears under one name across history. The CLI prints the current name and, when it differs, the written one
(`alicia (written as alice)`) in `pyn log`, `pyn show`, `pyn ls`, `pyn summary` and the audit and stranded listings. Locks show
their holder's current name. The same responses carry the holder's [public identity](profiles.md#public-identity-in-lists). For an account that was deleted there is no current name and the last name stays. Audit logs keep
names as written. The server log records a rename as `account_renamed` with actor the old name and detail `old renamed to new`;
each repository that moved gets the same action in its own log, under the new name, with detail `owner old renamed to new`. The
`account.renamed` [event](events.md#accountrenamed) announces it to webhooks.

A name check for a new account, an organization or a rename goes through one server function (`ensure_name_free`, with
`name_is_free` for renames), so the reservation of a deleted account's name applies to all of them.

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
The account pages (SSH keys, personal access tokens, password, [profile](profiles.md)) are the same endpoints used with a session.

Failed sign-ins are throttled twice: five wrong passwords for a user name (which need not exist) lock that name out for
15 minutes, and 30 failures from one client address lock that address out for the same time, whatever names it tries.
Every attempt is counted before the password is checked (the address first, so a refused address creates no per-name counter), so attempts past a limit are refused without any password work,
even when they arrive together. A success clears the name's count and gives the address's count back. The window is fixed:
attempts made while locked out do not extend it. User names and addresses longer than 64 characters are keyed by their
SHA-256 digest. The error never says whether the name or the password was wrong, and
a user name that does not exist costs the same work as one that does. Only a right password learns that the account cannot
sign in yet (`email_not_verified`, `approval_pending`, `account_disabled`, all 403). `pyn password` changes your own password and ends your other web sessions (the one making the change stays signed in) and the sign-in tokens that `pyn login` issued, including the one the CLI itself was using: the CLI then says so, forgets the saved sign-in and tells you to run `pyn login` again; tokens you created and SSH keys keep working. An operator's password reset ends all sessions and sign-in tokens too. The current password is always required when the account has one, and wrong guesses are limited like sign-in: five per account in 15 minutes (`PYN_RATE_SIGN_IN_ACCOUNT`), counted before the check, cleared by a right password, then `429 too_many_attempts`; an account with no password yet (created through an external provider) can set its first only from a signed-in web session.

A server can also sign people in through an external OpenID Connect provider, and turn password sign-in off (server
administrators keep it); see [signing in with an external provider](external-sign-in.md).

An account can add a second step to password sign-in; see [two-factor authentication](#two-factor-authentication).

## Two-factor authentication

An account can require a second factor at password sign-in (decision #1097). Today that is an authenticator app (a time-based
one-time password, RFC 6238: SHA-1, six digits, 30 seconds), a [passkey or security key](#passkeys-and-security-keys) (WebAuthn),
and single-use recovery codes. It is opt-in until a [server administrator or an organization owner requires it](#requiring-two-factor-authentication). Nothing here needs email or a paid service.

**Turning it on.** `POST /v1/me/two-factor/totp {label?, password}` starts an enrolment and returns the seed (`secret`, in
base32, and an `otpauth://` `uri` for a QR code) once; an account that has a password must send it. Nothing changes until
`POST /v1/me/two-factor/totp/{id}/confirm {code}` proves the app shows the right code. Starting again replaces an unconfirmed
enrolment. The account's first confirmed factor also returns **ten recovery codes**, shown only then: they are stored as
hashes, each works once, and `POST /v1/me/two-factor/recovery-codes` replaces all ten with a new set (the old ones stop
working). An account may hold up to five factors, each with its own name. `GET /v1/me/two-factor` lists the confirmed factors
and how many recovery codes are left, never the seed or a code. `DELETE /v1/me/two-factor/{id}` removes a factor; removing the
last one turns two-factor authentication off and drops the recovery codes.

**Signing in.** `POST /v1/login` and `POST /v1/session` take an optional `code`: the app's six digits, or a recovery code
(`xxxxx-xxxxx`, case and the dash do not matter). For an account with a factor, a right password without a code answers `401
second_factor_required` (a wrong password still gets the one generic `401`, so this reveals nothing to someone without the
password), and a wrong code `401 second_factor_invalid`. A client asks for the code and sends the request again. An app code
works once: the server remembers the latest time step it accepted and accepts one step of clock drift either way. Signing in
with a recovery code is recorded in the server audit log. [External sign-in](external-sign-in.md) brings its own second
factor and is not challenged, and neither are tokens or SSH keys: they are credentials for automation, except that a sign-in token (what `pyn login` makes) counts as a sign-in and is held to a [requirement](#requiring-two-factor-authentication) like a session.

`pyn login` asks for the code after the password when the server answers `second_factor_required`, and asks again (up to three codes) after `second_factor_invalid`. The prompt takes one line from standard input, so a script that already pipes the password with `--password-stdin` sends the code as the next line; there is no code flag or environment variable, as a code would show in the process list, and unattended use should sign in with a token instead. A `429 too_many_attempts` is reported with its `Retry-After` wait. Passkeys are a browser ceremony and the CLI does not run one: an account whose only second factor is a passkey signs in on the CLI with a recovery code.

**Fresh check (sudo mode).** An account that has a factor must pass it again, recently, before a sensitive action:
changing the password or the factors (adding, removing, new recovery codes), creating or revoking a token, adding or removing
an SSH key, and scheduling the account's deletion. (Administrator access sessions will use the same check.) `POST
/v1/me/two-factor/verify {code}` takes an app code or a recovery code and keeps those actions open for 15 minutes on the
credential that sent it, a session or a sign-in token (the window belongs to that credential, not to the account), and answers
`{fresh_until}`. A sign-in that passed the second factor starts with the window open. Without it the action answers `403
fresh_check_required` and changes nothing. The CLI handles that answer for `pyn token create` and `revoke`, `pyn key add` and `remove`, `pyn password` and `pyn account delete`: it asks for a code (one line of standard input), calls `verify` and repeats the command once. An account with no factor is never asked. A fine-grained token is not challenged
(decision #1097 leaves automation alone), but it can only be created by a credential that passed the check, and it never
reaches the factor routes, which are [full-scope](#full-scope-credentials) only like the password.

**Limits on attempts.** Every code check, at sign-in, at `confirm` and at `verify`, counts against the account, whichever factor
or recovery code it targets, and is counted before it is checked. The first four wrong attempts wait for nothing; from the
fifth on the next attempt waits 30 seconds, then 60, doubling up to an hour (`429 too_many_attempts` with `Retry-After`). The
delay locks the attempt, not the account: the password, tokens and SSH keys keep working, and a right code during the delay
is refused too. A success clears the count, and so does 24 hours without attempts. This is on top of the password limits
under [accounts and signing in](#accounts-and-signing-in).

**Seeds at rest.** A seed has to be readable to check a code, so it cannot be hashed. With `PYN_SECRET_KEY` set it is stored
encrypted with AES-256-GCM (the same key as webhook secrets, with its own context), so a copy of the database alone does not
leak the seeds; unset, it is stored as plaintext and the server warns at start. `pyn-server rotate-secret-key` re-encrypts the
seeds with the webhook secrets. A seed encrypted under a key the server no longer has cannot check codes (a recovery code still
signs in): see [Lost secret key](../deployment.md#lost-secret-key).

**Recovery without email.** A recovery code signs in; so does any other registered factor. `pyn-server recover-admin` on the host
also removes the account's factors, which is how an administrator who lost both the device and the codes gets back in. Resetting
another account's factors as an administrator is a [reset](#requiring-two-factor-authentication), which does not apply to an administrator's account.

Audit actions in the server log (`GET /v1/admin/audit`): `two_factor_added`, `two_factor_removed`,
`recovery_codes_regenerated`, `recovery_code_used`, `two_factor_reset`, `passkey_added`, `passkey_removed`, `passkey_clone_suspected`, `server_two_factor_requirement_changed`; an organization's requirement changes are
`org_two_factor_requirement_changed` in its [organization log](repositories.md#organizations).

| Route | Notes |
| --- | --- |
| `GET /v1/me/two-factor` | `{enabled, factors: [{id, kind, label, created_at}], passkeys: [Passkey], passkeys_available, recovery_codes_left, required?}`; `enabled` is true with a factor or a passkey; `required` is the [requirement](#requiring-two-factor-authentication) still unmet |
| `POST /v1/me/two-factor/totp` | `{label?, password?}` gives `201 {id, secret, uri}`; `401` for a missing or wrong password, `403 fresh_check_required` when a factor exists |
| `POST /v1/me/two-factor/totp/{id}/confirm` | `{code}` gives `{factor, recovery_codes?, fresh_until}`; `recovery_codes` only with the first factor. `401 second_factor_invalid`, `404 factor_not_found`, `429` |
| `DELETE /v1/me/two-factor/{id}` | `204`; `403 fresh_check_required`, `404 factor_not_found` |
| `POST /v1/me/two-factor/recovery-codes` | `{recovery_codes}`, ten new ones shown once; `400` when two-factor authentication is off, `403 fresh_check_required` |
| `POST /v1/me/two-factor/verify` | `{code}` gives `{fresh_until}`; `400` when it is off, `401 second_factor_invalid`, `429` |

### Passkeys and security keys

A passkey or security key (WebAuthn, decision #1127, built in house in `pyn-webauthn`: no OpenSSL, `none` attestation only,
ES256 and EdDSA keys) is a second factor and, because the authenticator checks the person (PIN or biometrics), a sign-in that
needs no password. An account may hold up to ten, each with a name, the time it was added and last used, the transports the
browser reported, the signature counter and whether the passkey is synced (`backup_eligible`, `backup_state`). A passkey counts
wherever a second factor counts: `has_second_factor` is the one place the [requirements](#requiring-two-factor-authentication),
the fresh check and the recovery codes ask, so a passkey meets a requirement and the account's first one (like its first
authenticator app) brings the ten recovery codes.

**The relying party is the server's public address.** WebAuthn binds a credential to a relying-party id and an origin, so the
server derives them from `PYN_PUBLIC_URL` (or the address recorded at setup): the id is its host, the allowed origin its
origin, and each `PYN_ALLOWED_ORIGINS` entry inside the id's scope (the host or a subdomain of it) is allowed too.
`PYN_WEBAUTHN_RP_ID` may widen the id to a parent domain of that host (`git.example.com` served as `example.com`). Changing the
host or the id later orphans every registered passkey, so choose it before people register. A server whose public address has
no usable host (empty, an IP address, or `http://` other than `localhost`) offers authenticator apps only: `GET /v1/capabilities`,
`GET /v1/sign-in-options` and `GET /v1/me/two-factor` report `passkeys: false` / `passkeys_available: false`, and every
ceremony route answers `501 passkeys_unavailable`. Listing, renaming and removing still work, and the passkeys an account already
holds still count as a second factor, so a changed address never turns two-factor authentication off.

**Ceremonies.** Each is two calls around one browser call, and the bodies are the WebAuthn JSON forms (`credential.toJSON()` of
the browser's result goes in `credential` as it is; binary fields are base64url). `begin` returns a `ceremony` id and `public_key`
options; `finish` takes the `ceremony` back. A challenge is random, good for one `finish` and for five minutes, and spent whatever
the outcome (`401 passkey_invalid` when it is unknown, used, expired or issued for another purpose or account). The server
asks for user verification and a discoverable credential at every ceremony, and refuses an authenticator that did not verify
the person.

**Counter.** An authenticator whose counter did not increase, while either value is non-zero, may be a copy: the sign-in or check is
refused with `passkey_invalid`, the stored counter is left as it was, the server log records `passkey_clone_suspected` and the
account gets a [notice](#requiring-two-factor-authentication) (`passkey_clone_suspected`) so it can remove the passkey. Authenticators
that keep no counter (always zero, typical of synced passkeys) are accepted.

**Removing.** `DELETE /v1/me/passkeys/{id}` needs a fresh check like removing any factor. Removing the last second factor turns
two-factor authentication off and drops the recovery codes; an account under a requirement is then held to it again once its
grace period is over. An account that has only passkeys passes the fresh check with a passkey (`verify`), with a recovery code,
or by signing in with a passkey, which opens the window. Password sign-in with only a passkey as the second factor answers
`second_factor_required` and accepts a recovery code; sign in with the passkey instead.

| Route | Notes |
| --- | --- |
| `GET /v1/me/passkeys` | `[{id, name, created_at, last_used_at, transports, sign_count, backup_eligible, backup_state}]`, oldest first; never a key or a credential id |
| `POST /v1/me/passkeys/register/begin` | `{password?}` gives `{ceremony, public_key: {rp, user, challenge, pubKeyCredParams, timeout, excludeCredentials, authenticatorSelection, attestation}, expires_at}` (`PublicKeyCredentialCreationOptionsJSON`). The password is needed while the account has no second factor, a fresh check once it has one: `401`, `403 fresh_check_required`; `400` at ten passkeys; `501 passkeys_unavailable` |
| `POST /v1/me/passkeys/register/finish` | `{ceremony, name?, credential}` gives `201 {passkey, recovery_codes?, fresh_until}`; `recovery_codes` only with the account's first second factor. `401 passkey_invalid` (also an authenticator already registered), `501` |
| `PATCH /v1/me/passkeys/{id}` | `{name}` gives the passkey; `404 passkey_not_found` |
| `DELETE /v1/me/passkeys/{id}` | `204`; `403 fresh_check_required`, `404 passkey_not_found` |
| `POST /v1/me/passkeys/verify/begin`, `.../verify/finish` | the fresh check with a passkey: `begin` gives `{ceremony, public_key}` (`PublicKeyCredentialRequestOptionsJSON`, the account's passkeys in `allowCredentials`; `400` when it has none), `finish {ceremony, credential}` gives `{fresh_until}`; `401 passkey_invalid`, `429` (the same delay as codes) |
| `POST /v1/session/passkey/begin` | open; `{username?}` gives `{ceremony, public_key, expires_at}`. With a name the account's passkeys are offered; without one (or with an unknown name) `allowCredentials` is empty and the browser offers a discoverable passkey. Same origin check as sign-in |
| `POST /v1/session/passkey/finish` | open; `{ceremony, credential}` signs in as `POST /v1/session` does (cookie and `SessionInfo`) with the fresh window open. `401 passkey_invalid`, `403` for an account that cannot sign in or a foreign origin, `429` (the client's sign-in limit), `501` |

Registration, rename and removal are on the [enrolment routes](#requiring-two-factor-authentication) (`full_scope_enrolment`);
`verify` is `full_scope`. Passkeys are not available to tokens, only full-scope credentials, like the other factors.

### Requiring two-factor authentication

A server administrator can require a second factor of every account, and an organization owner of the organization's members
(decision #1097). Both come with a **grace period** of 0 to 90 days (default 7).

| Route | Notes |
| --- | --- |
| `GET /v1/admin/two-factor`, `PUT /v1/admin/two-factor` | `{required, since?, grace_days?}`; `PUT {required, grace_days?}` sets or lifts the server-wide requirement. Administrators only, never a service credential; `403 fresh_check_required` for an administrator who has a factor and no fresh check |
| `GET /v1/orgs/{org}/two-factor`, `PUT /v1/orgs/{org}/two-factor` | the same for one organization: `org_administration` read and write, and the creator must be an owner of that organization (`403 not_org_owner` otherwise, so an owner of another organization or a plain member is refused) |
| `POST /v1/admin/users/{user}/two-factor/reset` | `204`; removes every factor and recovery code of the account. Administrators only; `400 invalid_request` for an administrator's account (use `recover-admin` on the host), `404 user_not_found` |
| `GET /v1/me/notices`, `DELETE /v1/me/notices/{id}` | the account's notices, oldest first `[{id, kind, detail, created_at}]`, and dismissing one (`404 notice_not_found`) |

**Grace and enforcement.** The grace period runs from the later of the moment the requirement was set and the account's
creation, so a new account always gets its full grace. An account with several requirements (the server's and its
organizations') is held to the earliest deadline. Setting the same requirement again changes nothing; a different grace period
starts the clock again. Until the deadline the account works as before, and the API says what is due: `GET /v1/me` carries
`two_factor_required: {by: "server"|"organization", organization?, deadline, enforced: false}` and so does `required` in `GET
/v1/me/two-factor`, so a client can nag. From the deadline `enforced` is true and an account that holds no factor can reach
only the **enrolment routes** (reach `full_scope_enrolment`): `GET /v1/me/two-factor`, `POST /v1/me/two-factor/totp`, `POST
/v1/me/two-factor/totp/{id}/confirm`, `DELETE /v1/me/two-factor/{id}` (to cancel a pending enrolment), the `/v1/me/passkeys` routes (list, register, rename, remove), `GET /v1/me/notices`, `DELETE
/v1/me/notices/{id}` and `GET`/`DELETE /v1/session`. Everything else answers `403 two_factor_enrolment_required` until it
confirms a factor; the requirement is met as soon as it holds one. Lifting the requirement, or leaving the organization, lifts the block.
An administrator is held to a server-wide requirement like anyone, so enrol before the deadline; if none can, `recover-admin`
on the host remains the way back in.

**Who is held to it.** Sessions and sign-in tokens (what `pyn login` makes), which are people signing in. Other tokens, SSH
keys and service credentials are automation and are not challenged (decision #1097): they keep working after the deadline, and
cannot be made or revoked by a blocked account because token creation is not an enrolment route.

**Reset.** An administrator resets an account that lost its device and recovery codes. The reset removes the factors and
recovery codes (sessions and tokens stay), is recorded as `two_factor_reset` in the server audit log, and leaves the account a
notice (`kind: "two_factor_reset"`, naming the administrator) that its web app shows until dismissed; there is no email. A reset
does not lift a requirement: an account that must have a factor, past its deadline, signs in and can only enrol again.


## SSH keys

Today a key can be linked, listed and removed, and nothing more: link a public key to your account with `pyn key add` (it uses
`~/.ssh/id_ed25519.pub`, `id_ecdsa.pub` or `id_rsa.pub` if you do not name a file), see them with `pyn key list`, and remove one
with `pyn key remove <id>`. Adding a key needs a fresh [second-factor](#two-factor-authentication) check when the account has a second factor. The command line signs in with
`pyn login` (a password, and a second-factor code when the account has one) or with a token.

Provisional: signing in and cloning over SSH wait for the SSH transport ([#68](https://github.com/avalon-initiative/pyn/issues/68)).
The intent is that a linked key then authenticates people at the command line and for clone and push, as with git; until it
ships, a linked key opens no session and authenticates no request.

As on GitHub, a key belongs to exactly one account: its fingerprint, the same `SHA256:...` that `ssh-keygen -l` prints,
is unique across the server, and linking a key that another account has is refused. Ed25519, ECDSA, security keys and RSA
of at least 2048 bits are accepted; DSA and small RSA keys are not. Someone with `manage_users` in a repository can list and
remove the keys and tokens of its members, except those of a server administrator or of the repository's owner (the owning
account, or an owner of the owning organization), which only a server administrator can. [Server administrators](#protecting-open-registration) approve and disable
accounts but do not gain that.

Token management for another person is bounded by the same reach. A manager lists and revokes only the `selected` tokens whose
repositories are all ones where the manager holds `manage_users`, and the repositories shown are those. Any other token
(another repository, `all` or `public` access, or a sign-in token) is left out of the listing and revoking it reads
as `404 token_not_found`; only a server administrator reaches those. A token id that does not exist, or that belongs to
someone the caller cannot manage, gets the same answer. A bearer token with an unknown id and one with a wrong secret get
the same `401`.

## Server access mode

A server is `private` or `public`; the setting is stored with the server, separate from the registration mode below.
A new server is `private` unless first-run setup (or `PYN_ACCESS` before it) says `public`, and so is every server that
existed before the setting did.

| Mode | What a signed-out caller can read |
| --- | --- |
| `private` (default) | nothing, except what a visitor needs to sign in or sign up (listed below) |
| `public` | what a signed-out caller reads on any server: public repositories (tree, files, history, fetch, Git LFS object download), public profiles and avatars, the people directory (full-scope credentials only when signed in; rate limited, see [protecting open registration](#protecting-open-registration)), organization avatars and the repository explore list |

On a `private` server every route declared `public`, `optional_sign_in`, `optional_full_scope`, `public_repo_read` or `public_lfs_read`
(see [route reach](#route-reach)) refuses a request that carries no credential: `401 unauthenticated`, except on a
repository route, which answers `404 repo_not_found` exactly as it does for a private repository, so it neither
confirms nor denies that a repository exists (a `public_lfs_read` route answers `401` in the LFS error format with `WWW-Authenticate`, so git-lfs asks for credentials). That covers anonymous clone and fetch of a public repository, profiles,
avatars, the people directory, the explore list and organization avatars. A request that carries a credential is judged as on a `public` server: a signed-in account sees what it
sees there (a public repository stays readable by any signed-in account), a bad credential is still `401`, and a
service credential is still refused where it was. Stored data and repository visibility flags are never touched by the mode, and
existing tokens keep working. LFS uploads, verify, locks and tus, and the organization lookups, need a credential on both modes; LFS download is the one LFS operation a `public` server gives a signed-out caller, from a public repository.

These stay open in both modes, because a signed-out visitor needs them to reach a server at all: `GET /healthz`,
`GET /openapi.json`, `GET /v1/capabilities`, `GET`/`POST /v1/setup`, `GET /v1/registration`,
`POST /v1/register`, `POST /v1/register/verify`, `POST /v1/register/resend`, `POST /v1/login`, `POST`/`DELETE /v1/session`,
`GET /v1/sign-in-options`, `GET /v1/oidc/authorize`, `GET /v1/oidc/callback`, and the tus `OPTIONS` preflight. Accepting an
organization or repository invitation needs a signed-in account on both modes. They carry reach `open`.

Anyone can read the mode without a credential: `GET /v1/setup` returns `access_mode`, so a client can choose its
signed-out pages. A server administrator changes it with `PUT /v1/admin/access` and `{"access_mode": "private"|"public"}`
(a full-scope administrator credential; a scoped token, a service credential or an ordinary account gets `403`). The
change applies to the next request on that server copy, and to other copies within a minute (the settings are re-read
every minute). It is recorded in the [server audit log](#protecting-open-registration) as `server_access_mode_changed`
with the actor and both modes, and in the server log at `info`. Setting the mode already in force changes nothing and
records nothing.

First-run setup takes an optional `access_mode`. When it is omitted, `PYN_ACCESS` (`private` or `public`) is used, else
`private`. After setup the stored value wins: `PYN_ACCESS` is only the default for a server that has no stored value,
so changing it on a set-up server does nothing. The web panel that warns about each direction (what becomes readable
signed out, and what stops working at once) is a client concern; the API only does what it is told.

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
`open` server it is allowed and rate limited like sign-up. Creating an account is server-wide, not an action on the
repository, so the route needs a [full-scope credential](#full-scope-credentials) as well as `manage_users`: a token limited
to repositories or to fewer permissions is refused with `403 forbidden`, even one that holds `manage_users` there and even
one of an administrator, and a service credential with `403 service_credential_not_allowed`. The administrator path
likewise needs a full-scope credential of an administrator. Adding an existing person to a repository is not account
creation and needs only `manage_users`. Giving a role (`PUT /v1/repos/{owner}/{name}/members/{user}`, `pyn member set`) never creates an
account, for any credential: the name must already be an account, or the answer is `404 user_not_found` and nothing is written.
To bring in someone new, register them (`POST .../users`, `pyn user add`) or send an invitation. An invitation is a one-time code
(`pyn invite create --role writer`) for one repository that carries the role the new person will get there and an expiry of 1 to 8760 hours; it
is shown once, can be revoked only in the repository it belongs to, and cannot grant a role above the level of the person
creating it. The new person runs `pyn register <name> --invite <code>` and gets that role in that repository.

An invitation lets someone create an account on the server even when registration is closed, so creating one is not an action
on the repository: it needs a [full-scope credential](#full-scope-credentials) as well as `manage_users`. A token limited to
repositories or to fewer permissions is refused with `403 forbidden`, even one that holds `manage_users` there and even one
of a server administrator, and a service credential with `403 service_credential_not_allowed`. Listing and revoking a
repository's invitations stay `manage_users` actions in that repository.

## Protecting open registration

An open server attracts bots and password guessing, so it layers these protections. Each is server-side; clients only
display what the server says. The mode default is `invite`.

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
| wrong current passwords in `PUT /v1/me/password`, per account | 5 per 15 minutes | `PYN_RATE_SIGN_IN_ACCOUNT` |
| second-factor code attempts, per account | the fifth attempt on starts a delay of 30 seconds that doubles up to an hour | fixed, see [two-factor authentication](#two-factor-authentication) |
| sign-up and resend attempts per client address | 10 per hour | `PYN_RATE_REGISTER_CLIENT` |
| verification emails per recipient address | 3 per hour | `PYN_RATE_MAIL_PER_EMAIL` |
| signed-out people directory requests (`GET /v1/users`) per client address | 60 per minute | `PYN_RATE_PEOPLE_CLIENT` |

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
and SSH keys stop working (403 `account_disabled`) until it is enabled; its email address stays reserved until the account is [deleted](#deleting-an-account), which happens on its own after
`PYN_DISABLED_ACCOUNT_RETENTION_DAYS` (default 90). Administrators
cannot disable themselves. Approvals, disables and enables are recorded with the actor and reason in a server-wide audit
log, read at `GET /v1/admin/audit`. The server warns once at start when only one active administrator exists, `GET /v1/admin/settings` reports it, and `pyn setup` and the installer suggest adding a second one. An administrator's own sign-in has no emailed or self-service recovery beyond the account's [recovery codes](#two-factor-authentication) (decision #968: host control is the only recovery path). If none is
reachable, `pyn-server recover-admin <username>` run on the host creates or repairs one; it needs host control and has no
API route ([recovery](../deployment.md#lost-administrator)).

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
| `POST /v1/login`, `POST /v1/session` | `{username, password, code?}`; `403` `email_not_verified`, `approval_pending` or `account_disabled` after a right password; `401` `second_factor_required` or `second_factor_invalid` for an account with a [second factor](#two-factor-authentication); an account scheduled for deletion by its owner signs in, with a credential that reaches only the deletion routes |
| `GET /v1/me/emails`, `POST /v1/me/emails {address}`, `POST /v1/me/emails/{address}/resend`, `PUT /v1/me/emails/{address}/primary`, `DELETE /v1/me/emails/{address}` | [your addresses](#your-email-addresses); `POST` gives `201` with the pending address |
| `PUT /v1/me/username` | `{username, password}` gives `200 {old_name, new_name, repositories}`; see [changing your username](#changing-your-username) |
| `GET /v1/me` | `{id, user, admin}` |
| `GET /v1/admin/users?status=&limit=` | accounts oldest first: `{id, user, email, email_verified, status, disabled_at, disabled_reason, purge_at, admin, created_at}`; `purge_at` is the scheduled deletion time, as in `GET /v1/account/deletion`, or null |
| `GET /v1/admin/settings` | `{account_deletion: {window_days, disabled_retention_days}, administrators: {active, single}}`: the effective deletion window and disabled-account retention, read-only (set by the environment variables below), and the count of active administrators; `single` is true when exactly one exists, so a client can warn |
| `POST /v1/admin/users/{user}/approve` | `{}`; the updated account |
| `POST /v1/admin/users/{user}/disable` | `{reason?}`; the updated account. `409 last_server_admin` if the account is the only active administrator |
| `POST /v1/admin/users/{user}/enable` | the updated account; also cancels a scheduled deletion |
| `POST /v1/admin/users/{user}/delete` | `{reason, immediately?}` for a disabled account: `{user, deleted, purge_at}`; see [deleting an account](#deleting-an-account). `409` `account_not_disabled`, `account_owns_resources`, `account_frozen`, `last_server_admin` |
| `GET /v1/account/deletion`, `POST /v1/account/deletion`, `POST /v1/account/deletion/restore` | the signed-in account's own deletion: status, schedule (`{password}`) and restore; they also answer for a pending account |
| `PUT /v1/admin/users/{user}/admin` | grants the flag; the updated account. `400 invalid_request` if the account is not active; granting again changes nothing |
| `PUT /v1/admin/access` | `{access_mode}`: the [server access mode](#server-access-mode) now in force; administrators only, never a service credential |
| `GET`/`PUT /v1/admin/two-factor`, `POST /v1/admin/users/{user}/two-factor/reset` | [requiring two-factor authentication](#requiring-two-factor-authentication) and an administrator's reset of an account's factors |
| `DELETE /v1/admin/users/{user}/admin` | revokes the flag; the updated account. `409 last_server_admin` if no other active administrator would remain; revoking from a non-administrator changes nothing |
| `GET /v1/admin/audit?before=&limit=` | `account_approved`, `account_disabled`, `account_enabled`, `account_deletion_scheduled`, `account_deletion_restored`, `account_deleted`, `admin_granted`, `admin_revoked`, `owner_limits_changed`, `owner_frozen`, `owner_unfrozen`, `server_setup_completed`, `server_access_mode_changed`, `external_account_created`, `external_identity_linked`, `external_identity_unlinked` and the `webhook_*` [webhook](webhooks.md) events, same page shape as a repository's |
| `GET /v1/admin/audit?before=&limit=` | `account_approved`, `account_disabled`, `account_enabled`, `account_renamed`, `email_added`, `email_verified`, `email_primary_changed`, `email_removed`, `two_factor_added`, `two_factor_removed`, `recovery_codes_regenerated`, `recovery_code_used`, `two_factor_reset`, `passkey_added`, `passkey_removed`, `passkey_clone_suspected`, `server_two_factor_requirement_changed`, `admin_granted`, `admin_revoked`, `owner_limits_changed`, `owner_frozen`, `owner_unfrozen`, `server_setup_completed`, `server_access_mode_changed`, `external_account_created`, `external_identity_linked`, `external_identity_unlinked` and the `webhook_*` [webhook](webhooks.md) events, same page shape as a repository's |

On the command line (server administrators; each listing is an aligned table with the name last):

| Command | Does |
| --- | --- |
| `pyn admin user list [--status S] [--limit N]` | columns `STATUS ROLE CREATED EMAIL USER` |
| `pyn admin user delete <user> [--now] --reason <text>` | deletes a disabled account after the recovery window, or at once with `--now` |
| `pyn account delete [--password-stdin]`, `pyn account restore`, `pyn account status` | your own account: schedule its deletion (password again), cancel it, show the deadline |
| `pyn admin user approve <user>`, `disable <user> [--reason R]`, `enable <user>` | the account routes above |
| `pyn admin grant <user>`, `pyn admin revoke <user>` | grant or revoke the administrator flag |
| `pyn admin service-credential create <name> --scopes manage_accounts,manage_organizations,manage_limits,manage_webhooks,manage_freeze [--expires-at T] [--allow-from ADDR,CIDR] [--rate-limit N]` | prints the secret alone on stdout; it is shown once. The options are [hardening](#hardening-a-service-credential) |
| `pyn admin service-credential list`, `revoke <name>` | columns `STATE CREATED LAST USED FROM BY SCOPES NAME`; `STATE` is `active`, `expired` or `revoked`, and revoked and expired ones are listed |
| `pyn admin limits list`, `show <owner>`, `set <owner> [--repos N] [--members N] [--storage SIZE]` | optional [limits](limits.md); `default` drops an owner's own value |
| `pyn admin freeze <owner> --reason R`, `unfreeze <owner>`, `frozen <owner>` | [freeze](limits.md#freezing-an-owner) a user or organization read-only, lift it, or show it |
| `pyn admin webhook create`, `list`, `show`, `update`, `delete`, `rotate`, `retire` | [webhook subscriptions](webhooks.md) and their secrets |
| `pyn admin org create <name> --owner <user>`, `pyn admin org delete <name> [--yes]` | on behalf of an owner; delete asks to type the name |

Automation signs in with `--token` or `PYN_TOKEN` set to a service credential's secret; it is limited to its scopes. A `--token`
value is visible in the process list (the CLI warns), so use `--token -` to read the token from one line of standard input, or `PYN_TOKEN` (the same goes for `pyn setup --setup-token` and `PYN_SETUP_TOKEN`).

The admin routes answer `403 server_admin_required` to anyone else and `404 user_not_found` for an unknown account.
`pyn register --email <address>` signs up on a server that verifies.

## Tokens

Tokens are modelled on GitHub's fine-grained tokens (decisions #1092 and #1099). A token is built once, for one **resource
owner**, one set of repositories and a list of per-resource permissions, and is never widened or edited afterwards; to use one
on a new repository, make a new token. It is shown once when created; only a hash is stored.

| Choice | Values |
| --- | --- |
| Name, description | a label; the description is optional |
| Resource owner | the creator's own account, or an organization the creator belongs to (`403 not_org_member` otherwise). The token acts only on what that owner owns: its repositories, and the organization or account itself |
| Expiry | an `expires_at` date, `never: true`, or neither, which means 30 days. Both together is `400 invalid_request` |
| Repository access | `public` (no repository beyond the public ones every token can read), `all` (every repository the owner has now and later) or `selected` (the listed `repos`, each owned by the owner) |
| Permissions | a list of `{resource, level}`: repository, organization and account resources, each only where the owner fits (see below) |

**Repository permissions** apply to the token's repositories. Each is `read` or `write` (write includes read); `locks` also has `admin`
(includes write); `policy` has no read level (`400 invalid_request`). Metadata read is required with `all` or `selected` and is added
when left out; `public` takes no repository permissions (`400 invalid_request`), and `all` or `selected` need at least one beyond metadata.

| Resource | Read | Write | Admin | Internal permissions |
| --- | --- | --- | --- | --- |
| `metadata` | repository info and the caller's own role | (none) | | none by itself |
| `contents` | clone, fetch, read files and history | check in, restore | | read; checkin and restore |
| `locks` | list locks | take and release one's own locks | force-unlock another person's lock | read; lock; force_unlock |
| `policy` | (none) | edit the rules (`.pyn/pyn.toml`) | | edit_policy |
| `members` | list roles | list members, teams and invitations; add and remove members, change roles | | read; manage_users |
| `administration` | (none yet) | rename, delete, define roles, the usage breakdown | | manage_roles |
| `audit` | read the repository audit log | (none) | | view_audit |

Which resource and level each repository route needs is in [repository routes](#repository-routes).

**Organization permissions** are for a token whose resource owner is an organization. Their wire names carry an `org_` prefix so
they never clash with the repository resources of the same topic.

| Resource | Read | Write |
| --- | --- | --- |
| `org_members` | list members | list invitations; invite, change a role, remove a member, cancel an invitation |
| `org_teams` | list and get teams | create, edit and delete teams, add and remove their members |
| `org_administration` | read the repository-creation policy, the organization's repository settings and the [two-factor requirement](#requiring-two-factor-authentication) | edit the profile and avatar, change the creation policy and its rules, change the repository settings and the two-factor requirement, delete the organization |
| `org_audit` | read the organization audit log | (none) |

**Account permissions** are for a token whose resource owner is the creator's own account. Their names carry an `account_` prefix.

| Resource | Read | Write |
| --- | --- | --- |
| `account_profile` | (none) | edit the profile and avatar |
| `account_ssh_keys` | list the account's keys | add and remove its keys |
| `account_locks` | list one's locks across repositories (`GET /v1/me/locks`) | (none) |

Creation rules for these, all with existing error codes:

- Organization resources are accepted only when the owner is an organization the creator belongs to, account resources only when
  the owner is the creator's own account (`400 invalid_request` otherwise). Repository resources need repository access `all` or
  `selected`; organization and account resources go with any repository access, `public` included, and then no metadata read is added.
- `org_audit` and `account_locks` take `read` only; `account_profile` takes `write` only (`read` is `400 invalid_request`, as for `policy`); every other one takes `read` or `write` (`400 invalid_request` for a higher level).
- An organization grant must add something the creator holds in that organization. Listing members or teams (`read` of `org_members` or
  `org_teams`) is open to any member; everything else (every write, `org_administration` read, `org_audit` read) needs an owner, so a
  member who is not an owner is refused (`403 not_org_owner`). Listing invitations is owner-only in the core and so needs
  `org_members` write; `org_members` read lists members only. An account grant is always held by its own account.

At use time an organization token acts with the intersection of its grants and its creator's current organization role, as a
repository token does with the repository role: demoting an owner to member makes an `org_teams` write token answer `403 not_org_owner`
on its next use, while reads that a member may make keep working. A token owned by one organization, or by an account, is refused on
another owner's routes (`403 forbidden`). `account_ssh_keys` reaches the account's own keys only, never another member's through
`manage_users`. `account_locks` read lists the creator's locks in every repository the creator can read, not only a token's
repositories, which is what the permission is for; a repository token keeps seeing its own repositories' locks.

A token never gets, whatever it holds: creating or revoking tokens, the password, external identities, two-factor factors, account
deletion, the hidden fields of the people directory, server administration, and creating an organization (that keeps the `namespace`
reach). Those routes stay `full_scope`, `admin_only` or `namespace`.

The server turns the grants into the nine [permissions](#permissions-and-roles) with one mapping and keeps the grants as the
record of what the token may do. A token authenticates with the **intersection** of those permissions and its creator's
current role in the repository the request is for, so demoting the creator narrows their tokens, and a revoked token stops
working at once. A token never holds more than its creator: each grant must add something the creator holds in every selected
repository, and the role cuts off the rest at use time (contents `write` carries both check-in and restore, so a writer's
token checks in but cannot restore). A token used on a repository it does not cover (another owner's, or outside `selected`)
is told the repository does not exist (`404 repo_not_found`); on a public one it holds `read` only. A token can rename or delete a
repository, or create repositories and organizations, only when it is a token of its own account for all repositories and holds
`administration` write. Tokens are meant for automation and CI; [OpenID Connect](external-sign-in.md) is for signing people in
(and, once the SSH transport arrives, [SSH keys](#ssh-keys)), and resolves to the same users and permissions.

A token for `all` repositories is also refused at creation (`403 forbidden`) when, for one of its grants, the creator holds none of the
permissions the grant adds in any repository the owner has now: a token that could never do anything is an error, not a silent
success. This is a convenience; the intersection at use time is what keeps a token within its creator. A member whose role is only
in repository X therefore cannot select Y (a repository they cannot see is `404 repo_not_found`, as everywhere), and an `all` token
of theirs acts only in X, only up to their role there, and answers any other repository as a missing one.

A creator's own account is the usual owner, which means a collaborator's token cannot reach a repository somebody else owns; a
collaborator works with a sign-in token or an organization-owned repository.

**Sign-in tokens** (`pyn login`, `sign_in: true` in the listing) are not fine-grained tokens. They keep reaching every repository
the account belongs to, whoever owns it, with every permission, and stay [full-scope credentials](#full-scope-credentials).

```bash
pyn token create ci --permissions contents:write,locks:write --repos alice/game --expires-days 30
pyn token create teams --permissions org_teams:write,org_members:read --owner acme --expires 2027-06-30
pyn token create keys --permissions account_ssh_keys:write --never
pyn token list
pyn token revoke <id>
```

`pyn token create` takes the choices above as flags, and a token cannot be edited afterwards:

| Flag | Meaning |
| --- | --- |
| `--permissions` | comma-separated `resource:level` grants, such as `contents:write`, `locks:admin`, `org_members:read` or `account_ssh_keys:write` (required) |
| `--owner` | the resource owner: your own account by default, or an organization you belong to; taken from `--repos` when those are given |
| `--access` | `public`, `all` or `selected`. Without it, `selected` when `--repos` is given or a repository resource is granted, `public` otherwise |
| `--repos` | the repositories (`owner/name`, one owner) of a `selected` token; the current repository by default |
| `--expires-days`, `--expires`, `--never` | expire after N days, at the end of a UTC date (`YYYY-MM-DD`), or not at all; 30 days when none is given |

The CLI checks the grammar (resources, levels, the access and owner fit, the expiry) to fail early with a clear message; the server
checks everything again and is the authority, including what the creator may grant. `pyn token list` shows each token's owner,
expiry, repository access (`selected:` then its repositories) and grants, and the secret is printed once, when it is created. The API takes the
grants directly:

```json
POST /v1/tokens
{
  "name": "ci",
  "description": "builds",
  "owner": "alice",
  "expires_at": null,
  "never": false,
  "repository_access": {"mode": "selected", "repos": ["alice/game"]},
  "permissions": [{"resource": "contents", "level": "write"}, {"resource": "locks", "level": "write"}]
}
```

`GET /v1/tokens` returns the same fields plus `user` (the creator) and `sign_in`.

## Error responses

`storage` and `history_corrupt` errors answer `500` with a generic message and their stable `code`; the detail is written to
the server log with the method and path of the request. Every other error keeps its message.

## Full-scope credentials

Account management is limited to a full-scope credential: a web session or a sign-in token from `pyn login`. A fine-grained
token is never full-scope, even one for every repository holding every grant. It covers tokens (create, list, revoke), the
password, [two-factor](#two-factor-authentication) factors, linking or unlinking an external identity, creating an account with `POST /v1/repos/{owner}/{name}/users`, and creating an invitation. A fine-grained token is refused with `403 forbidden`; it keeps working for its own scope. SSH keys, the profile and avatar, and the
organization routes of the [organization and account permissions](#tokens) are the exception: a token holding the matching grant reaches them. A sign-in token cannot create a token that
outlives it: the new token's expiry is capped at its own (so `never` from a sign-in token lasts as long as the sign-in does), and a
web session may create any token its owner could.

Two reads follow the same rule. `GET /v1/account/deletion` (the deletion status) needs a full-scope credential, like scheduling and
restoring a deletion (a pending account's own session and sign-in tokens are full-scope, so they still reach it). `GET /v1/me` is open
to every credential because a CI token may ask who it is, but a narrower one gets only the account name and id and the public profile
view, never the owner's hidden fields, visibility settings or `has_password` ([profiles](profiles.md)).

A fine-grained token may still read any public repository, but it learns nothing of the owner's standing outside its grant:
the `role` on `GET /v1/repos/{owner}/{name}`, `GET /v1/explore/repos` and `GET /v1/users/{user}` is absent for a repository the
token does not name (as for a stranger), the `role` on `GET /v1/orgs/{org}` is absent unless the credential is full-scope (the lookup itself still
succeeds), and `GET /v1/me/locks` lists only locks in the token's repositories.

The organization-wide reads need the same credential, because they expose data beyond one repository: the organization list
(`GET /v1/orgs`, `GET /v1/me/orgs`), and members, audit log, teams and the repository-creation policy, which a token owned by the
organization reaches only with the matching `org_*` read grant. Any other token or a service credential is refused (`403`). A team's `repos` hold only repositories the caller could open (public ones, or any in which they hold a
role), so a member never learns the name of a private repository they cannot see.

The owner-level reads of [limits, usage and freeze](limits.md#seeing-limits-and-usage) (`GET /v1/owners/{owner}/limits`,
`/usage` and `/freeze`) need it too: they are account-level, and the usage report names every repository of the owner. A
narrower token gets `403 forbidden`; a service credential keeps its `manage_limits` and `manage_freeze` scopes.

## Administrator access sessions

An administrator sometimes has to see what an account sees: a repository with an inappropriate name, tooling its owner keeps
private. By default an administrator sees what any other viewer sees ([profiles](profiles.md)); an **access session** is
the one way in, modelled on GitHub Enterprise Server's impersonation (decision #1024) and not covert: the account is told
and every use is audited. It is a distinct credential kind (`pyna_<id>_<secret>`), never a standing grant.

| | |
| --- | --- |
| Start | `POST /v1/admin/users/{user}/access-sessions {reason, notes?}` (`pyn admin access-session start <user> --reason ...`). Server administrators only, never a service credential or another access session. The secret comes back once, is stored only as a hash, and is presented as a bearer token. `201` |
| Reason | chosen for each session from `abuse_report`, `security_incident`, `support_request`, `legal_request` or `other`; `other` needs `notes` (at most 500 characters). Notes are optional otherwise. `400 invalid_request` for anything else |
| Length | one hour, fixed at start. Using the session never extends it and nothing renews it: a further session is a new start with its own reason, and its own notification |
| End | `POST /v1/admin/access-sessions/{id}/end` (`pyn admin access-session end <id>`): any server administrator may end any session, at once. A session that ran out stops working at `expires_at` without anyone acting |
| List | `GET /v1/admin/access-sessions?user=&limit=` (`pyn admin access-session list`): newest first, each with the routes it called |
| Who | the target must be an active user account (`404 user_not_found`, `403 account_inactive`), never the administrator's own (`400`). One administrator may act as another: the two are the actor and the target, and there is no separate approval step (decision #1024) |
| Stops early | when the administrator is no longer an active administrator, or the account is no longer active (disabled, scheduled for deletion), the session is refused (`401` or `403`) while that holds, and works again if it is undone before the hour is over |

### What a session reaches

A session carries the same access as the account where the account's repositories are concerned (GitHub's model: read and
act). It is not read-only. Everything that manages the account itself, access to it, or the server is refused with `403
access_session_not_allowed`, because a session must neither outlast itself nor widen. The rule is an allowlist by [route reach](#route-reach):
a reach the session is not listed for refuses it, so a new route is closed to sessions until it is declared otherwise.

| Reach | An access session |
| --- | --- |
| `open`, `public`, `optional_sign_in` | reaches them, as the account (the account's private repositories and fields, where the route narrows by viewer) |
| `any_credential` | reaches them, as the account: `GET /v1/me`, the repository list, `/v1/me/locks`, an organization lookup |
| `repo_scoped`, `public_repo_read`, `public_lfs_read` | reach them as the account's role allows: files, history, locks, check-in and restore, branches, policy and audit where the role holds the permission. Refused: `members:write` and `administration:write` (adding or changing members, invitations, teams and roles, renaming or deleting the repository, the usage breakdown), because they decide who can reach a repository afterwards or whether it exists; creating accounts and invitations in a repository is also refused (full scope) |
| `optional_full_scope` | refused: the people directory is not for a narrower credential |
| `full_scope`, `full_scope_pending`, `full_scope_enrolment`, `namespace`, `session` | refused. These are the account's own credentials and standing: tokens, SSH keys, [passwords](#accounts-and-signing-in), [two-factor](#two-factor-authentication) and passkeys, emails and username, [deleting the account](#deleting-an-account), creating repositories and organizations, accepting invitations, signing out |
| `org_scoped`, `account_scoped` | refused: organization administration (members, teams, settings, repository policy, audit) and the account's own profile, avatar and SSH keys. The account's role in an organization still applies to the organization's repositories through the repository routes |
| `admin_only`, `service:*`, `full_scope_or_service:*` | refused, whether or not the account is an administrator: a session never administers the server, starts another session, changes the freeze or limits of an owner, or reads the server audit log |

Each refusal is one of: managing credentials (a session could mint one and keep access after it ends), managing who may
reach a repository (the same), changing the account's identity or recovery (lockout), or acting as the account in the
server's administration (the administrator already has that with their own credential, which is audited under their own name).
Two-factor is not asked of the account for a session: the administrator proved theirs to start it, and a requirement the
account has not met yet does not apply to the administrator.

**Frozen owner.** A session on a [frozen](limits.md#freezing-an-owner) account is allowed, because reading is the point: it
reads what the owner's members read and every write under the owner is refused `owner_frozen`, exactly as for the account. A
session cannot lift or change the freeze (administrator route). **Organizations.** The account's memberships and team roles apply as
they do for the account, so the session reads and works in the organization's repositories by the account's role, and no
further; it cannot administer the organization, and a frozen organization is frozen for it. **Scoped credentials**
([tokens](#tokens)). A session is not a token and holds no grants: it has the account's full role reach, not that of a
fine-grained token the account also holds, and it never reads, lists, creates or revokes the account's tokens. The account's
tokens are untouched and keep working as before while a session runs and after it ends.

### The record

- **Server audit log**: `access_session_started` (reason, notes, expiry), `access_session_reached` and
  `access_session_ended` ([audit](#audit)), with the administrator as actor.
- **Account security log**: the same events, in a second hash-chained log kept for the affected account, keyed by its
  stable account id so a rename keeps it (`@account:<id>`). It is the same append-only chain as the others, so a break is
  detectable ([integrity](integrity.md)). Today it holds access sessions; it is where later account security events can go.
- **What was reached**: every request of a session is counted by method and route template (never a body), and the *first*
  call to each method, route and repository (`owner/name` for a repository route) writes one `access_session_reached` entry,
  as does the first refused call to a route (`refused:`). Later calls only add to the count, so a script cannot flood the
  logs. At most 200 distinct entries are itemised per session; the rest are counted under one `(further routes)` entry. The end
  entry carries the total (`N requests over M routes`). A call that cannot be recorded is not served. Repository audit logs
  record the account as the actor of what the session did, because that is whose access it was: the session log, which names
  the administrator and carries the time, is how to tell. An expired session is recorded as ended, at its expiry time, by a
  sweep that runs every minute and by any read of the sessions.
- **Notification**: `GET /v1/me/access-sessions` lists every access session on the signed-in account, newest first, with the
  administrator, reason, notes, times, state (`active`, `ended`, `expired`) and the routes it called. It cannot be turned off or
  dismissed: there is no setting, and no route that edits or deletes an entry, so a session started on an account always
  appears there, including one that has ended. It needs a full-scope credential and is refused to a session. Email does not exist yet
  (it will send the same facts once the optional email capability is there, and it will not be optional either). A web app
  shows a banner while a session is `active`, and the list as the account's security history.

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
| `manage_freeze` | [freeze](limits.md#freezing-an-owner) and unfreeze an owner, and read any owner's frozen state (`/v1/admin/owners/{owner}/freeze`, `/v1/owners/{owner}/freeze`) |
| `manage_webhooks` | create, change and delete [webhook subscriptions](webhooks.md) and rotate their secrets (`/v1/admin/webhooks*`) |

Scopes are only ever added, and a credential holds exactly the scopes it was given. Server administrators pass every
scope check. A credential is accepted only on the routes its scopes cover (`/v1/admin/*`, and the owner limits, usage and freeze reads); elsewhere it gets
`403 service_credential_not_allowed`. It can neither manage service credentials nor read the server log
(`403 server_admin_required`).

Creating and revoking a credential, and every action taken with one, go to the [server audit log](#protecting-open-registration)
with the actor `@service:<name>`; organization actions also appear in that organization's log.

| Route | Notes |
| --- | --- |
| `POST /v1/admin/service-credentials` | `{name, scopes, expires_at?, allowed_sources?, rate_limit_per_minute?}` gives `201 {secret, info}`; `400 invalid_request` for a bad name, no scope, an unknown scope, an expiry not in the future, a malformed range or a rate limit below 1; `409 service_credential_exists` |
| `GET /v1/admin/service-credentials` | `[{name, scopes, created_by, created_at, revoked_at, last_used_at, last_used_from, expires_at, allowed_sources, rate_limit_per_minute}]`, oldest first |
| `DELETE /v1/admin/service-credentials/{name}` | `204`; `404 service_credential_not_found` |
| `POST /v1/admin/orgs` | `{name, owner}` gives `201`; the owner must be an existing active user (`404 user_not_found`); `409 user_exists` |
| `DELETE /v1/admin/orgs/{org}` | `204`; `404 org_not_found`, `409 org_not_empty` |

A credential missing the scope gets `403 service_scope_required`; a revoked, expired or unknown one gets `401 unauthenticated`.

### Blast radius of each scope

A service credential has no repository access and no account, but each scope is a lever over everyone on the server. Grant
the fewest scopes a service needs, and use one credential per service so a leak is revoked on its own.

| Scope | What an attacker holding the secret can do |
| --- | --- |
| `manage_accounts` | list every account with its email and status, approve pending accounts, and disable any account, locking its owner out until it is enabled again, or enable one. It cannot set a password, sign in as anyone, or grant the administrator flag |
| `manage_organizations` | create an organization owned by any existing active user, which is how a squatted name or a surprise owner appears, and delete an organization that owns no repositories. It cannot touch an organization's content or members |
| `manage_limits` | read every owner's limits and usage (the usage report names every repository and its size) and lift or lower any owner's limits, which removes a quota meant to bound storage and cost, or starves an owner |
| `manage_freeze` | freeze any owner read-only (a denial of service against that owner's writes) or lift a freeze the operator set, and read any owner's frozen state |
| `manage_webhooks` | create subscriptions that send events to a host the attacker picks (limited by `PYN_WEBHOOKS_ALLOWED_HOSTS` when set, and never to private addresses), read what they carry, rotate or delete subscriptions and so cut a legitimate integration off |

None of the scopes reads file content, or reads or writes the server audit log and the credential list. Every use is
logged under `@service:<name>`.

### Hardening a service credential

All of these are optional at creation and none changes a credential that already exists. They are checked on the server
for every request, after the secret itself.

| Control | Behaviour |
| --- | --- |
| Expiry | `expires_at` is an absolute RFC 3339 time that must be in the future; from that instant the credential is refused with `401 unauthenticated`. An expired credential stays in the listing with its `expires_at` (the CLI shows `expired`); it is not revoked and cannot be extended, so make a new one (rotation in place is tracked separately) |
| Source restriction | `allowed_sources` lists addresses or CIDR blocks, IPv4 or IPv6, at most 64 (`10.0.0.0/8`, `192.0.2.7`, `2001:db8::/32`), stored in canonical form. A request from another address is refused with `401 unauthenticated`. A restricted credential is also refused when the server cannot tell the address, so it fails closed. Empty or omitted allows any address |
| Rate limit | A credential may make `rate_limit_per_minute` requests a minute (a fixed window, like the other [rate limits](#protecting-open-registration)); the next gets `429 too_many_attempts` with `Retry-After`. Without its own value the server default applies, `PYN_RATE_SERVICE_CREDENTIAL` (default `0`: no limit). Requests refused for their source do not count, so a thief elsewhere cannot use up the real service's allowance |
| Last use | The listing shows `last_used_at` and `last_used_from`, the address of the last accepted request (absent when the address is unknown) |
| Audit | A refused request of an identified credential (revoked, expired, from a disallowed source, a missing scope, or a route a service credential cannot reach) adds a `service_credential_refused` entry to the server audit log with actor `@service:<name>`, the reason and the address. It never contains the secret. A wrong or unknown secret identifies nobody and is not logged, and rate-limit refusals are not logged either, so a flood cannot fill the log |

The address is the one used for every other rate limit: the connection's peer, or the last `X-Forwarded-For` entry when
`PYN_TRUST_FORWARDED_FOR=true` (only behind a proxy that sets it). Source restriction is only as good as that setting, so
behind a proxy that does not overwrite the header it must not be relied on, and without one that the header is ignored.

### Recommended deployment

Keep the hosted service and the core on a private network path, so the core's API is not reachable from the internet at
all for that traffic:

- Give the service its own address range (a private network, VPN or peering link) and set `allowed_sources` to exactly
  that range; if the core sits behind a reverse proxy, let the proxy overwrite `X-Forwarded-For` and turn on
  `PYN_TRUST_FORWARDED_FOR`.
- Expose `/v1/admin/*` only on that path where the proxy can route it separately, and keep TLS on the link.
- One credential per service and per environment, only the scopes it uses, an `expires_at` (a quarter or a year) with a
  reminder to replace it, and a `rate_limit_per_minute` a little above its real peak.
- Store the secret in the service's secret manager, never in a repository or a log, and revoke it at the first sign of a leak.
- Watch `service_credential_refused` entries and `last_used_from` for use from an address that is not the service's.

## Route reach

Every route declares which credentials it accepts, in one table (`crates/pyn-server/src/routes.rs`) that also builds the
router. The OpenAPI document publishes each declaration as `x-pyn-reach` on the operation; the LFS and tus routes are in
the table but not in the document. A handler learns who is calling only through an extractor tied to its declared reach
(`crates/pyn-server/src/caller.rs`), so it cannot read an identity without declaring what it needs. The checks inside the
core (full scope, namespace management, administrator and service scopes, per-repository permissions) stay as a second line.

| Reach (`x-pyn-reach`) | Accepts |
| --- | --- |
| `open` | everyone, in both access modes; no credential is read |
| `public` | everyone on a `public` server; on a `private` one a valid credential. No credential is read otherwise |
| `optional_sign_in` | signed out or any user credential on a `public` server; any user credential on a `private` one |
| `optional_full_scope` | like `optional_sign_in`, but a signed-in caller needs full scope: a repository-scoped or otherwise narrower token is `403 forbidden`, a service credential `403 service_credential_not_allowed` (the people directory) |
| `session` | a web session cookie only |
| `any_credential` | any user credential; the response narrows to the credential's scope |
| `full_scope` | a session, a full-scope token or the development identity |
| `full_scope_pending` | `full_scope`, also while the account is scheduled for deletion |
| `full_scope_enrolment` | `full_scope`, also an account that must set up a [second factor](#requiring-two-factor-authentication) and has not (the enrolment routes); every other kind refuses such an account with `403 two_factor_enrolment_required` |
| `namespace` | full scope, or a token of the caller's own account for all repositories holding `administration` write |
| `org_scoped:<resource>:<level>` | a full-scope credential, or a token owned by the organization in the path holding the organization resource at the level; the creator's organization role still applies (see [organization and account routes](#organization-and-account-routes)). A personal token with repository `administration` write no longer reaches these routes |
| `account_scoped:<resource>:<level>` | a full-scope credential, or a token owned by the caller's own account holding the account resource at the level |
| `admin_only` | a server administrator; service credentials and [access sessions](#administrator-access-sessions) are refused |
| `service:<scope>` | a server administrator, or a service credential holding the scope |
| `full_scope_or_service:<scope>` | a full-scope user (the core limits it to its own owner), or a service credential holding the scope |
| `repo_scoped:<resource>:<level>` | a signed-in credential holding the resource at the level in the named repository (see [repository routes](#repository-routes)) |
| `public_repo_read:<resource>:<level>` | like `repo_scoped`, and a public repository's reads need no credential on a `public` server; a `private` one needs a credential and answers a signed-out caller `404 repo_not_found` |
| `public_lfs_read:<resource>:<level>` | the Git LFS download routes (`POST .../info/lfs/objects/batch`, `GET .../info/lfs/objects/{oid}`): like `public_repo_read`, answered in the LFS error format, and a signed-out batch is accepted only for `operation: "download"` (an upload batch signed out is `401`); a private server answers a signed-out caller `401` |

### What each credential kind reaches

The same rules as a matrix, by reach and credential kind. "Session" is a web session or a sign-in token (full scope), "access session" an [administrator access session](#administrator-access-sessions), "dev" the
development identity (full scope, administrator on its own header). A repository-scoped token is any token limited to
repositories or holding fine-grained grants; a service credential holds only `service` scopes. On a `private` server a signed-out
caller is refused wherever the table says "public servers only"; a credential that is sent must be valid on every route.

| Reach | Signed out | Session, full-scope token, dev | Repository-scoped token | Service credential | Access session |
| --- | --- | --- | --- | --- | --- |
| `open` | yes | yes | yes | yes | yes |
| `public` | public servers only | yes | yes | yes, public servers only | yes |
| `optional_sign_in` | public servers only | yes | yes (the same public answer) | no | yes, as the account |
| `optional_full_scope` | public servers only | yes | no | no | no |
| `session` | no | session cookie only | no | no | no |
| `any_credential` | no | yes | yes, narrowed to its repositories (no profile fields beyond the public view) | no | yes, as the account |
| `full_scope`, `full_scope_pending`, `full_scope_enrolment`, `namespace` | no | yes | no | no | no |
| `org_scoped`, `account_scoped` | no | yes | only a token owned by that organization or account holding the grant | no | no |
| `admin_only` | no | administrators | no | no | no |
| `service:<scope>` | no | administrators | no | holding the scope | no |
| `full_scope_or_service:<scope>` | no | yes | no | holding the scope | no |
| `repo_scoped` | no | yes, by role | only its repositories, holding the resource at the level | no | yes, except `members:write` and `administration:write` |
| `public_repo_read`, `public_lfs_read` | public repositories on public servers | yes, by role | its repositories by grant, other public repositories read-only | no | yes, as the account |
A repository-scoped token never reaches an account, organization or server-wide listing, another account's non-public profile
fields, an audit log outside its repositories, or any settings route. `GET /v1/orgs/{org}` and the organization profile are the
exceptions that answer only the public view, as do single-name profiles and avatars (an organization's name is not secret). `crates/pyn-server/tests/reach.rs` keeps this table
and the route table in step: every reach label must appear in it.

### Repository routes

A repository route declares the resource and level of the [token catalog](#tokens) that it needs. The extractor checks it
twice, and a repository that the credential cannot see is answered `404 repo_not_found` before either:

- A fine-grained token must hold the resource at the level (`403 forbidden`, "the token needs write access to contents"). Holding
  another resource does not stand in for it: a token with only `locks` read cannot read files. A sign-in token holds every grant.
- Every credential, tokens included, must then hold the internal permission the level adds, as `permissions_of` derives it
  (contents write: `checkin`, or `restore` on the restore route; locks write: `lock`; locks admin: `force_unlock`; policy write:
  `edit_policy`; members write: `manage_users`; administration write: `manage_roles`; audit read: `view_audit`; any read of
  contents, locks or members: `read`). A session, the development identity and a sign-in token have no grants to check, so only their
  role decides. Metadata read and administration read add no internal permission, so a route that needs only one of
  them is open to anyone who can see the repository.

A read of a public repository skips the grant check: any caller, signed out or with any token, may read it.

| Resource and level | Routes |
| --- | --- |
| `metadata:read` | `GET /v1/repos/{owner}/{name}`, `.../me` |
| `contents:read` | usage, files, tree, summary, content, history, verify, commits, snapshot, branches, fetch, objects, merge preview, and the LFS batch and object download (an upload batch needs `contents:write`, checked in the handler) |
| `contents:write` | checkin, changesets, restore, branch create, rename and delete, push, merge, object upload, chunk, missing and assemble, LFS object upload, verify and tus |
| `locks:read` | the lock lists: `.../locks`, the lock change stream `.../locks/events`, LFS lock list and verify |
| `locks:write` | checkout, release, LFS lock create and unlock |
| `locks:admin` | force-unlock |
| `policy:write` | policy preview |
| `members:read` | list roles |
| `members:write` | list members, invitations and teams; add a member, an invitation or a team; remove or change them; add a user (an invitation and a new user also need a full-scope credential, so no token reaches them) |
| `administration:write` | rename, delete, define roles, the usage breakdown |
| `audit:read` | the audit log |

The listings under `members:write` and the usage breakdown are guarded at the write level because the core requires
`manage_users` and `manage_roles` for them and no read-level grant adds those (decision #1117). Members read lists roles only,
and administration read grants nothing yet. A token holding contents read also holds locks read, so a read-only token keeps
listing locks; a token holding only locks read lists locks and reads no files, tree or history. Policy offers write only: the
rules are `.pyn/pyn.toml`, read with `contents:read`.

### Organization and account routes

An `org_scoped` or `account_scoped` route declares the organization or account resource and level it needs.
The extractor checks the token's owner (the organization named in the path, or the caller's own account) and the grant, and the core
repeats it before the role checks, so a token of another owner or without the grant is refused with `403 forbidden` ("the token needs
write access to org_members"). A session, the development identity and a sign-in token have no grants and are decided by the
creator's role alone, as before.

| Resource and level | Routes |
| --- | --- |
| `org_members:read` | `GET /v1/orgs/{org}/members` |
| `org_members:write` | `GET /v1/orgs/{org}/invitations`, `POST /v1/orgs/{org}/members`, `PATCH` and `DELETE /v1/orgs/{org}/members/{user}`, `DELETE /v1/orgs/{org}/invitations/{user}` |
| `org_teams:read` | `GET /v1/orgs/{org}/teams`, `GET /v1/orgs/{org}/teams/{team}` |
| `org_teams:write` | create, `PATCH` and `DELETE` a team, `PUT` and `DELETE` a team member |
| `org_administration:read` | `GET /v1/orgs/{org}/repo-policy`, `GET /v1/orgs/{org}/settings`, `GET /v1/orgs/{org}/two-factor` |
| `org_administration:write` | `PUT /v1/orgs/{org}/profile`, `PUT` and `DELETE /v1/orgs/{org}/avatar`, `DELETE /v1/orgs/{org}`, `PUT /v1/orgs/{org}/repo-policy`, `PUT` and `DELETE` its `rules/{effect}/{kind}/{subject}`, `PUT /v1/orgs/{org}/settings`, `PUT /v1/orgs/{org}/two-factor` |
| `org_audit:read` | `GET /v1/orgs/{org}/audit` |
| `account_profile:write` | `PUT /v1/me/profile`, `PUT` and `DELETE /v1/me/avatar` |
| `account_ssh_keys:read` | `GET /v1/keys` |
| `account_ssh_keys:write` | `POST /v1/keys`, `DELETE /v1/keys/{id}` |

The organization write routes used to accept a personal token for all repositories holding repository `administration` write
(`namespace`). They now need an organization-owned token with the matching organization grant (decision #1120), so such
a personal token gets `403 forbidden` on them. `GET /v1/orgs/{org}/profile`, `GET /v1/orgs/{org}/avatar` and `GET /v1/orgs/{org}` stay as
they were (any credential, the public view; the caller's `role` only for a full-scope credential). `GET /v1/me/locks` stays
`any_credential`: a repository token lists the locks in its repositories, and a token holding `account_locks` read lists the creator's
locks everywhere. Accepting an organization invitation, declining one, listing one's own invitations and organizations, and creating
an organization are not organization permissions and keep their `full_scope` and `namespace` reaches.

A repository-scoped token reaches only `open`, `public`, `optional_sign_in` (single-name public profiles and avatars), `any_credential` and the repository kinds; a service
credential reaches only `open`, `public` (on a `public` server), the `service` kinds and `full_scope_or_service`. `crates/pyn-server/tests/reach.rs` checks that the table and the
document agree and calls every route with each credential kind, so a new route with no declaration, or one that a credential
reaches against its declaration, fails the build.

## First-run setup

A new server is uninitialised. Until setup completes it serves only `GET /v1/setup`, `POST /v1/setup` and `GET /healthz`;
every other route answers `503 not_initialised`. Setup creates the first administrator and records the essentials, and it
can never run again (`409 already_initialised`).

To stop whoever reaches a fresh server first from claiming it, setup needs a one-time **setup token**. On first start the server prints
one to standard error in a delimited block (never through the log), or an operator sets `PYN_SETUP_TOKEN` (at least 16 characters, no
whitespace) ahead of time for unattended installs. The token is compared in constant time, wrong tries are rate limited
per client (10 per 15 minutes) and, at a much higher ceiling (1000), for the whole server (`429 too_many_attempts`), so one client or a few addresses cannot use up everyone's budget before the operator runs setup, and it is discarded when setup completes, after which it is never printed again. Attempts are counted before the comparison and a right token gives its count back. A server that is
already set up ignores `PYN_SETUP_TOKEN` and warns about it. With several server copies on one database, set the same
`PYN_SETUP_TOKEN` on all of them; otherwise each prints its own token and any one of them completes setup. Server name and
public address are re-read from the database every minute, so a copy picks up changes made through another.

| Route | Notes |
| --- | --- |
| `GET /v1/setup` | `{initialised, server_name?, public_url, registration, access_mode}`; no secrets. Before setup `public_url`, `registration` and `access_mode` are the configured defaults |
| `POST /v1/setup` | `{token, username, password, email?, server_name?, public_url?, registration, access_mode?}` gives `201 {user}`; `403 invalid_setup_token`, `400 invalid_request`, `409 already_initialised`, `429 too_many_attempts` |

`registration` is `open`, `invite` or `closed`; `access_mode` is `private` or `public` (see [server access mode](#server-access-mode)). What setup records replaces `PYN_REGISTRATION`, `PYN_ACCESS` and `PYN_PUBLIC_URL`; the
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

The server refuses to start with `PYN_DEV_AUTH=true` unless `PYN_ADDR` is a loopback address (`127.0.0.1`, `::1` or
`localhost`). To run it on another address anyway (a throwaway container, say), also set
`PYN_DEV_AUTH_ALLOW_NON_LOOPBACK=true`. The server logs a warning with the address on every start with the flag on, and
`/healthz` carries an `X-Pyn-Dev-Auth: on` response header (`off` otherwise). A bearer token always wins over the
header, and the flag never changes what a service credential may do.

## Audit

Organization events (created, deleted, members added, removed or changed, teams and their members, repository-creation policy changes) are kept in a separate [organization log](repositories.md#organizations) for its
owners (`pyn org audit <org>`). Adding a member (by an admin, by registration or as the creator of a repository), changing a member's role, changing what a
role grants, giving a team a role or taking it away, creating or revoking a token or an invitation (never its code), and creating, changing or deleting the repository itself are recorded in that
repository's audit log with the actor and what changed, and are visible to `view_audit`. Server-wide account actions and service credential changes go to
the [server audit log](#protecting-open-registration), and so do [administrator access sessions](#administrator-access-sessions), which are also written to a security log of the affected account. A token's events go to the log of
each repository it is limited to. Token secrets are never recorded, and neither are sign-in sessions.
