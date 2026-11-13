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

Everything in a profile is public, readable signed out. There are no per-field privacy settings (provisional: the owner
cannot hide a field, only clear it). An email address is never part of a profile response, whoever asks.

## Routes

| Route | What it does |
| --- | --- |
| `GET /v1/users/{user}` | The public profile. Works signed out. |
| `GET /v1/users/{user}/avatar` | The avatar image. Works signed out. |
| `PUT /v1/me/profile` | Replaces your display name, bio, location, company and links. A field left out is cleared. |
| `PUT /v1/me/avatar` | Sets your avatar from the request body (the raw image bytes). |
| `DELETE /v1/me/avatar` | Removes your avatar. |
| `GET /v1/me` | Your account, with `has_password` and your `profile`. |

Editing needs a full-scope credential (a session, or a token with every permission and no repository restriction).

`GET /v1/me` reports `has_password`. It is false for an account created through
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
the start of the hash: a request carrying the current version is cached for a year (`immutable`), any other for five minutes,
so a new image takes effect at once.

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
