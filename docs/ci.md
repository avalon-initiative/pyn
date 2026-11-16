# CI and the pinned git

Many git-home tests use a real `git` as an oracle (decision #718). CI does not use the runner image's git, which drifts
between releases: every job that runs tests builds the pinned version from the kernel.org tarball with the composite
action `.github/actions/setup-git`, caches it by version, puts it first on `PATH` and prints `git --version` in the log.
The action fails the job if the built git is not exactly the pinned version. `ci.yml` (`test` shards and `live`) and
`release.yml` (`gate` and `live`) use it.

The build keeps libcurl, because the HTTP transport tests need `git-remote-http`; gettext, Tcl/Tk, Perl, Python and expat are
off.

## Moving the pin

1. Change `version` (the `default`) in `.github/actions/setup-git/action.yml`. It is the only place the version is written.
2. Run the suite locally against that git and fix any oracle test that drifted (tests assert semantics, not exact output).
3. Open the PR: the cache key includes the version, so CI builds the new git once and reuses it afterwards.

Local runs use whatever `git` is on your `PATH`; install the pinned version there to reproduce CI.

## Comparing pyn with git in tests

pyn and git must behave the same, not word things the same. A twin test (`crates/pyn-cli/tests/common/behavior.rs`) runs one
command with both tools on two copies of a repository and compares what they did:

- Repository state, read with real git plumbing on both copies (`Snapshot`): refs and `HEAD`, reachable objects, index
  entries (`mode id stage path`), work tree files with modes, local config, and the state of an operation in progress
  (`MERGE_HEAD`, `rebase-merge`, `rebase-apply`, ...).
- Exit status: the stable codes (0, 1, 2, 128, 129) exactly; any other code by `status_class`.
- Standard output only for a machine format, marked explicitly with `Format::Machine` (porcelain, plumbing, `-z`,
  `--numstat`, `--raw`, `--name-only`, `ls-files`, `rev-parse`).
- Standard error: not compared by default (git moves advice between stdout and stderr across releases). A caller passes
  `Stderr::Quiet` only for a command git leaves silent in every release, such as a clean `apply --check`; both must then
  print nothing.

Never compare: message wording, hints, human-readable output (use `Ran::has_facts` for the few facts it must state),
stderr text, abbreviation widths, or anything that branches on git's version or one release's output. Local and CI gits
differ (see above), and pyn's own messages are free to differ.

A test builds a `Twin` (the `patchfix` helpers for the patch family), then `twin.same(sub, args, stdin)` for human output
or `twin.same_as(Format::Machine, Stderr::Ignored, sub, args, stdin)`; both return the two outputs for extra checks. For a custom flow call
`assert_same_behavior(what, format, stderr, (&git_out, &git_dir), (&pyn_out, &pyn_dir))` or `assert_same_state(what, &git_dir,
&pyn_dir)`. A test that compares wording instead asserts state, exit status and facts.

`scripts/check_oracle_text.py` (run by `make lint` and CI, self-test in `scripts/test_check_oracle_text.py`) fails a test that compares git's stderr or
stdout text outside the shared comparator, branches on git's version, or rewrites `pyn` to `git` in output (decision #1032). Files that
still do are listed in `scripts/oracle_text_allowlist.txt`; the list only shrinks, so a listed file that is clean must be removed. A
single justified line carries `// oracle-text: allowed <reason>`.
