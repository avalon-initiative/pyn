# CI and the pinned git

Many git-home tests use a real `git` as an oracle (decision #718). CI does not use the runner image's git, which drifts
between releases: every job that runs tests builds the pinned version from the kernel.org tarball with the composite
action `.github/actions/setup-git`, caches it by version, puts it first on `PATH` and prints `git --version` in the log.
The action fails the job if the built git is not exactly the pinned version. `ci.yml` (`test` shards and `live`) and
`release.yml` (`verify` and `live`) use it.

The build keeps libcurl, because the HTTP transport tests need `git-remote-http`; gettext, Tcl/Tk, Perl, Python and expat are
off.

## Moving the pin

1. Change `version` (the `default`) in `.github/actions/setup-git/action.yml`. It is the only place the version is written.
2. Run the suite locally against that git and fix any oracle test that drifted (tests assert semantics, not exact output).
3. Open the PR: the cache key includes the version, so CI builds the new git once and reuses it afterwards.

Local runs use whatever `git` is on your `PATH`; install the pinned version there to reproduce CI.
