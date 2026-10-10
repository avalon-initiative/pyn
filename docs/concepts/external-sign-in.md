# Signing in with an external provider

A server can let people sign in with an OpenID Connect provider, such as a company identity provider, as well as with a
password. It is opt-in: with no provider configured nothing changes, no route behaves differently and no setting applies.
The provider is only ever a way to prove who someone is. What they may do is decided by the same
[accounts, roles and permissions](access.md) as for any account, and a session opened through the provider is the same
web session as a password sign-in (cookie, CSRF token, expiry). Personal access tokens and SSH keys work as they do for
every account, and the command line keeps using them.

## Configuring a provider

Register pyn with the provider as a confidential client using the authorization-code flow, with this redirect URL:
`{PYN_PUBLIC_URL}/v1/oidc/callback` (or `PYN_OIDC_REDIRECT_URL`). The web app and `/v1` are expected on one origin, which is
how `pyn-web` reaches the API.

| Variable | Meaning |
| --- | --- |
| `PYN_OIDC_ISSUER` | The provider's issuer URL; discovery is read from `{issuer}/.well-known/openid-configuration`. `https` only, except `http` to this machine for development |
| `PYN_OIDC_CLIENT_ID`, `PYN_OIDC_CLIENT_SECRET` | The client's credentials. All three variables must be set together; setting some of them is a startup error |
| `PYN_OIDC_NAME` | What the sign-in button calls the provider, default `Single sign-on` |
| `PYN_OIDC_USERNAME_CLAIM` | The ID-token claim a new account's user name comes from, default `preferred_username` |
| `PYN_OIDC_CREATE_ACCOUNTS` | `true` lets a first sign-in create the account. Default `false`: only people who have linked the provider to an account can use it |
| `PYN_OIDC_REDIRECT_URL` | The redirect URL, if it is not `{PYN_PUBLIC_URL}/v1/oidc/callback` |
| `PYN_PASSWORD_SIGN_IN` | `false` turns password sign-in off (see below); needs a provider. Default `true` |

The provider is contacted lazily, so a server starts even when the provider is down; sign-in then answers
`502 external_provider_unavailable` until it is back. The client secret is never logged or returned by the API.

## The flow

1. The web sign-in page reads `GET /v1/sign-in-options`: `{password, external: {name, sign_in_url} | null}` and offers the
   provider when it is present.
2. The browser navigates to `GET /v1/oidc/authorize?return_to=/some/path`. The server stores a random state, nonce and
   PKCE verifier (in the metadata store, so any server instance can finish the flow; a flow lasts 10 minutes and works
   once), sets a short-lived `HttpOnly` cookie `pyn_oidc` limited to `/v1/oidc` that ties the flow to this browser, and
   redirects to the provider with `response_type=code`, `scope=openid profile email`, `state`, `nonce` and an S256
   `code_challenge`.
3. The provider redirects to `GET /v1/oidc/callback?code=&state=`. The server checks the state and the cookie, exchanges
   the code with the verifier, and validates the ID token: signature against the provider's published keys (RSA, ECDSA or
   EdDSA, chosen by the key, never by the token), `iss`, `aud` (and `azp` when there are several), `exp`, `nbf`, the
   nonce and a present subject.
4. It then redirects the browser to the web app: `{PYN_PUBLIC_URL}{return_to}` (default `/`) with the `pyn_session`
   cookie set, or `{PYN_PUBLIC_URL}/sign-in?error=<code>` when anything failed. `return_to` must be a path in the web
   app; anything else is refused with `400 invalid_request`.

Error codes on the sign-in page: `external_sign_in_failed` (a bad, expired or replayed callback, an invalid ID token, or
the person did not sign in at the provider), `external_account_not_linked`, `external_identity_taken`,
`external_provider_unavailable`, `account_disabled`, `approval_pending` and `email_not_verified` (the same account
states that stop a password sign-in), and `too_many_attempts`. Starting a flow is rate limited per client address like
failed password sign-ins.

## Accounts

An account is linked to the provider's **issuer and subject**, which never change for a person. Nothing else links:

- **First sign-in with no linked account.** With `PYN_OIDC_CREATE_ACCOUNTS=true` the server creates an active account with
  no password. Its name comes from the configured claim, else from the part of the email before the `@`, lowercased with
  other characters turned into `-`. A [reserved](repositories.md#addresses-and-namespaces) or already taken name is never
  used: the server appends `-2` to `-5`, then random hex. With the option off the sign-in ends with
  `external_account_not_linked`. Creating accounts through the provider is independent of `PYN_REGISTRATION`, so a server
  with closed registration can still onboard its provider's people, and
  [sign-up protection](access.md#protecting-open-registration) (email verification, approval) does not apply: the
  operator trusts the provider.
- **Email addresses never link an account.** A matching address, verified or not, does not give access to an existing
  account. The provider's address is stored on a new account only when the provider says it verified it
  (`email_verified`) and no other account has verified the same address; the account then counts as verified.
- **Linking an existing account.** A signed-in person sends `POST /v1/oidc/link` (CSRF token required, optional
  `return_to`) and navigates to the returned `url`; the identity the provider then signs in as is linked to their
  account and their session is unchanged. A provider identity links to one account only (`external_identity_taken`);
  linking it again to the same account is harmless.
- **Seeing and removing links.** `GET /v1/me/identities` lists them; `DELETE /v1/me/identities/{id}` removes one. The only
  way into an account cannot be removed: with no password and no other link the answer is `409 last_sign_in_method`.
- **Disabled accounts** cannot sign in through the provider, and disabling an account ends its sessions as always.

Linked identities and provider-created accounts are written to the [server audit log](access.md#protecting-open-registration)
as `external_account_created` (actor: the new account), `external_identity_linked` and `external_identity_unlinked`.
Sign-ins themselves are not recorded, like password sign-ins.

## Turning password sign-in off

`PYN_PASSWORD_SIGN_IN=false` refuses password sign-in (`POST /v1/session`, `POST /v1/login`) with
`403 password_sign_in_disabled` after the password is checked, so a wrong password still looks like one. **Server
administrators are exempt**, so a broken provider cannot lock the operator out. The setting needs a configured provider;
the server refuses to start otherwise. `GET /v1/sign-in-options` reports `password: false`. A person with no password can
set one with `PUT /v1/me/password` (no current password needed), which only matters if the setting is turned back on.

## Contract for clients

| Route | Notes |
| --- | --- |
| `GET /v1/sign-in-options` | `{password, external: {name, sign_in_url} \| null}` |
| `GET /v1/oidc/authorize?return_to=` | `302` to the provider and a `pyn_oidc` cookie; `404 oidc_not_configured` without a provider |
| `GET /v1/oidc/callback` | `302` to the web app, always: see [the flow](#the-flow) |
| `POST /v1/oidc/link?return_to=` | `{url}`; signed in only |
| `GET /v1/me/identities` | `[{id, issuer, subject, email, created_at, last_sign_in_at}]`, oldest first |
| `DELETE /v1/me/identities/{id}` | `204`; `404 external_identity_not_found`, `409 last_sign_in_method` |

`pyn-web` needs a `/sign-in` page that reads `?error=`, a button for `external.name`, and an account-settings list of
linked identities. The CLI has no provider flow: sign in on the web, then create a personal access token.
