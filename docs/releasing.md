# Releasing

A release is a `v<MAJOR>.<MINOR>.<PATCH>[-label]` tag on `main`. Pushing the tag starts the
[release workflow](../.github/workflows/release.yml); nothing publishes without a `v*` tag, and there is no manual
publish path. Provisional: no release has been cut yet.

## Cutting a release

The maintainer runs `make release` on an up-to-date `main`. [`scripts/release.sh`](../scripts/release.sh) runs
`make check`, asks for the next version and an optional label (`alpha`, `rc.1`), bumps the workspace version and the
intra-workspace requirements, regenerates `docs/generated/openapi.json` (it embeds the version), commits `Release v<version>`
and tags it. Push the commit through a pull request as usual, then push the tag from the merged `main` commit.

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
   `ghcr.io/avalon-initiative/pyn-core:<tag>` (linux/amd64 and linux/arm64). There is no `latest` tag.
6. Creates the GitHub Release with the archives, a `SHA256SUMS` file and generated notes; a tag with a label is marked
   as a pre-release.

The image and archives are built from the same tagged commit. A failed run publishes no release. If it fails after the
image push in step 5, fix forward with a new patch version rather than re-tagging.

After the first push, the org owner must set the `pyn-core` package on ghcr.io to public (GitHub Packages settings);
it starts private, and pulls need a login until then.

## Using a release

- Server: set `PYN_IMAGE=ghcr.io/avalon-initiative/pyn-core:<tag>` in `.env` and follow [deployment](deployment.md); an
  upgrade is a new tag in the same place.
- CLI and server binaries: download the archive for your platform from the release page and check it with
  `sha256sum --check --ignore-missing SHA256SUMS`.
