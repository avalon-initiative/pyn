# Releasing

A release is a `v<MAJOR>.<MINOR>.<PATCH>[-title-slug]` tag on `main`. Pushing the tag starts the
[release workflow](../.github/workflows/release.yml); nothing publishes without a `v*` tag, and there is no manual
publish path. Provisional: no release has been cut yet.

The optional suffix is a free-text title slug (`v1.0.0-first-stable`). It does not change what is published, except that a
suffix starting with `alpha`, `beta`, `rc` or `pre` (any case, then end, `.`, `-` or a digit; `v1.0.0-rc.1`,
`v1.0.0-beta2`) marks the GitHub Release as a pre-release. The Release title is the tag.

## Cutting a release

`main` is protected, so the version bump goes through a pull request and the tag goes on the merged commit.

1. On an up-to-date, clean `main`, run `make release` (optionally `VER=1.0.0 TITLE="First stable"`; `make release-skip-tests`
   or `SKIP_CHECKS=1` skips the gate). [`scripts/release.sh`](../scripts/release.sh) requires `gh` to be logged in, runs
   `make check`, asks for the version (it must be greater than the current one) and an optional title, and refuses a tag
   that already exists locally or on origin.
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

## What the pipeline does

1. Refuses a tag whose commit is not on `main`.
2. Checks the tag against the workspace version ([`scripts/verify-release-version.sh`](../scripts/verify-release-version.sh)).
3. Runs the same checks CI runs: `make check` (fmt, clippy, docs links, public boundary, tests) and, against a Postgres 16
   service, `make test-live`. Nothing below is built or pushed unless both pass.
4. Builds release binaries and packs `pyn-<tag>-<target>.tar.gz` (binaries plus `LICENSE`):

   | Target | Contents |
   | --- | --- |
   | `x86_64-unknown-linux-gnu` | `pyn-server`, `pyn` |
   | `aarch64-unknown-linux-gnu` | `pyn-server`, `pyn` |
   | `aarch64-apple-darwin` | `pyn` (CLI only) |

   Windows builds are not produced yet.
5. Builds the server image on a native runner per architecture and publishes the multi-arch manifest
   `ghcr.io/avalon-initiative/pyn-core:v<MAJOR>.<MINOR>.<PATCH>` (linux/amd64 and linux/arm64), and also
   `pyn-core:<tag>` when the tag has a suffix. There is no `latest` tag.
6. Creates the GitHub Release with the archives, a `SHA256SUMS` file and generated notes; the title is the tag, and
   only an `alpha`, `beta`, `rc` or `pre` suffix marks it as a pre-release.

The image and archives are built from the same tagged commit. A failed run publishes no release. If it fails after the
image push in step 5, fix forward with a new patch version rather than re-tagging.

After the first push, the org owner must set the `pyn-core` package on ghcr.io to public (GitHub Packages settings);
it starts private, and pulls need a login until then.

## Using a release

- Server: set `PYN_IMAGE=ghcr.io/avalon-initiative/pyn-core:v<version>` (for example `:v1.0.0`, whatever the tag's title) in `.env` and follow [deployment](deployment.md); an
  upgrade is a new tag in the same place.
- CLI and server binaries: download the archive for your platform from the release page and check it with
  `sha256sum --check --ignore-missing SHA256SUMS`.
