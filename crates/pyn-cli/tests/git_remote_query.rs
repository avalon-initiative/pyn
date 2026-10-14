//! `pyn remote show`, `update`, `prune`, `set-head` and `set-branches` on a git repository against a real server (ssh
//! through a shim, smart HTTP through `git http-backend`), compared with the real `git remote` on a twin repository by
//! meaning: exit status, refs and configuration, and the output with its spacing collapsed. Real git is the oracle in
//! these tests only.

#[path = "../../pyn-gittransport/tests/common/gitsrv.rs"]
mod gitsrv;

use std::path::{Path, PathBuf};
use std::process::Command;

use gitsrv::*;
use tempfile::{TempDir, tempdir};

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

struct Twin {
    _tmp: TempDir,
    bare: PathBuf,
    work: PathBuf,
    git: PathBuf,
    pyn: PathBuf,
    home: PathBuf,
    shim: PathBuf,
    url: String,
    _server: Option<Server>,
}

fn out_of(o: std::process::Output) -> Out {
    Out {
        code: o.status.code().unwrap(),
        stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
    }
}

/// Lines with their spacing collapsed and the stale refs' full names shortened, so that git versions agree.
fn words(text: &str) -> Vec<String> {
    text.lines()
        .map(|l| {
            l.split_whitespace()
                .map(|w| w.strip_prefix("refs/remotes/origin/").unwrap_or(w))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

impl Twin {
    fn new(format: &str, via: Via) -> Self {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let bare = sample_bare(&root, format);
        let shim = ssh_shim(&root, false);
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let server = via.serve(&root);
        let url = via.url(&server, "served.git", &bare);
        let t = Self {
            work: root.join("seed"),
            git: root.join("git-side"),
            pyn: root.join("pyn-side"),
            _tmp: tmp,
            bare,
            home,
            shim,
            url,
            _server: server,
        };
        for dir in [&t.git, &t.pyn] {
            init(dir, format, false);
            run(dir, &["remote", "add", "origin", &t.url]);
        }
        t.setup_both(&["fetch", "-q", "origin"]);
        t
    }

    fn git_cmd(&self, dir: &Path, args: &[&str]) -> Out {
        let out = git(dir)
            .args(args)
            .env("HOME", &self.home)
            .env("GIT_SSH_COMMAND", &self.shim)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .unwrap();
        out_of(out)
    }

    fn pyn_cmd(&self, dir: &Path, args: &[&str]) -> Out {
        let out = Command::new(env!("CARGO_BIN_EXE_pyn"))
            .current_dir(dir)
            .args(args)
            .env_clear()
            .env("PATH", path_env())
            .env("HOME", &self.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_SSH_COMMAND", &self.shim)
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .output()
            .unwrap();
        out_of(out)
    }

    /// A real-git command on both twins, for setting a scenario up.
    fn setup_both(&self, args: &[&str]) {
        for dir in [&self.git, &self.pyn] {
            let out = self.git_cmd(dir, args);
            assert_eq!(out.code, 0, "git {args:?}: {}", out.stderr);
        }
    }

    fn configure(&self, key: &str, value: &str, add: bool) {
        for dir in [&self.git, &self.pyn] {
            run(
                dir,
                &[
                    "config",
                    if add { "--add" } else { "--replace-all" },
                    key,
                    value,
                ],
            );
        }
    }

    /// `remote <args>` on both twins; the exit status and the words of standard output must agree.
    fn both(&self, args: &[&str]) -> (Out, Out) {
        self.compare(args, true)
    }

    /// Like `both` but standard output is left out: git's wording for these messages changed between versions.
    fn both_state(&self, args: &[&str]) -> (Out, Out) {
        self.compare(args, false)
    }

    fn compare(&self, args: &[&str], stdout: bool) -> (Out, Out) {
        let mut full = vec!["remote"];
        full.extend(args);
        let g = self.git_cmd(&self.git, &full);
        let p = self.pyn_cmd(&self.pyn, &full);
        let ctx = format!(
            "{full:?}\ngit: {}{}\npyn: {}{}",
            g.stdout, g.stderr, p.stdout, p.stderr
        );
        assert_eq!(g.code, p.code, "{ctx}");
        if stdout {
            assert_eq!(words(&g.stdout), words(&p.stdout), "{ctx}");
        }
        assert_eq!(self.refs(&self.git), self.refs(&self.pyn), "{ctx}");
        assert_eq!(
            self.fetch_config(&self.git),
            self.fetch_config(&self.pyn),
            "{ctx}"
        );
        (g, p)
    }

    fn refs(&self, dir: &Path) -> String {
        run(dir, &["for-each-ref", "--format=%(refname) %(objectname)"])
            .lines()
            .filter(|l| l.starts_with("refs/remotes/") && !l.contains("/HEAD "))
            .map(|l| format!("{l}\n"))
            .collect()
    }

    fn fetch_config(&self, dir: &Path) -> String {
        let out = git(dir)
            .args(["config", "--get-regexp", r"^remote\..*\.fetch$"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn head_of(&self, dir: &Path) -> Option<String> {
        let out = git(dir)
            .args(["symbolic-ref", "-q", "refs/remotes/origin/HEAD"])
            .output()
            .unwrap();
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
    }

    /// Local branches that track `origin`, with `main` ahead of the remote.
    fn local_branches(&self) {
        self.setup_both(&["switch", "-q", "-c", "main", "origin/main"]);
        self.setup_both(&["switch", "-q", "-c", "side", "origin/feature"]);
        self.configure("branch.main.rebase", "true", false);
        self.configure("branch.side.remote", "origin", false);
        self.configure("branch.side.merge", "refs/heads/feature", false);
        for dir in [&self.git, &self.pyn] {
            run(dir, &["switch", "-q", "main"]);
            commit(dir, "local.txt", b"local\n", "local");
        }
    }

    /// The server moves on: a new branch, a lost branch, and `main` ahead.
    fn server_moves(&self) {
        run(&self.bare, &["branch", "extra", "main"]);
        run(&self.bare, &["branch", "-D", "feature"]);
        run(&self.work, &["switch", "-q", "main"]);
        commit(&self.work, "d.txt", b"d\n", "four");
        run(
            &self.work,
            &["push", "-q", self.bare.to_str().unwrap(), "main"],
        );
    }
}

#[test]
fn show_matches_git_for_every_branch_state() {
    for (format, via) in ALL_WAYS {
        let t = Twin::new(format, via);
        t.local_branches();
        t.both(&["show", "origin"]);
        t.server_moves();
        let (_, p) = t.both(&["show", "origin"]);
        assert!(p.stdout.contains("HEAD branch: main"), "{}", p.stdout);
        assert!(
            p.stdout
                .contains("new (next fetch will store in remotes/origin)")
        );
        assert!(
            p.stdout
                .contains("stale (use 'git remote prune' to remove)")
        );
        assert!(p.stdout.contains("main pushes to main"));
        t.both(&["show", "-n", "origin"]);
    }
}

#[test]
fn show_covers_push_specs_mirror_and_pull_variants() {
    let t = Twin::new("sha1", Via::Ssh);
    t.local_branches();
    t.configure("branch.side.merge", "refs/heads/main", true);
    t.both(&["show", "origin"]);
    for mode in ["interactive", "merges", "true", "false"] {
        t.configure("branch.main.rebase", mode, false);
        t.both(&["show", "origin"]);
    }
    t.configure(
        "remote.origin.push",
        "+refs/heads/main:refs/heads/mm",
        false,
    );
    t.configure("remote.origin.push", ":refs/heads/feature", true);
    t.configure("remote.origin.push", "refs/heads/*:refs/heads/*", true);
    t.both(&["show", "origin"]);
    t.both(&["show", "-n", "origin"]);
    t.configure(
        "remote.origin.push",
        "refs/heads/side:refs/heads/feature",
        false,
    );
    t.both(&["show", "origin"]);
    t.configure("remote.origin.mirror", "true", false);
    t.both(&["show", "origin"]);
    t.both(&["show", "-n", "origin"]);
    t.configure(
        "remote.origin.fetch",
        "+refs/heads/main:refs/remotes/origin/main",
        false,
    );
    t.both(&["show", "origin"]);
    t.both(&["show", "-n", "origin"]);
    t.configure("remote.origin.pushurl", "p1", false);
    t.configure("remote.origin.pushurl", "p2", true);
    t.both(&["show", "-n", "origin"]);
}

#[test]
fn show_names_unknown_remotes_and_lists_without_a_name() {
    let t = Twin::new("sha1", Via::Ssh);
    let (_, p) = t.both(&["show", "-n", "nonesuch"]);
    assert!(p.stdout.contains("* remote nonesuch"), "{}", p.stdout);
    t.both(&["show"]);
    let (_, p) = t.both(&["show", "-n", "nonesuch", "origin"]);
    assert!(p.stdout.contains("* remote origin"), "{}", p.stdout);
    let (_, p) = t.both(&["show", "nonesuch"]);
    assert_eq!(p.code, 128, "{}", p.stderr);
}

#[test]
fn show_stops_at_a_remote_that_cannot_be_reached() {
    let t = Twin::new("sha1", Via::Ssh);
    t.configure("remote.dead.url", "ssh://git@example.invalid/nope", false);
    let p = t.pyn_cmd(&t.pyn, &["remote", "show", "origin", "dead"]);
    assert_eq!(p.code, 128, "{}", p.stderr);
    assert!(p.stdout.contains("* remote origin"), "{}", p.stdout);
    assert!(!p.stdout.contains("* remote dead"), "{}", p.stdout);
    assert!(p.stderr.contains("fatal:"), "{}", p.stderr);
}

#[test]
fn prune_removes_stale_refs_and_dry_run_only_reports() {
    for (format, via) in ALL_WAYS {
        let t = Twin::new(format, via);
        t.both(&["prune", "origin"]);
        run(&t.bare, &["branch", "-D", "feature"]);
        let (_, dry) = t.both(&["prune", "-n", "origin"]);
        assert!(
            dry.stdout.contains("[would prune] origin/feature"),
            "{}",
            dry.stdout
        );
        assert!(run(&t.pyn, &["branch", "-r"]).contains("origin/feature"));
        let long = t.both(&["prune", "--dry-run", "origin"]).1;
        assert_eq!(long.stdout, dry.stdout);
        let (_, real) = t.both(&["prune", "origin"]);
        assert!(
            real.stdout.contains("[pruned] origin/feature"),
            "{}",
            real.stdout
        );
        assert!(!run(&t.pyn, &["branch", "-r"]).contains("origin/feature"));
        assert_eq!(
            t.both(&["prune", "origin"]).1.stdout,
            "",
            "nothing stale prints nothing"
        );
    }
}

#[test]
fn prune_follows_the_fetch_refspecs_and_fails_without_a_remote() {
    let t = Twin::new("sha1", Via::Ssh);
    run(&t.bare, &["branch", "-D", "feature"]);
    t.configure(
        "remote.origin.fetch",
        "+refs/heads/main:refs/remotes/origin/main",
        false,
    );
    t.both(&["prune", "origin"]);
    assert!(run(&t.pyn, &["branch", "-r"]).contains("origin/feature"));
    let (_, p) = t.both(&["prune", "nonesuch"]);
    assert_eq!(p.code, 128, "{}", p.stderr);
    assert!(p.stderr.contains("fatal:"), "{}", p.stderr);
}

#[test]
fn update_fetches_remotes_groups_and_prunes() {
    for (format, via) in [("sha1", Via::Ssh), ("sha256", Via::Http)] {
        let t = Twin::new(format, via);
        run(&t.work, &["branch", "more", "main"]);
        run(&t.work, &["push", "-q", t.bare.to_str().unwrap(), "more"]);
        t.both(&["update"]);
        assert!(run(&t.pyn, &["branch", "-r"]).contains("origin/more"));

        run(&t.bare, &["branch", "-D", "more"]);
        t.both(&["update", "origin"]);
        assert!(run(&t.pyn, &["branch", "-r"]).contains("origin/more"));
        t.both(&["update", "-p", "origin"]);
        assert!(!run(&t.pyn, &["branch", "-r"]).contains("origin/more"));

        t.configure("remotes.grp", "origin", false);
        let (_, p) = t.both(&["update", "grp"]);
        assert!(p.stdout.contains("Fetching origin"), "{}", p.stdout);
        t.configure("remote.origin.prune", "true", false);
        t.both(&["update"]);
    }
}

#[test]
fn update_default_set_honours_skips_and_reports_failures() {
    let t = Twin::new("sha1", Via::Ssh);
    t.configure("remote.dead.url", "ssh://git@example.invalid/nope", false);
    t.configure(
        "remote.dead.fetch",
        "+refs/heads/*:refs/remotes/dead/*",
        false,
    );
    let failed = t.both(&["update"]).1;
    assert_eq!(failed.code, 1, "{}", failed.stderr);
    assert!(failed.stdout.contains("Fetching dead"), "{}", failed.stdout);
    t.configure("remote.dead.skipDefaultUpdate", "true", false);
    let ok = t.both(&["update"]).1;
    assert_eq!(ok.code, 0, "{}", ok.stderr);
    assert!(!ok.stdout.contains("dead"), "{}", ok.stdout);
    t.configure("remotes.default", "origin", false);
    t.both(&["update"]);
    let missing = t.both(&["update", "nonesuch"]).1;
    assert_eq!(missing.code, 1, "{}", missing.stderr);
    assert!(
        missing
            .stderr
            .contains("no such remote or remote group: nonesuch"),
        "{}",
        missing.stderr
    );
}

#[test]
fn set_head_sets_deletes_and_follows_the_remote() {
    for (format, via) in [("sha1", Via::Ssh), ("sha256", Via::Http)] {
        let t = Twin::new(format, via);
        let (_, p) = t.both_state(&["set-head", "origin", "-a"]);
        assert!(p.stdout.contains("origin/HEAD set to main"), "{}", p.stdout);
        assert_eq!(
            t.head_of(&t.pyn).as_deref(),
            Some("refs/remotes/origin/main")
        );
        t.both_state(&["set-head", "origin", "feature"]);
        assert_eq!(t.head_of(&t.pyn), t.head_of(&t.git));
        assert_eq!(
            t.head_of(&t.pyn).as_deref(),
            Some("refs/remotes/origin/feature")
        );
        t.both_state(&["set-head", "origin", "-d"]);
        assert_eq!(t.head_of(&t.pyn), None);
        t.both_state(&["set-head", "origin", "-d"]);
        let bad = t.both_state(&["set-head", "origin", "nosuch"]).1;
        assert_eq!(bad.code, 1, "{}", bad.stderr);
        assert_eq!(t.head_of(&t.pyn), None);
        let none = t.both_state(&["set-head", "nonesuch", "-a"]).1;
        assert_eq!(none.code, 128, "{}", none.stderr);
    }
}

#[test]
fn set_branches_replaces_or_adds_fetch_refspecs() {
    let t = Twin::new("sha1", Via::Ssh);
    t.both(&["set-branches", "origin", "a", "b"]);
    t.both(&["set-branches", "--add", "origin", "c", "d*"]);
    t.both(&["set-branches", "origin", "*"]);
    let missing = t.both(&["set-branches", "nonesuch", "x"]).1;
    assert_eq!(missing.code, 2, "{}", missing.stderr);
}

#[test]
fn bad_options_and_missing_arguments_are_usage_errors() {
    let t = Twin::new("sha1", Via::Ssh);
    for args in [
        vec!["show", "-x", "origin"],
        vec!["update", "--bad"],
        vec!["prune"],
        vec!["prune", "-x", "origin"],
        vec!["set-head", "origin"],
        vec!["set-head", "origin", "-a", "x"],
        vec!["set-head", "origin", "-a", "-d"],
        vec!["set-branches"],
    ] {
        let mut full = vec!["remote"];
        full.extend(&args);
        let p = t.pyn_cmd(&t.pyn, &full);
        assert_eq!(p.code, 129, "{args:?}: {}", p.stderr);
        assert_eq!(p.stdout, "", "{args:?}");
    }
}

#[test]
fn show_output_has_gits_layout() {
    let t = Twin::new("sha1", Via::Ssh);
    run(&t.bare, &["branch", "extra", "main"]);
    run(&t.bare, &["branch", "-D", "feature"]);
    t.setup_both(&["switch", "-q", "-c", "main", "origin/main"]);
    let p = t.pyn_cmd(&t.pyn, &["remote", "show", "origin"]);
    let url = &t.url;
    let expected = format!(
        "* remote origin
  Fetch URL: {url}
  Push  URL: {url}
  HEAD branch: main
  Remote branches:
    extra                       new (next fetch will store in remotes/origin)
    main                        tracked
    refs/remotes/origin/feature stale (use 'git remote prune' to remove)
  Local branch configured for 'git pull':
    main merges with remote main
  Local ref configured for 'git push':
    main pushes to main (up to date)
"
    );
    assert_eq!(p.stdout, expected);
}

#[test]
fn update_of_one_remote_is_a_plain_fetch() {
    let t = Twin::new("sha1", Via::Ssh);
    run(&t.work, &["branch", "more", "main"]);
    run(&t.work, &["push", "-q", t.bare.to_str().unwrap(), "more"]);
    let p = t.pyn_cmd(&t.pyn, &["remote", "update"]);
    assert_eq!(p.code, 0, "{}", p.stderr);
    assert_eq!(p.stdout, "", "git prints no heading for a single remote");
    assert!(p.stderr.contains("[new branch]"), "{}", p.stderr);
}
