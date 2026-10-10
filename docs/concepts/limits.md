# Limits and usage

Limits are optional. A server enforces nothing until its operator sets a limit: with none configured, no owner is
limited, no default exists, no extra configuration is needed, and every command and route behaves as it did before limits
existed. They are meant mainly for public and hosted servers that need to keep one owner from starving the rest. The
server holds no concept of plans or payment; an operator or an external service only sets generic numbers.

## What can be limited

An **owner** is a user or an organization: the namespace in `owner/name`.

| Limit | Counts | Applies to |
| --- | --- | --- |
| `repositories` | repositories the owner has | users and organizations |
| `members` | members of the organization (owners included) | organizations only; a user has no member limit |
| `storage_bytes` | bytes of distinct content across the owner's repositories | users and organizations |

Unset means unlimited. `0` is a valid limit and blocks growth entirely. Bandwidth is not limited: nothing measures it yet.

Storage counts each distinct content once per repository (the same bytes checked in at two paths, or restored, add
nothing) and each repository separately, so deleting a repository frees its bytes. Revisions made before limits existed
carry no recorded size and count as 0 bytes.

## Where a limit comes from

For each field the server uses the owner's own value if one is set, else the **server default**, else no limit. The
default is itself unset unless the operator provides it:

| Variable | Sets the default for |
| --- | --- |
| `PYN_DEFAULT_MAX_REPOSITORIES` | repositories |
| `PYN_DEFAULT_MAX_ORG_MEMBERS` | members of an organization |
| `PYN_DEFAULT_MAX_STORAGE_BYTES` | stored bytes |

There is no way to exempt one owner from a default except a number large enough not to matter.

## Enforcement

The server checks at the moment something grows, in the same atomic step as the write, so concurrent requests cannot
overshoot. Each limit has its own error code and answers `409`:

| Code | When |
| --- | --- |
| `repo_limit_reached` | creating a repository when the owner already has the limit |
| `member_limit_reached` | adding a member to an organization at its limit |
| `storage_limit_reached` | a check-in or restore that would add content past the limit |

Lowering a limit below current use never deletes anything: existing repositories, members and content stay, and only
new growth is refused until use falls under the limit. The [lock limit](pyn-toml.md#lock-limit) is separate and
unchanged.

## Setting limits

Server administrators, and service credentials with the `manage_limits` scope, set an owner's own limits. A field left
out of a change stays as it is; a number sets it and `null` returns the field to the server default.

| Route | Notes |
| --- | --- |
| `PATCH /v1/admin/owners/{owner}/limits` | `{repositories?, members?, storage_bytes?}`; `400 invalid_request` for a value out of range or a member limit on a user; `404 user_not_found`; `403 server_admin_required` or `service_scope_required` |
| `GET /v1/admin/limits` | the server default and every owner with limits of its own |

Each change is recorded as `owner_limits_changed` in the server audit log, and in the organization's log for an
organization.

```bash
pyn admin limits set alice --repos 20 --storage 5G
pyn admin limits set acme --members 50 --repos default    # `default` drops the owner's own value
pyn admin limits show alice
pyn admin limits list
```

## Seeing limits and usage

The owner (for an organization, an owner of it), a server administrator and a service credential with `manage_limits`
may look at an owner's limits and usage. Anyone who can read a repository may see its own usage.

| Route | Returns |
| --- | --- |
| `GET /v1/owners/{owner}/limits` | `{kind, effective, own}`: the limits in force, and the owner's own; a null field is unlimited |
| `GET /v1/owners/{owner}/usage` | `{owner, limits, usage: {repositories, members, stored_bytes}, repositories: [...]}` with each repository's usage |
| `GET /v1/repos/{owner}/{name}/usage` | `{repository, stored_bytes, files, revisions}` |

```bash
pyn usage              # you
pyn usage acme         # an organization you own
pyn repo usage alice/game
```
