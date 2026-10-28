//! `checkout <name>` and `switch <name>` creating a tracking branch from a remote-tracking one, and `show <rev>:<path>`,
//! compared with real git on twin repositories by resulting state and exit status class. Needs `git` on the PATH (tests only).

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
        .env("GIT_AUTHOR_NAME", "T")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "T")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
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

/// Twin work trees that each have the remotes `origin` and `other`, both holding `main` and `topic`.
struct World {
    _tmp: TempDir,
    work: [PathBuf; 2],
    home: PathBuf,
}

impl World {
    fn new(topic_on: &[&str]) -> Self {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let seed = root.join("seed");
        fs::create_dir_all(seed.join("d")).unwrap();
        ok(&seed, &home, &["init", "-q", "-b", "main"]);
        fs::write(seed.join("d/f.txt"), "one\n").unwrap();
        ok(&seed, &home, &["add", "."]);
        ok(&seed, &home, &["commit", "-q", "-m", "one"]);
        ok(&seed, &home, &["switch", "-q", "-c", "topic"]);
        fs::write(seed.join("t.txt"), "topic\n").unwrap();
        ok(&seed, &home, &["add", "."]);
        ok(&seed, &home, &["commit", "-q", "-m", "topic"]);
        ok(&seed, &home, &["switch", "-q", "main"]);
        let work = ["git", "pyn"].map(|n| root.join(n));
        for (i, w) in work.iter().enumerate() {
            fs::create_dir_all(w).unwrap();
            ok(w, &home, &["init", "-q", "-b", "main"]);
            for name in ["origin", "other"] {
                let bare = root.join(format!("{name}-{i}.git"));
                ok(
                    &root,
                    &home,
                    &["init", "-q", "--bare", bare.to_str().unwrap()],
                );
                ok(
                    &seed,
                    &home,
                    &["push", "-q", bare.to_str().unwrap(), "main"],
                );
                if topic_on.contains(&name) {
                    ok(
                        &seed,
                        &home,
                        &["push", "-q", bare.to_str().unwrap(), "topic"],
                    );
                }
                ok(w, &home, &["remote", "add", name, bare.to_str().unwrap()]);
            }
            ok(w, &home, &["fetch", "-q", "--all"]);
            ok(w, &home, &["reset", "-q", "--hard", "origin/main"]);
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

    /// Runs `args` with git and with pyn and compares status class, the branch checked out, the commit and tracking.
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
        let state = |i: usize| {
            let (w, h) = (&self.work[i], &self.home);
            let facts = [
                git_out(w, h, &["symbolic-ref", "-q", "HEAD"]),
                git_out(w, h, &["rev-parse", "HEAD"]),
                git_out(
                    w,
                    h,
                    &[
                        "for-each-ref",
                        "refs/heads",
                        "--format=%(refname) %(objectname) %(upstream)",
                    ],
                ),
                git_out(w, h, &["config", "--get-regexp", "^branch\\."]),
                git_out(w, h, &["status", "--porcelain"]),
            ];
            facts.join("|")
        };
        assert_eq!(state(0), state(1), "{ctx}");
        (g, p)
    }
}

fn git_out(dir: &Path, home: &Path, args: &[&str]) -> String {
    String::from_utf8_lossy(&git(dir, home, args).stdout).into_owned()
}

fn created(w: &World, args: &[&str], branch: &str, remote: &str) {
    let (_, p) = w.same(args);
    assert_eq!(
        p.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&p.stderr)
    );
    let cfg = git_out(
        &w.work[1],
        &w.home,
        &["config", &format!("branch.{branch}.remote")],
    );
    assert_eq!(cfg.trim(), remote);
}

fn refused(w: &World, args: &[&str]) {
    let (_, p) = w.same(args);
    assert_ne!(p.status.code(), Some(0), "{args:?} should be refused");
    let head = git_out(&w.work[1], &w.home, &["symbolic-ref", "HEAD"]);
    assert_eq!(head.trim(), "refs/heads/main");
}

#[test]
fn a_single_matching_remote_branch_is_guessed() {
    for args in [&["checkout", "topic"][..], &["switch", "topic"][..]] {
        created(&World::new(&["origin"]), args, "topic", "origin");
    }
    created(
        &World::new(&["other"]),
        &["checkout", "topic"],
        "topic",
        "other",
    );
    created(
        &World::new(&["origin"]),
        &["checkout", "--guess", "topic"],
        "topic",
        "origin",
    );
    created(
        &World::new(&["origin"]),
        &["checkout", "topic", "--"],
        "topic",
        "origin",
    );
}

