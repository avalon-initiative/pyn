# pyn documentation

Concepts, architecture and specs for pyn: version control that merges what can be merged and locks what shouldn't be.
Markdown only; `make docs-check` verifies relative links and anchors.

- [Concepts](concepts/README.md): the model, in the order to read it
- [pyn.toml specification](concepts/pyn-toml.md)
- [Repositories: owners, names, visibility and the API shape](concepts/repositories.md)
- [Browsing a repository: folder listing and summary](concepts/browsing.md)
- [Access: roles, permissions and tokens](concepts/access.md)
- [Signing in with an external provider: OpenID Connect](concepts/external-sign-in.md)
- [Limits and usage: optional per-owner caps](concepts/limits.md)
- [Events: the lifecycle event schema](concepts/events.md)
- [Webhooks: subscriptions, secrets and rotation](concepts/webhooks.md)
- [Running several server copies: what is safe and what is not](concepts/scale-out.md)
- [Content storage: streaming, ranges and the planned chunked format](concepts/storage.md)
- [The `.pyn/` folder](concepts/workspace.md)
- [Git compatibility: where the CLI follows git and where it deliberately differs](concepts/git-compatibility.md)
- [Branches: branch heads, commits on a branch, exclusive paths across branches](concepts/branches.md)
- [Merging: merge base, merge commits, conflicts and resolution](concepts/merging.md)
- [Tamper-evident history: the frozen hash encoding, chained audit log, `pyn verify`](concepts/integrity.md)
- [Configuration: every environment variable](configuration.md)
- [Phase 1 scope](phase-1.md)

## API contract

[`generated/openapi.json`](generated/openapi.json) is the server's OpenAPI document, the contract `pyn-web` generates its
client from. It is generated, not edited: `make openapi` rewrites it, and `make check` fails when it is stale. Changes
are additive by default (new routes, new optional fields); removing or renaming something is a breaking change that
needs its own decision issue.

## Public boundary

`pyn` builds and runs with no private dependency (decision #140). Hosted-service code lives in a separate closed-source
application that uses only the documented API. `make boundary-check` (part of `make lint`, and a CI step) fails on
submodules, git or alternate-registry dependencies, path dependencies outside the repository, references to private
code, and files or content that look like secrets. Do not add any of these, and keep credentials out of the tree
(`.env` is ignored; `.env.example` holds placeholders only).
