# Releasing

A release is a `v<MAJOR>.<MINOR>.<PATCH>[-title-slug]` tag on `main`. Pushing the tag starts the
[release workflow](../.github/workflows/release.yml); nothing publishes without a `v*` tag, and there is no manual
publish path. Provisional: one pre-release, `v0.1.0-alpha`, has been cut.

The optional suffix is a free-text title slug (`v1.0.0-first-stable`). It does not change what is published, except that a
suffix starting with `alpha`, `beta`, `rc` or `pre` (any case, then end, `.`, `-` or a digit; `v1.0.0-rc.1`,
`v1.0.0-beta2`) marks the GitHub Release as a pre-release. The Release title is the tag.

## Cutting a release

`main` is protected, so the version bump goes through a pull request and the tag goes on the merged commit.

1. On an up-to-date, clean `main`, run `make release` (optionally `VER=1.0.0 TITLE="First stable"`; `make release-skip-tests`
   or `SKIP_CHECKS=1` skips the gate, `FORCE_CHECKS=1` always runs it). [`scripts/release.sh`](../scripts/release.sh)
   requires `gh` to be logged in, runs `make check` unless [CI already passed on this tree](#when-the-gate-is-skipped), asks for the version (it must be greater than the current one) and an optional title, and refuses a tag
   that already exists locally or on origin, and any existing tag for the same `X.Y.Z`, whatever its title, since the image tag would be overwritten.
2. It creates `release-v<version>`, bumps the workspace version and the intra-workspace requirements, refreshes
   `Cargo.lock`, regenerates `docs/generated/openapi.json` (it embeds the version) and commits
   `[noissue] - Release v<version>[-slug]`. After a confirmation it pushes the branch and opens the pull request; declining
   prints the two commands to run later. Nothing is pushed to `main`, and a failure after the branch exists prints how to
   remove it.
3. Merge the pull request (squash).
4. On the updated `main`, run `make release-publish`. It checks that HEAD is the squash-merged release commit and that the
   version files match, asks for confirmation, then creates the annotated tag on that commit and pushes only the tag.

`YES=1` answers the confirmation questions for scripted use; the other checks still apply, and a missing `VER` or `TITLE`
fails instead of prompting when there is no terminal. `make release-test` exercises the scripts against a throwaway
repository with a stub `gh`; it is part of `make check`.

### Both repositories at once

Maintainers with a sibling `pyn-web` checkout can release both in one go: `make release-all VER=1.0.0 TITLE="First stable"`
([`scripts/release-all.sh`](../scripts/release-all.sh); the checkout is `../pyn-web` unless `PYN_WEB_DIR` says otherwise).
It refuses to start unless both repositories are on `main` with a clean tree and both are at the same current version
(and `VER` is greater). It then runs `release.sh` in each (with `YES=1`), waits for both pull requests to turn green
(`mergeStateStatus` is `CLEAN`; a failed check or a pull request that fell behind aborts, `RELEASE_WAIT_MAX` seconds at most),
squash-merges both, checks that both versions on `main` equal `VER`, and tags pyn-web first and pyn second, since the
pyn-web workflow builds the web image the pyn smoke test pulls.

- **It publishes tags** (images, binaries, GitHub Releases), which cannot be undone, so it prints that and asks for
  confirmation unless `YES=1`.
- A failure part-way leaves what already happened in place (open or merged pull requests, a tag in one repository); finish
  the rest by hand with the single-repository steps above.
- pyn-common-ui is not released by it. It must be released first when it changed; the script only warns when its `origin/main`
  version differs from the one pinned in pyn-web's `package.json` (and says so when it cannot look).
- `SKIP_CHECKS` and `FORCE_CHECKS` pass through to both repositories' `release.sh`.

## What the pipeline does

The `preflight` job (cheap) refuses a tag whose commit is not on `main`, checks the tag against the workspace version
([`scripts/verify-release-version.sh`](../scripts/verify-release-version.sh)) and decides whether the gate must run (see
[below](#when-the-gate-is-skipped)). Everything after it starts at once, except the two public steps (4 and 5), which wait
for the gate.

1. `gate` (`make check`: fmt, clippy, docs links, public boundary, tests) and `live` (`make test-live` against a Postgres 16
   service) run unless preflight found a passing CI run on the same tree; they are skipped or run together.
2. Builds release binaries and packs each program in its own archive (the program plus `LICENSE`), so people can
   download only the CLI, only the server, or both:

   | Platform | CLI archive | Server archive |
   | --- | --- | --- |
   | Linux x86_64 | `pyn-cli-<tag>-linux-x86_64.tar.gz` | `pyn-server-<tag>-linux-x86_64.tar.gz` |
   | Linux arm64 | `pyn-cli-<tag>-linux-arm64.tar.gz` | `pyn-server-<tag>-linux-arm64.tar.gz` |
   | macOS Apple silicon | `pyn-cli-<tag>-macos-arm64.tar.gz` | none (the server runs on Linux or in Docker) |

   Windows builds are not produced yet. A target that fails to build does not stop the release: the GitHub Release is
   created with the archives that did build, and the run shows the failed target.
3. Builds the server image on a native runner per architecture and pushes each by digest, untagged: nothing can pull it
   by a version yet.
4. Once the image is built and the gate passed or was skipped, publishes the multi-arch manifest
   `ghcr.io/avalon-initiative/pyn-core:v<MAJOR>.<MINOR>.<PATCH>` (linux/amd64 and linux/arm64). The image tag is the plain
   version only, never the title (`v1.0.0-first-stable` publishes `pyn-core:v1.0.0`); there is no `latest` tag.
5. Once the same condition holds and the binaries are built, creates the GitHub Release with the archives, a `SHA256SUMS` file and generated notes; the title is the tag, and
   only an `alpha`, `beta`, `rc` or `pre` suffix marks it as a pre-release.
6. In parallel with step 5, after step 4, runs the [smoke test](#smoke-test) against the published images. It is not a dependency of the
   GitHub Release and never blocks it.

Invariant: no public tag or Release exists unless the gate passed or was legitimately skipped. Only untagged digests
(unreachable by version) are pushed before that. The image and archives are built from the same tagged commit. A failed run
publishes no release. If it fails after the manifest in step 4, fix forward with a new patch version rather than re-tagging.

After the first push, the org owner must set the `pyn-core` package on ghcr.io to public (GitHub Packages settings);
it starts private, and pulls need a login until then.

## When the gate is skipped

The gate would repeat what CI already ran on the release pull request. [`scripts/ci-green-for-tree.sh`](../scripts/ci-green-for-tree.sh)
`[<commit>]` exits 0 only when the commit came from a pull request whose head has the same git tree and whose latest `CI`
run concluded `success` (the `ci` workflow covers the unit, Postgres and image jobs, so `gate` and `live` are skipped
together). It prints one line saying which pull request and run satisfied it, or why not; any error is non-zero.

- The tag workflow runs it on the tagged commit (the `preflight` job's `gated` output is `skip` or `run`).
- `release.sh` runs it on `origin/main`, the commit the release branch is cut from, and skips the local `make check` with a message.
  `FORCE_CHECKS=1` runs the gate regardless; `SKIP_CHECKS=1` skips it without looking; both together is refused.

It fails safe: when the check cannot decide (another pull request merged in between so the tree differs, a failed or
missing CI run, an API error), the gate simply runs. A squash merge of an unchanged release branch keeps the pull request's
tree, which is the normal case.

Outcomes of the tag workflow, with `gate` and `live` as one column (a gate that was skipped counts as success):

| preflight | gate and live | image | `manifest` | `release` |
| --- | --- | --- | --- | --- |
| failed | skipped | skipped | skipped | skipped |
| ok, `gated=skip` | skipped | ok | runs | runs (binaries need not all succeed) |
| ok, `gated=run` | passed | ok | runs | runs |
| ok | failed or cancelled | any | skipped | skipped |
| ok | passed or skipped | failed | skipped | skipped |

## Smoke test

[`scripts/smoke-release.sh`](../scripts/smoke-release.sh) checks what was actually published: it takes `deploy/docker-compose.yml`
from the checkout, writes a throwaway `.env` (`PYN_VERSION=v<version>`, random secrets, a free web port) in a temporary
directory and starts the stack under its own compose project, so it does not touch a real deployment (it still needs host
port 7878 for the server). It then checks `/healthz` and the web app, completes first-run setup with the token from the
server log through `POST /v1/setup`, and confirms `GET /v1/setup` reports `initialised`. It always removes the stack and its
volumes (`docker compose down --volumes`), and on failure prints `compose ps` and the logs first.

The release workflow runs it as the `smoke` job once the images are published ([`smoke.yml`](../.github/workflows/smoke.yml)).
The web image is built by pyn-web's own tag workflow, which can finish after this one, so the script retries the pull for
about ten minutes (`SMOKE_PULL_ATTEMPTS`, `SMOKE_PULL_WAIT`).

A failed smoke run means look at it, not that the release is bad: the job is not part of the gate, the GitHub Release is
created regardless, and the cause is as often a late web image or a runner problem as a defect. Read the logs the job
prints before deciding anything.

Run it by hand on a machine with Docker (`docker compose`, `curl`, `openssl`), or trigger the workflow:

```bash
scripts/smoke-release.sh 0.1.0        # or: make release-smoke VER=0.1.0
gh workflow run smoke.yml -f version=0.1.0
```

## Using a release

- Server: set `PYN_IMAGE=ghcr.io/avalon-initiative/pyn-core:v<version>` (for example `:v1.0.0`; the title is not part of the image tag) in `.env` and follow [deployment](deployment.md); an
  upgrade is a new tag in the same place.
- CLI and server binaries: download the `pyn-cli-` and/or `pyn-server-` archive for your platform from the release page and check it with
  `sha256sum --check --ignore-missing SHA256SUMS`.