#[test]
fn a_name_that_matches_two_remotes_is_refused_unless_a_default_remote_picks() {
    for args in [&["checkout", "topic"][..], &["switch", "topic"][..]] {
        refused(&World::new(&["origin", "other"]), args);
    }
    for args in [&["checkout", "topic"][..], &["switch", "topic"][..]] {
        let w = World::new(&["origin", "other"]);
        w.setup(&["config", "checkout.defaultRemote", "other"]);
        created(&w, args, "topic", "other");
    }
    let w = World::new(&["origin"]);
    w.setup(&["config", "checkout.defaultRemote", "other"]);
    created(&w, &["checkout", "topic"], "topic", "origin");
}

#[test]
fn guessing_can_be_turned_off_by_flag_and_setting() {
    for args in [
        &["checkout", "--no-guess", "topic"][..],
        &["switch", "--no-guess", "topic"][..],
        &["checkout", "--detach", "topic"][..],
        &["checkout", "nothing"][..],
        &["switch", "nothing"][..],
    ] {
        refused(&World::new(&["origin"]), args);
    }
    for args in [&["checkout", "topic"][..], &["switch", "topic"][..]] {
        let w = World::new(&["origin"]);
        w.setup(&["config", "checkout.guess", "false"]);
        refused(&w, args);
    }
    let w = World::new(&["origin"]);
    w.setup(&["config", "checkout.guess", "false"]);
    created(&w, &["checkout", "--guess", "topic"], "topic", "origin");
}

#[test]
fn a_file_of_that_name_is_a_path_and_a_local_branch_wins() {
    let w = World::new(&["origin"]);
    for dir in &w.work {
        fs::write(dir.join("topic"), "file\n").unwrap();
    }
    w.setup(&["add", "topic"]);
    w.setup(&["commit", "-q", "-m", "file"]);
    refused(&w, &["checkout", "topic"]);
    w.same(&["checkout", "--no-guess", "topic"]);
    let w = World::new(&[]);
    for dir in &w.work {
        fs::write(dir.join("topic"), "file\n").unwrap();
    }
    w.setup(&["add", "topic"]);
    w.setup(&["commit", "-q", "-m", "file"]);
    w.same(&["checkout", "topic"]);
    let w = World::new(&["origin"]);
    w.setup(&["branch", "topic", "main"]);
    w.same(&["checkout", "topic"]);
    let w = World::new(&["origin"]);
    w.setup(&["branch", "topic", "main"]);
    w.same(&["switch", "topic"]);
}

#[test]
fn show_prints_a_blob_or_a_tree_named_by_revision_and_path() {
    let w = World::new(&["origin"]);
    for args in [
        &["show", "HEAD:d/f.txt"][..],
        &["show", "main:d/f.txt"][..],
        &["show", "origin/topic:t.txt"][..],
        &["show", "HEAD:d"][..],
        &["show", "HEAD:"][..],
        &["show", "HEAD:d/f.txt", "origin/topic:t.txt"][..],
        &["show", ":d/f.txt"][..],
    ] {
        let g = git(&w.work[0], &w.home, args);
        let p = pyn(&w.work[1], &w.home, args);
        assert_eq!(g.status.code(), Some(0), "{args:?}");
        assert_eq!(
            p.status.code(),
            Some(0),
            "{args:?}: {}",
            String::from_utf8_lossy(&p.stderr)
        );
        assert_eq!(g.stdout, p.stdout, "{args:?}");
    }
    let sub = w.work[0].join("d");
    let g = git(&sub, &w.home, &["show", "HEAD:./f.txt"]);
    let p = pyn(&w.work[1].join("d"), &w.home, &["show", "HEAD:./f.txt"]);
    assert_eq!(g.stdout, p.stdout);
    for args in [&["show", "HEAD:missing"][..], &["show", "nope:d"][..]] {
        let g = git(&w.work[0], &w.home, args);
        let p = pyn(&w.work[1], &w.home, args);
        assert_ne!(g.status.code(), Some(0));
        assert_eq!(
            oracle::status_class(g.status.code().unwrap()),
            oracle::status_class(p.status.code().unwrap())
        );
    }
}
