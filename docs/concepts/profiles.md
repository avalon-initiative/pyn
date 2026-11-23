# Profiles

An account can carry a small public profile: who this person is and what they work on. A profile is optional; an account
with none shows its name, join date and repositories.

## Fields

| Field | Limit |
| --- | --- |
| `display_name` | at most 64 characters, trimmed, no control characters |
| `bio` | at most 160 characters, trimmed; may span lines, no other control characters |
| `location`, `company` | at most 64 characters each, trimmed, no control characters |
| `links` | at most 5 distinct `http` or `https` URLs, each at most 200 bytes, ASCII, no spaces, no user name or password in them |
| avatar | a png, jpeg or webp image of at most 1 MiB, or none |

Limits count characters, not bytes (links count bytes). A blank value is cleared. A field that breaks a limit is refused with
`400 invalid_request` and nothing is changed.

A profile is public, readable signed out on a `public` [server](access.md#server-access-mode) (a `private` one needs a sign-in), except for the fields its owner hides (see [Privacy](#privacy)). An email
address is never part of a profile response, whoever asks.

## Privacy

The owner can hide each of `display_name`, `bio`, `location`, `company`, `links` and the avatar. Every field is visible by
default, so existing accounts and clients are unchanged. The setting is a `visibility` object of booleans (`false` hides):

```json
{ "display_name": true, "bio": false, "location": true, "company": true, "links": true, "avatar": false }
```

- `PUT /v1/me/profile` takes an optional `visibility`. Left out, the stored setting is kept (so a client that does not know
  about it never resets it); given, it replaces the setting, and a key missing from it is visible.
- `GET /v1/me` and the owner's own `GET /v1/users/{user}` (through a full-scope credential) carry `visibility` and every field. Any other viewer gets the
  profile without the hidden fields and without a `visibility` object: a hidden text field is `null`, hidden `links` is
  empty, a hidden avatar has no `avatar_url` and `GET /v1/users/{user}/avatar` answers `404 avatar_not_found`. Nothing
  tells a viewer whether a field is hidden or just empty.
- A server administrator sees what any other viewer sees: hiding is not overridden by the admin role. The one way past it is an [administrator access session](access.md#administrator-access-sessions): the account is told and every use is audited. A credential that is not full-scope counts as
  another viewer, even the owner's own.
- The lists of people below honour it too, for every viewer including the owner: a hidden display name or avatar is
  null in `PublicIdentity`.

Avatar responses are revalidated on every use (see [Avatars](#avatars)), so hiding takes effect at the next request; only a
copy a client already displays outlives it.

## Routes

| Route | What it does |
| --- | --- |
| `GET /v1/users` | The people directory: browse and search accounts. Signed out on a `public` server, rate limited. |
| `GET /v1/users/{user}` | The public profile. Works signed out on a `public` server. |
| `GET /v1/users/{user}/avatar` | The avatar image. Works signed out on a `public` server. |
| `PUT /v1/me/profile` | Replaces your display name, bio, location, company and links. A field left out is cleared. |
| `PUT /v1/me/avatar` | Sets your avatar from the request body (the raw image bytes). |
| `DELETE /v1/me/avatar` | Removes your avatar. |
| `GET /v1/me` | Your account, with `has_password` and your `profile`. A credential that is not full-scope gets only the account `id` and `user` and the public view of the profile. |

Editing needs a full-scope credential (a session, or a sign-in token from `pyn login`; a fine-grained token never is, whatever it holds).

`GET /v1/me` through a credential that is not full-scope (a repository-scoped or read-only token) answers "who am I" and nothing
more: the account name and id, `admin` false, and the profile as a stranger sees it (hidden fields absent, no `visibility`
object), with no `has_password`. A CI token can therefore identify its owner without reading what the owner hid. Full-scope
credentials get the full own view.

`GET /v1/me` reports `has_password` to a full-scope credential. It is false for an account created through
[external sign-in](external-sign-in.md) that has not set one; such an account sets its first password with
`PUT /v1/me/password` and no current password, from a signed-in web session.

## What a viewer sees

`GET /v1/users/{user}` returns the profile, the account id and join date, and two lists that depend on the viewer:

- `repositories`: the repositories the user owns that the viewer may know about, by the same rule as every other
  repository route: public ones, plus private ones where the viewer holds a role (directly, through a team, or as an
  organization owner). A stranger or a signed-out visitor never sees a private repository of the user, and
  `repository_count` counts only the listed ones. The listing carries no pagination: it is every visible repository, ordered
  by name.
- `organizations`: the organizations the user belongs to that the viewer belongs to as well, with the viewer's role. pyn has no
  public organizations (an organization's members are visible to its members only), so a signed-out visitor sees none, and
  nobody learns of an organization they are not in.

A name that is no account, an organization name, and an account that is disabled or not yet approved all answer
`404 user_not_found`, with the same message, so a disabled account is indistinguishable from a missing one. A frozen
owner stays visible: freezing refuses writes, not reads.

## People directory

`GET /v1/users` lists accounts, searchable, the way GitHub's people search is. It is readable signed out on a `public`
[server](access.md#server-access-mode); a `private` one needs a sign-in (`401 unauthenticated`). A session, a full-scope token or the development identity gets exactly
the entries a signed-out caller gets, because an entry holds only public data. A repository-scoped or otherwise narrower token is
`403 forbidden` (a scoped credential reaches only what it was granted, never a server-wide listing), a service credential is
`403 service_credential_not_allowed`, and a credential that is sent must be valid (else `401`). Signed-out requests are
rate limited per client address (60 per minute, `PYN_RATE_PEOPLE_CLIENT`; see
[rate limits](access.md#protecting-open-registration)): past it `429 too_many_attempts` with `Retry-After`. Signed-in callers
are not limited. Query: `q` (optional
search), `after` (an account name; the page starts strictly after it) and `limit` (default 30, at most 100). The response is
`{ "people": [...], "next_after": "name" | null }`, ordered by account name (byte order, so capitals sort first); pass
`next_after` back as `after` for the next page. Each entry is `identity` (`PublicIdentity`) plus `bio`, `location`, `company`
and `links`, as a stranger sees them.

- Only active user accounts are listed: never a disabled account, one not yet approved, one with a deletion scheduled,
  or an organization. There is no email address, repository, count or role in an entry, and no visibility object.
- `q` (at most 64 characters, trimmed, else `400 invalid_request`) keeps accounts whose name or display name contains it,
  ignoring case; it is a plain substring, with no wildcards. A display name its owner hides is not searched, so a search
  cannot reveal it. Bio, location, company and links are not searched. A blank `q` lists everyone.
- Repositories stay out of it: a private repository appears in no list, count or search, and asking for one by address is
  the same `404 repo_not_found` as for a missing one.

## Public identity in lists

Wherever a response lists people, it carries a small public identity next to the account name, so clients can show an
avatar and display name without one profile request per person. `PublicIdentity` is `user` (the account name),
`display_name` and `avatar_url` (same form as in the profile), both null when unset. It is the same data as the public
profile as a stranger sees it, so a field its owner hid is null.

| Field | On |
| --- | --- |
| `identity` | repository members, organization members, and lock owners (`GET .../locks`, the summary's `locks`, the lock on file and tree entries) |
| `author_public` | revisions, stranded versions and commits (every response that carries `author_current`) |
| `actor_public` | activity and audit entries, the organization's included |
| `owner_identity` | repositories owned by an organization (see [Organization profiles](#organization-profiles)) |

For history the identity is that of the account's current name when it was renamed, else the name as written. The field is
absent when no active account holds the name (deleted, disabled, not yet approved, or an organization). It is additive:
`author`, `actor` and `owner` are unchanged. The lock responses of checkout and force-unlock and the caller's own
`GET /v1/me/locks` carry none.

## Organization profiles

An organization carries a description, a display name, a website link and an avatar. All are optional and public to the
same readers as the organization itself; there is no per-field privacy for them. The description is at most 256
characters, trimmed, may span lines, and has no other control characters. The display name (shown beside the handle) is 1
to 80 characters, trimmed, on one line, with no control characters. The link (`url`) is an `http` or `https` address of at
most 200 characters with a valid host, no spaces and no user name or password. A blank value clears any of them. The avatar follows [Avatars](#avatars) below (png, jpeg or webp,
at most 1 MiB, judged from the bytes) and shares their storage.

| Route | What it does |
| --- | --- |
| `GET /v1/orgs/{org}/profile` | `{name, description, display_name, url, avatar_url}`; the three optional fields are null when unset. Any signed-in account, like `GET /v1/orgs/{org}`. |
| `PUT /v1/orgs/{org}/profile` | `{description?, display_name?, url?}` replaces all three: a field left out or blank is cleared. `400 invalid_request` for a field that breaks its limits or a `url` that is not a plain http or https address. |
| `PUT /v1/orgs/{org}/avatar` | Sets the avatar from the request body (the raw image bytes). |
| `DELETE /v1/orgs/{org}/avatar` | Removes the avatar. |
| `GET /v1/orgs/{org}/avatar` | The image, readable signed out on a `public` server so it can sit in an `<img>`; `404 avatar_not_found` when none. |

Editing is for organization owners with a full-scope credential (`403 not_org_owner` for anyone else), and is refused while
the organization is [frozen](limits.md#freezing-an-owner). An organization may change its avatar 30 times an hour. The
`avatar_url` has the form `/v1/orgs/{org}/avatar?v=<hash start>`, cached like a user's. Deleting an organization deletes its
profile. Edits are written to the organization's audit log as `org_profile_updated` (written only when something changed; the detail lists the changed fields, `description`, `display_name` and `url`), `org_avatar_set` and
`org_avatar_removed`, with the acting owner; the detail names the field, never the text or the image.

Every repository response (`RepoInfo`) carries `owner_identity` when an organization owns the repository: a `PublicIdentity`
whose `user` is the organization's name, `display_name` is null (an organization's display name is on its profile) and `avatar_url` is its avatar.
It is absent for a repository a user owns, whose profile is on `GET /v1/users/{user}`.

## Avatars

The server stores the bytes through the object store under their SHA-256 and records the hash, the type and the size on the
account. The type is judged from the bytes, never from the `Content-Type` of the upload: the server checks the size
(`413 object_too_large` over 1 MiB), then the magic numbers (a PNG signature followed by an `IHDR` chunk, a JPEG start-of-image
marker, a RIFF container holding a `VP8 `, `VP8L` or `VP8X` WebP chunk), and refuses anything else with
`415 unsupported_image`. An empty body is `400 invalid_request`. No image is decoded, resized or re-encoded: what was uploaded
is what is served, so a file that has the right header and is otherwise not a valid image is stored as sent, and a client
that cannot display it shows no avatar.

The avatar route answers with the detected `Content-Type`, `X-Content-Type-Options: nosniff`, a sandboxing
`Content-Security-Policy`, and an `ETag` (the hash; `If-None-Match` gets a `304`). The profile's `avatar_url` ends in `?v=` and
the start of the hash, which keeps caches of different images apart. Every response carries `Cache-Control: no-cache`, so a
cache must revalidate with the `ETag` before each use: a new image takes effect at once, and an avatar that was hidden or
removed answers `404 avatar_not_found` on the next request instead of a `304`. The remaining window is a client that
displays a copy without asking the server (an open page, or an image it has already decoded) until it next loads the image.

Replacing or removing an avatar does not delete the old bytes. The object store is shared and content-addressed, so another
record may name the same object. An account may change its avatar 30 times an hour.

## CLI

```bash
pyn profile show [user]                    # yours when no user is named
pyn profile set --name "Ada" --bio "..." --location Lisbon --company Acme --link https://ada.example
pyn profile set --name "" --link ""        # an empty value clears a field; --link "" clears the list
pyn profile avatar set me.png
pyn profile avatar remove
pyn whoami                                 # alice (Ada) once a display name is set
```

`pyn profile set` leaves fields it is not given as they are, and `--link` replaces the whole list.
