//! Shallow clones and fetches over ssh, smart HTTP, `file://` and local paths: `pyn clone --depth` and `pyn fetch
//! --deepen/--depth/--unshallow` against real git on twin clones, comparing the shallow file, refs, config and what
//! history commands print. Real git is the oracle in these tests only; semantics are compared, never message wording.

#[path = "../../pyn-gittransport/tests/common/gitsrv.rs"]
mod gitsrv;
#[path = "common/oracle.rs"]
mod oracle;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use gitsrv::*;
use tempfile::{TempDir, tempdir};

const DAY: i64 = 86_400;
const BASE: i64 = 1_700_000_000;

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

fn out_of(o: std::process::Output) -> Out {
    Out {
        code: o.status.code().unwrap(),
        stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
    }
}

struct Rig {
    _tmp: TempDir,
    root: PathBuf,
    bare: PathBuf,
    home: PathBuf,
    shim: PathBuf,
    url: String,
    _server: Option<Server>,
}

fn dated_commit(dir: &Path, file: &str, content: &str, msg: &str, at: i64) {
    fs::write(dir.join(file), content).unwrap();
    run(dir, &["add", file]);
    let date = format!("{at} +0000");
    let out = git(dir)
        .args(["commit", "-q", "-m", msg])
        .env("GIT_COMMITTER_DATE", &date)
        .env("GIT_AUTHOR_DATE", &date)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

impl Rig {
    /// Six commits on `main` (one a day, each adding a file and a line of `log.txt`, `v1` on the third), two on `feature`,
    /// and `joined`, which merges two more (`topic`, from the second commit) into `feature`.
    fn new(format: &str, via: Via) -> Self {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let work = root.join("seed");
        init(&work, format, false);
        for i in 1..=6 {
            let mut log = fs::read_to_string(work.join("log.txt")).unwrap_or_default();
            log.push_str(&format!("line {i}\n"));
            fs::write(work.join("log.txt"), log.clone()).unwrap();
            dated_commit(
                &work,
                &format!("f{i}.txt"),
                &format!("{i}\n"),
                &format!("c{i}"),
                BASE + i * DAY,
            );
            if i == 3 {
                run(&work, &["tag", "-a", "-m", "one", "v1"]);
            }
            if i == 4 {
                run(&work, &["branch", "feature"]);
            }
        }
        run(&work, &["switch", "-q", "feature"]);
        dated_commit(&work, "g1.txt", "g1\n", "g1", BASE + 7 * DAY);
        dated_commit(&work, "g2.txt", "g2\n", "g2", BASE + 8 * DAY);
        run(&work, &["switch", "-q", "-c", "topic", "main~4"]);
        dated_commit(&work, "t1.txt", "t1\n", "t1", BASE + 9 * DAY);
        dated_commit(&work, "t2.txt", "t2\n", "t2", BASE + 10 * DAY);
        run(&work, &["switch", "-q", "-c", "joined", "feature"]);
        let date = format!("{} +0000", BASE + 11 * DAY);
        let merged = git(&work)
            .args(["merge", "-q", "--no-ff", "-m", "join", "topic"])
            .env("GIT_COMMITTER_DATE", &date)
            .env("GIT_AUTHOR_DATE", &date)
            .output()
            .unwrap();
        assert!(merged.status.success());
        run(&work, &["switch", "-q", "main"]);
        let bare = root.join("served.git");
        init(&bare, format, true);
        run(&work, &["push", "-q", bare.to_str().unwrap(), "--all"]);
        run(&work, &["push", "-q", bare.to_str().unwrap(), "--tags"]);
        run(&bare, &["symbolic-ref", "HEAD", "refs/heads/main"]);
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        fs::write(home.join(".gitconfig"), "[init]\n\tdefaultBranch = main\n").unwrap();
        let server = via.serve(&root);
        let url = via.url(&server, "served.git", &bare);
        Rig {
            shim: ssh_shim(&root, false),
            _tmp: tmp,
            root,
            bare,
            home,
            url,
            _server: server,
        }
    }

    fn git(&self, cwd: &Path, args: &[&str]) -> Out {
        out_of(
            git(cwd)
                .args(args)
                .env("HOME", &self.home)
                .env("GIT_SSH_COMMAND", &self.shim)
                .env("GIT_SSH_VARIANT", "ssh")
                .env("GIT_TERMINAL_PROMPT", "0")
                .output()
                .unwrap(),
        )
    }

    fn pyn(&self, cwd: &Path, args: &[&str]) -> Out {
        out_of(
            Command::new(env!("CARGO_BIN_EXE_pyn"))
                .current_dir(cwd)
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
                .unwrap(),
        )
    }

    fn side(&self, name: &str) -> PathBuf {
        let d = self.root.join(name);
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// Clones with real git into `git-side/c` and with pyn into `pyn-side/c`; both must agree.
    fn clone_both(&self, flags: &[&str]) -> (PathBuf, PathBuf) {
        self.clone_from(&self.url, flags)
    }

    fn clone_from(&self, url: &str, flags: &[&str]) -> (PathBuf, PathBuf) {
        let mut args = vec!["clone"];
        args.extend_from_slice(flags);
        args.extend(["--", url, "c"]);
        let g = self.git(&self.side("git-side"), &args);
        let p = self.pyn(&self.side("pyn-side"), &args);
        assert_eq!(
            g.code, p.code,
            "{flags:?}\ngit: {}\npyn: {}",
            g.stderr, p.stderr
        );
        let dirs = (self.root.join("git-side/c"), self.root.join("pyn-side/c"));
        if p.code == 0 {
            self.same(&dirs.0, &dirs.1, &format!("clone {flags:?}"));
        }
        dirs
    }

    /// Runs the same git subcommand with git in `g` and pyn in `p`; the results must agree.
    fn both(&self, g: &Path, p: &Path, args: &[&str]) -> Out {
        let go = self.git(g, args);
        let po = self.pyn(p, args);
        let ctx = format!("{args:?}\ngit: {}\npyn: {}", go.stderr, po.stderr);
        assert_eq!(go.code, po.code, "{ctx}");
        if args[0] == "blame" {
            assert_eq!(
                oracle::blame_lines(go.stdout.as_bytes()),
                oracle::blame_lines(po.stdout.as_bytes()),
                "{ctx}"
            );
        } else if args.contains(&"--graph") {
            assert_eq!(
                oracle::graph_commits(&go.stdout),
                oracle::graph_commits(&po.stdout),
                "{ctx}"
            );
        } else {
            assert_eq!(go.stdout, po.stdout, "{ctx}");
        }
        po
    }

    /// Fetches in both and compares the repositories afterwards.
    fn fetch_both(&self, g: &Path, p: &Path, flags: &[&str]) -> Out {
        let mut args = vec!["fetch"];
        args.extend_from_slice(flags);
        let go = self.git(g, &args);
        let po = self.pyn(p, &args);
        let ctx = format!("{args:?}\ngit: {}\npyn: {}", go.stderr, po.stderr);
        assert_eq!(go.code, po.code, "{ctx}");
        if po.code == 0 {
            self.same(g, p, &format!("{args:?}"));
        }
        po
    }

    fn same(&self, g: &Path, p: &Path, ctx: &str) {
        let shallow = |d: &Path| {
            let mut ids: Vec<String> = fs::read_to_string(d.join(".git/shallow"))
                .unwrap_or_default()
                .lines()
                .map(str::to_owned)
                .collect();
            ids.sort();
            ids
        };
        assert_eq!(shallow(g), shallow(p), "{ctx}: shallow file");
        for args in [
            &["for-each-ref", "--format=%(refname) %(objectname)"][..],
            &["rev-list", "--all"],
            &["config", "--local", "--list"],
            &["rev-parse", "--is-shallow-repository"],
        ] {
            let (a, b) = (self.git(g, args), self.git(p, args));
            assert_eq!(a.stdout, b.stdout, "{ctx}: {args:?}");
        }
        fsck(p);
    }
}

fn has_http_client() -> bool {
    let out = Command::new("git").arg("--exec-path").output().unwrap();
    let dir = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    Path::new(&dir).join("git-remote-http").exists()
}

fn ways() -> Vec<(&'static str, Via)> {
    let mut v = vec![
        ("sha1", Via::Ssh),
        ("sha1", Via::FileUrl),
        ("sha256", Via::FileUrl),
    ];
    if has_http_client() {
        v.push(("sha1", Via::Http));
    }
    v
}

#[test]
fn clone_depth_matches_git() {
    for (format, via) in ways() {
        let rig = Rig::new(format, via);
        for flags in [
            &["--depth", "2"][..],
            &["--depth=1"],
            &["--depth", "3", "--no-single-branch"],
            &["--depth", "50"],
            &["--depth", "2", "--bare"],
            &["--depth", "2", "--branch", "feature"],
            &["--depth", "2", "--tags"],
        ] {
            let _ = fs::remove_dir_all(rig.root.join("git-side"));
            let _ = fs::remove_dir_all(rig.root.join("pyn-side"));
            let (_, p) = rig.clone_both(flags);
            assert!(p.exists(), "{flags:?}");
        }
    }
}

#[test]
fn clone_by_date_and_revision_matches_git() {
    for (format, via) in ways() {
        let rig = Rig::new(format, via);
        let since = format!("--shallow-since={}", BASE + 4 * DAY);
        for flags in [
            vec![since.as_str()],
            vec!["--shallow-exclude=v1"],
            vec!["--shallow-exclude=refs/tags/v1", "--no-single-branch"],
        ] {
            let _ = fs::remove_dir_all(rig.root.join("git-side"));
            let _ = fs::remove_dir_all(rig.root.join("pyn-side"));
            rig.clone_both(&flags);
        }
    }
}

#[test]
fn fetch_moves_the_boundary_like_git() {
    for (format, via) in ways() {
        let rig = Rig::new(format, via);
        let (g, p) = rig.clone_both(&["--depth", "1"]);
        for flags in [
            &["--deepen", "1"][..],
            &["--deepen", "2"],
            &["--depth", "1"],
            &["--depth", "4"],
            &["--depth", "4"],
            &["--unshallow"],
            &["--unshallow"],
            &["--deepen", "3"],
        ] {
            rig.fetch_both(&g, &p, flags);
        }
        assert!(!p.join(".git/shallow").exists());
    }
}

#[test]
fn deepening_across_branches_and_merges_matches_git() {
    for (format, via) in ways() {
        let rig = Rig::new(format, via);
        let (g, p) = rig.clone_both(&["--depth", "1", "--no-single-branch"]);
        for flags in [
            &["--deepen", "1"][..],
            &["--deepen", "2"],
            &["--depth", "2"],
            &["--deepen", "1"],
            &["--depth", "1"],
            &["--deepen", "5"],
            &["--unshallow"],
        ] {
            rig.fetch_both(&g, &p, flags);
        }
        let (g, p) = rig.clone_from(&rig.url, &["--depth", "2", "--branch", "joined"]);
        rig.fetch_both(&g, &p, &["--deepen", "1"]);
        rig.fetch_both(&g, &p, &["--depth", "3", "origin", "feature:feature"]);
        rig.fetch_both(&g, &p, &["--deepen", "4"]);
    }
}

#[test]
fn fetch_by_date_and_revision_matches_git() {
    for (format, via) in ways() {
        let rig = Rig::new(format, via);
        let (g, p) = rig.clone_both(&["--depth", "1"]);
        let since = format!("--shallow-since={}", BASE + 2 * DAY);
        rig.fetch_both(&g, &p, &[&since]);
        rig.fetch_both(&g, &p, &["--shallow-exclude=v1"]);
    }
}

#[test]
fn a_complete_clone_can_be_made_shallow() {
    let rig = Rig::new("sha1", Via::FileUrl);
    let (g, p) = rig.clone_both(&[]);
    rig.fetch_both(&g, &p, &["--deepen", "2"]);
    rig.fetch_both(&g, &p, &["--unshallow"]);
    rig.fetch_both(&g, &p, &["--depth", "2"]);
}

#[test]
fn flag_errors_exit_as_git_does() {
    let rig = Rig::new("sha1", Via::FileUrl);
    for flags in [&["--depth", "0"][..], &["--depth", "x"], &["--depth", "-1"]] {
        rig.clone_both(flags);
    }
    let (g, p) = rig.clone_both(&["--depth", "2"]);
    for flags in [
        &["--depth", "1", "--unshallow"][..],
        &["--deepen", "1", "--depth", "1"],
        &["--deepen", "1", "--shallow-since=2020-01-01"],
        &["--depth", "0"],
    ] {
        rig.fetch_both(&g, &p, flags);
    }
    let (g, p) = rig.clone_both(&[]);
    rig.fetch_both(&g, &p, &["--unshallow"]);
}

#[test]
fn history_commands_stop_at_the_boundary_like_git() {
    for (format, via) in ways() {
        let rig = Rig::new(format, via);
        let (g, p) = rig.clone_both(&["--depth", "3", "--no-single-branch"]);
        for args in [
            &["log", "--oneline"][..],
            &["log", "--oneline", "--all"],
            &["log", "--format=%H %P", "feature"],
            &["log", "--graph", "--oneline", "--all"],
            &["log", "--oneline", "--", "log.txt"],
            &["log", "--stat", "--format=%h"],
            &["blame", "log.txt"],
            &["blame", "-s", "f6.txt"],
            &["blame", "-s", "f5.txt"],
            &["blame", "-s", "f4.txt"],
            &["describe", "--always"],
            &["shortlog", "-s", "HEAD"],
            &["rev-parse", "HEAD~1"],
            &["rev-parse", "HEAD~2"],
            &["rev-parse", "HEAD~3"],
            &["show", "--stat", "--format=%H", "HEAD~2"],
            &["diff", "HEAD~2", "HEAD"],
        ] {
            rig.both(&g, &p, args);
        }
    }
}

#[test]
fn merging_across_the_boundary_refuses_like_git() {
    let rig = Rig::new("sha1", Via::FileUrl);
    let (g, p) = rig.clone_both(&["--depth", "2", "--no-single-branch"]);
    let args = ["merge", "origin/feature"];
    let go = rig.git(&g, &args);
    let po = rig.pyn(&p, &args);
    assert_eq!(go.code, po.code, "git: {}\npyn: {}", go.stderr, po.stderr);
    assert_ne!(po.code, 0, "the merge base is beyond the boundary");
    rig.same(&g, &p, "after merge");
}

#[test]
fn a_shallow_clone_pushes_new_commits() {
    for (format, via) in ways() {
        let rig = Rig::new(format, via);
        let (_, p) = rig.clone_both(&["--depth", "1"]);
        fs::write(p.join("new.txt"), "n\n").unwrap();
        rig.git(&p, &["add", "new.txt"]);
        let c = rig.git(&p, &["commit", "-q", "-m", "from shallow"]);
        assert_eq!(c.code, 0, "{}", c.stderr);
        let push = rig.pyn(&p, &["push", "origin", "main"]);
        assert_eq!(push.code, 0, "{}", push.stderr);
        let tip = run(&p, &["rev-parse", "HEAD"]);
        assert_eq!(run(&rig.bare, &["rev-parse", "refs/heads/main"]), tip);
        fsck(&rig.bare);
        let count = run(&rig.bare, &["rev-list", "--count", "main"]);
        assert_eq!(count.trim(), "7");
    }
}

#[test]
fn local_paths_ignore_shallow_flags_with_a_warning() {
    let rig = Rig::new("sha1", Via::Local);
    for flags in [
        &["--depth", "1"][..],
        &["--shallow-since=2020-01-01"],
        &["--shallow-exclude=v1"],
    ] {
        let _ = fs::remove_dir_all(rig.root.join("git-side"));
        let _ = fs::remove_dir_all(rig.root.join("pyn-side"));
        let mut args = vec!["clone"];
        args.extend_from_slice(flags);
        args.extend(["--", rig.url.as_str(), "c"]);
        let g = rig.git(&rig.side("git-side"), &args);
        let p = rig.pyn(&rig.side("pyn-side"), &args);
        assert_eq!(g.code, p.code);
        assert!(
            p.stderr.contains("is ignored in local clones"),
            "{}",
            p.stderr
        );
        assert!(g.stderr.contains("is ignored in local clones"));
        rig.same(
            &rig.root.join("git-side/c"),
            &rig.root.join("pyn-side/c"),
            &format!("{flags:?}"),
        );
        assert!(!rig.root.join("pyn-side/c/.git/shallow").exists());
    }
}

#[test]
fn a_shallow_repository_can_be_cloned() {
    for url_of in [
        |p: &Path| format!("file://{}", p.display()),
        |p: &Path| format!("ssh://git@example.invalid{}", p.display()),
    ] {
        let rig = Rig::new("sha1", Via::FileUrl);
        let made = rig.git(
            &rig.side("mid"),
            &[
                "clone",
                "-q",
                "--depth",
                "2",
                "--no-single-branch",
                "--",
                &rig.url,
                "m",
            ],
        );
        assert_eq!(made.code, 0, "{}", made.stderr);
        let mid = rig.root.join("mid/m");
        let url = url_of(&mid);
        for flags in [&[][..], &["--depth", "1"]] {
            let _ = fs::remove_dir_all(rig.root.join("git-side"));
            let _ = fs::remove_dir_all(rig.root.join("pyn-side"));
            rig.clone_from(&url, flags);
        }
    }
}

#[test]
fn a_plain_fetch_into_a_shallow_clone_matches_git() {
    for (format, via) in ways() {
        let rig = Rig::new(format, via);
        let (g, p) = rig.clone_both(&["--depth", "2", "--no-single-branch"]);
        let work = rig.root.join("seed");
        for i in 0..2 {
            dated_commit(
                &work,
                &format!("n{i}.txt"),
                "n\n",
                "next",
                BASE + (20 + i) * DAY,
            );
            run(&work, &["push", "-q", rig.bare.to_str().unwrap(), "main"]);
            rig.fetch_both(&g, &p, &[]);
        }
        rig.both(&g, &p, &["log", "--oneline", "origin/main"]);
        let go = rig.git(&g, &["merge", "--ff-only", "origin/main"]);
        let po = rig.pyn(&p, &["merge", "origin/main"]);
        assert_eq!(go.code, po.code, "{}{}", go.stderr, po.stderr);
        rig.same(&g, &p, "after merge");
        rig.both(&g, &p, &["log", "--oneline"]);
    }
}
