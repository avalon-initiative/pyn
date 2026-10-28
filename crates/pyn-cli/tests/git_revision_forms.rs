//! `@{upstream}`, `@{push}` and `@{-N}` as start points and revisions, `-q` on `branch`/`checkout`/`switch`, tracking flags
//! that do not apply, and `show` of tags and of `<rev>:<path>` mixed with commits, compared with real git on twin
//! repositories by resulting refs, configuration, content and exit status class. Needs `git` on the PATH (tests only).

#[path = "common/oracle.rs"]
mod oracle;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::{TempDir, tempdir};

fn git(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .args(["-c", "gc.auto=0", "-c", "maintenance.auto=false"])
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_EDITOR", "true")
        .env("GIT_AUTHOR_NAME", "T")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "T")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .env("GIT_AUTHOR_DATE", "2020-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2020-01-01T00:00:00Z")
        .output()
        .expect("git is installed")
}

fn pyn(dir: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pyn"))
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("TZ", "UTC")
        .env("PYN_CONFIG_DIR", home.join("pyn-config"))
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("PYN_SERVER")
        .env_remove("PYN_REPO")
        .output()
        .unwrap()
}

fn ok(dir: &Path, home: &Path, args: &[&str]) -> String {
    let out = git(dir, home, args);
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn git_out(dir: &Path, home: &Path, args: &[&str]) -> String {
    String::from_utf8_lossy(&git(dir, home, args).stdout).into_owned()
}

/// Twin work trees cloned from one origin: `main` is checked out and tracks `origin/main`; `feat` has one more commit.
struct World {
    _tmp: TempDir,
    work: [PathBuf; 2],
    home: PathBuf,
}

impl World {
    fn new() -> Self {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let seed = root.join("seed");
        fs::create_dir_all(seed.join("d")).unwrap();
        ok(&seed, &home, &["init", "-q", "-b", "main"]);
        fs::write(seed.join("d/f.txt"), "one\n").unwrap();
        fs::write(seed.join("g.txt"), "g\n").unwrap();
        ok(&seed, &home, &["add", "."]);
        ok(&seed, &home, &["commit", "-q", "-m", "one"]);
        ok(&seed, &home, &["switch", "-q", "-c", "topic"]);
        fs::write(seed.join("t.txt"), "topic\n").unwrap();
        ok(&seed, &home, &["add", "."]);
        ok(&seed, &home, &["commit", "-q", "-m", "topic"]);
        ok(&seed, &home, &["switch", "-q", "main"]);
        let work = ["git", "pyn"].map(|n| root.join(n));
        for (i, w) in work.iter().enumerate() {
            let bare = root.join(format!("origin-{i}.git"));
            ok(
                &root,
                &home,
                &["init", "-q", "-b", "main", "--bare", bare.to_str().unwrap()],
            );
            ok(
                &seed,
                &home,
                &["push", "-q", bare.to_str().unwrap(), "main", "topic"],
            );
            ok(
                &root,
                &home,
                &["clone", "-q", bare.to_str().unwrap(), w.to_str().unwrap()],
            );
            ok(w, &home, &["switch", "-q", "-c", "feat"]);
            fs::write(w.join("h.txt"), "h\n").unwrap();
            ok(w, &home, &["add", "."]);
            ok(w, &home, &["commit", "-q", "-m", "three"]);
            ok(
                w,
                &home,
                &["tag", "-a", "-m", "release notes", "v1", "HEAD~1"],
            );
            ok(
                w,
                &home,
                &["tag", "-a", "-m", "tree tag", "tt", "HEAD^{tree}"],
            );
            ok(w, &home, &["tag", "light"]);
            ok(w, &home, &["switch", "-q", "main"]);
        }
        Self {
            _tmp: tmp,
            work,
            home,
        }
    }

    fn setup(&self, args: &[&str]) {
        for w in &self.work {
            ok(w, &self.home, args);
        }
    }

    /// Runs `args` with git and with pyn and compares the exit status class and the resulting refs, settings and files.
    fn same(&self, args: &[&str]) -> (Output, Output) {
        let g = git(&self.work[0], &self.home, args);
        let p = pyn(&self.work[1], &self.home, args);
        let ctx = format!(
            "{args:?}\ngit: {}\npyn: {}",
            String::from_utf8_lossy(&g.stderr),
            String::from_utf8_lossy(&p.stderr)
        );
        assert_eq!(
            oracle::status_class(g.status.code().unwrap()),
            oracle::status_class(p.status.code().unwrap()),
            "{ctx}"
        );
        assert_eq!(self.state(0), self.state(1), "{ctx}");
        (g, p)
    }

    /// Like `same`, and the output must match byte for byte.
    fn same_output(&self, args: &[&str]) {
        let (g, p) = self.same(args);
        assert_eq!(
            String::from_utf8_lossy(&g.stdout),
            String::from_utf8_lossy(&p.stdout),
            "{args:?}"
        );
    }

    fn state(&self, i: usize) -> String {
        let (w, h) = (&self.work[i], &self.home);
        let mut config: Vec<String> = git_out(w, h, &["config", "--get-regexp", "^branch\\."])
            .lines()
            .map(str::to_owned)
            .collect();
        config.sort();
        [
            git_out(w, h, &["symbolic-ref", "-q", "HEAD"]),
            git_out(
                w,
                h,
                &[
                    "for-each-ref",
                    "refs/heads",
                    "--format=%(refname) %(objectname)",
                ],
            ),
            config.join("\n"),
            git_out(w, h, &["status", "--porcelain"]),
        ]
        .join("|")
    }
}

fn succeeds(w: &World, args: &[&str]) {
    let (g, p) = w.same(args);
    assert_eq!(g.status.code(), Some(0), "{args:?}");
    assert_eq!(p.status.code(), Some(0), "{args:?}");
}

#[test]
fn start_points_written_as_marks_create_the_same_branch() {
    for args in [
        &["branch", "n", "@{upstream}"][..],
        &["branch", "n", "@{u}"],
        &["branch", "n", "@{U}"],
        &["branch", "n", "main@{u}"],
        &["branch", "n", "@{push}"],
        &["branch", "n", "main@{push}"],
        &["branch", "n", "HEAD@{0}"],
        &["checkout", "-b", "n", "@{u}"],
        &["checkout", "-B", "n", "@{u}"],
        &["checkout", "-b", "n", "--no-track", "@{u}"],
        &["checkout", "--track", "-b", "n", "@{u}"],
        &["switch", "-c", "n", "@{upstream}"],
        &["switch", "-C", "n", "@{push}"],
        &["switch", "--detach", "@{u}"],
        &["checkout", "--detach", "@{u}"],
        &["checkout", "@{u}"],
    ] {
        let w = World::new();
        succeeds(&w, args);
    }
}

#[test]
fn a_mark_that_names_the_previous_branch_is_a_start_point() {
    for args in [
        &["branch", "n", "@{-1}"][..],
        &["branch", "n", "@{-2}"],
        &["checkout", "-b", "n", "@{-1}"],
        &["switch", "-c", "n", "@{-1}"],
        &["switch", "-"],
        &["checkout", "-"],
        &["checkout", "@{-1}"],
        &["switch", "@{-1}"],
        &["switch", "--detach", "@{-1}"],
        &["checkout", "--track", "-b", "n", "@{-1}"],
        &["branch", "-t", "n", "@{-1}"],
        &["branch", "-u", "@{-1}"],
        &["branch", "--set-upstream-to=@{-1}", "feat"],
        &["branch", "-u", "@{-2}", "feat"],
    ] {
        let w = World::new();
        w.setup(&["switch", "-q", "feat"]);
        w.setup(&["switch", "-q", "topic"]);
        w.setup(&["switch", "-q", "main"]);
        succeeds(&w, args);
    }
}

#[test]
fn marks_without_a_target_are_refused_as_git_refuses_them() {
    for args in [
        &["branch", "n", "@{u}"][..],
        &["branch", "n", "feat@{u}"],
        &["branch", "n", "@{push}"],
        &["branch", "n", "@{-9}"],
        &["checkout", "-b", "n", "@{u}"],
        &["switch", "-c", "n", "@{-9}"],
        &["branch", "-u", "@{u}"],
        &["branch", "n", "@{bogus}"],
        &["branch", "n", "@{-0}"],
    ] {
        let w = World::new();
        w.setup(&["switch", "-q", "feat"]);
        let g = git(&w.work[0], &w.home, args);
        let p = pyn(&w.work[1], &w.home, args);
        assert_eq!(
            oracle::status_class(g.status.code().unwrap()),
            1,
            "{args:?}"
        );
        assert_eq!(
            oracle::status_class(p.status.code().unwrap()),
            1,
            "{args:?}"
        );
        assert_eq!(w.state(0), w.state(1), "{args:?}");
    }
}

#[test]
fn a_branch_made_from_a_mark_tracks_what_git_tracks() {
    let w = World::new();
    w.setup(&["config", "branch.feat.remote", "origin"]);
    w.setup(&["config", "branch.feat.merge", "refs/heads/topic"]);
    for args in [
        &["branch", "a", "feat@{u}"][..],
        &["branch", "c", "main@{upstream}"],
        &["branch", "--no-track", "d", "main@{u}"],
        &["branch", "--track", "e", "feat@{u}"],
        &["switch", "-c", "f", "feat@{u}"],
    ] {
        succeeds(&w, args);
    }
}

#[test]
fn revisions_with_marks_read_the_same_commits() {
    let w = World::new();
    w.setup(&["switch", "-q", "feat"]);
    w.setup(&["switch", "-q", "main"]);
    w.setup(&["config", "branch.feat.remote", "origin"]);
    w.setup(&["config", "branch.feat.merge", "refs/heads/topic"]);
    for args in [
        &[
            "rev-parse",
            "@{u}",
            "@{upstream}",
            "main@{push}",
            "@{-1}",
            "feat@{u}",
        ][..],
        &["rev-parse", "@{-1}^", "@{u}~1", "feat@{u}^{tree}"],
        &["log", "--format=%H %s", "@{u}"],
        &["log", "--format=%H", "@{-1}..@{u}"],
        &["log", "--format=%H", "-1", "feat@{upstream}"],
        &["diff", "--stat", "@{u}", "@{-1}"],
        &["diff", "--name-only", "@{-1}..@{u}"],
        &["show", "-s", "--format=%H", "@{u}", "@{-1}"],
        &["show", "@{-1}:h.txt"],
        &["show", "feat@{u}:t.txt"],
    ] {
        w.same_output(args);
    }
}

#[test]
fn merge_takes_a_mark() {
    let w = World::new();
    w.setup(&["switch", "-q", "-c", "work", "origin/topic"]);
    w.setup(&["switch", "-q", "main"]);
    w.setup(&["branch", "-q", "-u", "origin/topic", "main"]);
    succeeds(&w, &["merge", "--ff-only", "@{u}"]);
    succeeds(&w, &["merge", "-m", "again", "@{-1}"]);
}

#[test]
fn quiet_leaves_the_same_state_and_prints_no_progress() {
    for args in [
        &["branch", "-q", "n"][..],
        &["branch", "--quiet", "n", "origin/topic"],
        &["branch", "-q", "-t", "n", "origin/topic"],
        &["branch", "-q", "-d", "side"],
        &["branch", "-q", "-D", "feat"],
        &["branch", "-q", "-m", "feat", "other"],
        &["branch", "-q", "-u", "origin/topic", "feat"],
        &["checkout", "-q", "feat"],
        &["checkout", "--quiet", "-b", "n"],
        &["checkout", "-q", "-b", "n", "origin/topic"],
        &["checkout", "-q", "--detach", "feat"],
        &["checkout", "-q", "origin/topic"],
        &["checkout", "-q", "feat", "--", "h.txt"],
        &["checkout", "-q", "--", "g.txt"],
        &["checkout", "-q", "topic"],
        &["switch", "-q", "feat"],
        &["switch", "--quiet", "-c", "n"],
        &["switch", "-q", "-c", "n", "origin/topic"],
        &["switch", "-q", "-d", "feat"],
        &["switch", "-q", "-C", "feat", "main"],
        &["switch", "-qc", "n"],
    ] {
        let w = World::new();
        w.setup(&["branch", "side"]);
        let (g, p) = w.same(args);
        assert_eq!(g.status.code(), Some(0), "{args:?}");
        assert!(p.stdout.is_empty(), "{args:?}: stdout {:?}", p.stdout);
        assert!(p.stderr.is_empty(), "{args:?}: stderr {:?}", p.stderr);
    }
}

#[test]
fn quiet_still_reports_failures() {
    for args in [
        &["branch", "-q", "feat", "main"][..],
        &["checkout", "-q", "nope"],
        &["switch", "-q", "nope"],
        &["switch", "-q", "origin/topic"],
        &["branch", "-q", "-d", "nope"],
        &["branch", "-q", "-u", "nope"],
    ] {
        let w = World::new();
        let g = git(&w.work[0], &w.home, args);
        let p = pyn(&w.work[1], &w.home, args);
        assert_eq!(
            oracle::status_class(g.status.code().unwrap()),
            oracle::status_class(p.status.code().unwrap()),
            "{args:?}"
        );
        assert_eq!(
            oracle::status_class(p.status.code().unwrap()),
            1,
            "{args:?}"
        );
        assert!(!p.stderr.is_empty(), "{args:?}");
        assert_eq!(w.state(0), w.state(1), "{args:?}");
    }
}

#[test]
fn tracking_flags_are_ignored_where_nothing_is_created() {
    for args in [
        &["branch", "-t"][..],
        &["branch", "--no-track", "-l"],
        &["branch", "-t", "-a"],
        &["branch", "-t", "-r"],
        &["branch", "--no-track", "-v"],
        &["branch", "-t", "--show-current"],
        &["branch", "-t", "-d", "side"],
        &["branch", "--track=inherit", "-d", "side"],
        &["branch", "-t", "-D", "feat"],
        &["branch", "-t", "-m", "feat", "other"],
        &["branch", "--no-track", "-m", "feat", "other"],
        &["branch", "-t", "-c", "feat", "other"],
        &["branch", "-t", "-u", "origin/topic", "feat"],
        &["branch", "-t", "--unset-upstream"],
    ] {
        let w = World::new();
        w.setup(&["branch", "side"]);
        succeeds(&w, args);
    }
}

#[test]
fn tracking_flags_with_paths_are_refused() {
    for args in [
        &["checkout", "-t", "--", "g.txt"][..],
        &["checkout", "--no-track", "--", "g.txt"],
        &["checkout", "-t", "feat", "--", "h.txt"],
        &["checkout", "-b", "n", "--track", "--", "g.txt"],
    ] {
        let w = World::new();
        w.same(args);
        let g = git(&w.work[0], &w.home, args);
        assert_eq!(
            oracle::status_class(g.status.code().unwrap()),
            1,
            "{args:?}"
        );
    }
}

#[test]
fn show_prints_tags_and_the_objects_they_name() {
    let w = World::new();
    for args in [
        &["show", "v1"][..],
        &["show", "-s", "v1"],
        &["show", "--oneline", "v1"],
        &["show", "--stat", "v1"],
        &["show", "tt"],
        &["show", "light"],
        &["show", "v1", "HEAD"],
        &["show", "HEAD", "v1"],
        &["show", "v1", "v1"],
        &["show", "tt", "v1", "light"],
        &["show", "tt:", "v1"],
        &["show", "--date=iso", "v1"],
    ] {
        w.same_output(args);
    }
    for pretty in [
        "oneline",
        "short",
        "medium",
        "full",
        "fuller",
        "raw",
        "format:%H%n",
        "tformat:%s",
    ] {
        let pretty = format!("--pretty={pretty}");
        for target in ["v1", "tt"] {
            w.same_output(&["show", "-s", &pretty, target]);
        }
    }
}

#[test]
fn show_mixes_blobs_trees_and_commits_in_the_order_given() {
    let w = World::new();
    for args in [
        &["show", "HEAD:g.txt", "HEAD"][..],
        &["show", "HEAD", "HEAD:g.txt"],
        &["show", "-s", "HEAD:g.txt", "HEAD"],
        &["show", "HEAD:d", "HEAD"],
        &["show", "HEAD", "HEAD:d"],
        &["show", "HEAD~1", "HEAD:d", "HEAD"],
        &["show", "--oneline", "HEAD", "HEAD:g.txt", "topic"],
        &["show", "--format=%s", "HEAD:d", "HEAD"],
        &["show", "HEAD:g.txt", "v1", "HEAD:d"],
        &["show", "HEAD^{tree}"],
        &["show", "HEAD^{tree}", "HEAD"],
        &["show", "--pretty=oneline", "HEAD:d"],
        &["show", "--pretty=raw", "HEAD:d"],
        &["show", "--oneline", "HEAD:d"],
        &["show", "-s", "HEAD:d"],
        &["show", "HEAD", "HEAD"],
        &["show", "-s", "--oneline", "HEAD", "HEAD~0", "main"],
    ] {
        w.same_output(args);
    }
}
