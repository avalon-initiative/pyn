//! `pyn pull` on a git repository against a real server (ssh through a shim, smart HTTP through `git http-backend`),
//! compared with the real `git pull` on a twin repository: resulting refs, index, work tree, merge state and exit
//! status. Real git is the oracle in these tests only, and only for meaning, never for its wording.

#[path = "../../pyn-gittransport/tests/common/gitsrv.rs"]
mod gitsrv;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use gitsrv::*;
use tempfile::{TempDir, tempdir};

#[derive(Clone, Copy, PartialEq, Debug)]
enum Via {
    Ssh,
    Http,
    HttpAuth,
}

const ALL_WAYS: [(&str, Via); 4] = [
    ("sha1", Via::Ssh),
    ("sha256", Via::Ssh),
    ("sha1", Via::Http),
    ("sha256", Via::Http),
];

const DATE: &str = "2020-01-01T00:00:00Z";

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

struct Twin {
    _tmp: TempDir,
    bare: PathBuf,
    seed: PathBuf,
    git: PathBuf,
    pyn: PathBuf,
    home: PathBuf,
    shim: PathBuf,
    url: String,
    via: Via,
    _server: Option<Server>,
}

impl Twin {
    /// Both twins are real clones of the served `main`.
    fn new(format: &str, via: Via) -> Self {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let bare = sample_bare(&root, format);
        let shim = ssh_shim(&root, false);
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let server = match via {
            Via::Ssh => None,
            Via::Http => Some(Server::start(&root, Opts::default())),
            Via::HttpAuth => Some(Server::start(
                &root,
                Opts {
                    auth: Some(("alice".into(), "s3cret".into())),
                    ..Opts::default()
                },
            )),
        };
        let url = match (&server, via) {
            (None, _) => format!("ssh://git@example.invalid{}", bare.display()),
            (Some(s), Via::HttpAuth) => format!("http://alice:s3cret@{}/served.git", &s.url[7..]),
            (Some(s), _) => format!("{}/served.git", s.url),
        };
        let t = Self {
            seed: root.join("seed"),
            git: root.join("git-side"),
            pyn: root.join("pyn-side"),
            _tmp: tmp,
            bare,
            home,
            shim,
            url,
            via,
            _server: server,
        };
        for dir in [&t.git, &t.pyn] {
            let out = t.git_cmd(&root, &["clone", "-q", &t.url, dir.to_str().unwrap()]);
            assert_eq!(out.code, 0, "clone: {}", out.stderr);
        }
        t
    }

    /// An empty repository on each side, with the served repository as `origin`.
    fn unborn(format: &str, via: Via) -> Self {
        let t = Self::new(format, via);
        for dir in [&t.git, &t.pyn] {
            fs::remove_dir_all(dir).unwrap();
            init(dir, format, false);
            run(dir, &["remote", "add", "origin", &t.url]);
        }
        t
    }

    fn git_cmd(&self, dir: &Path, args: &[&str]) -> Out {
        let out = git(dir)
            .args(args)
            .env("HOME", &self.home)
            .env("GIT_SSH_COMMAND", &self.shim)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_EDITOR", "true")
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
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .output()
            .unwrap();
        out_of(out)
    }

    /// `git pull <args>` and `pyn pull <args>` on the twins, which must end in the same exit status and state.
    fn both(&self, args: &[&str]) -> (Out, Out) {
        let mut full = vec!["pull"];
        full.extend(args);
        let g = self.git_cmd(&self.git, &full);
        let p = self.pyn_cmd(&self.pyn, &full);
        let ctx = format!("{full:?} via {:?}", self.via);
        assert_eq!(
            g.code, p.code,
            "{ctx}\ngit: {}\npyn: {}",
            g.stderr, p.stderr
        );
        assert_eq!(state(&self.git), state(&self.pyn), "{ctx}");
        fsck(&self.pyn);
        (g, p)
    }

    fn both_ok(&self, args: &[&str]) -> Out {
        let p = self.both(args).1;
        assert_eq!(p.code, 0, "{args:?}: {}", p.stderr);
        p
    }

    /// Runs `args` through `git` on both twins.
    fn each(&self, args: &[&str]) {
        for dir in [&self.git, &self.pyn] {
            run(dir, args);
        }
    }

    /// The same commit in both twins: identical content, parents and dates give identical ids.
    fn local(&self, file: &str, content: &str, msg: &str) {
        for dir in [&self.git, &self.pyn] {
            fs::write(dir.join(file), content).unwrap();
            run(dir, &["add", file]);
            let out = git(dir)
                .args(["commit", "-q", "-m", msg])
                .env("GIT_AUTHOR_DATE", DATE)
                .env("GIT_COMMITTER_DATE", DATE)
                .output()
                .unwrap();
            assert!(out.status.success());
        }
    }

    /// A commit on the served `main`.
    fn remote(&self, file: &str, content: &str, msg: &str) -> String {
        run(&self.seed, &["switch", "-q", "main"]);
        let id = commit(&self.seed, file, content.as_bytes(), msg);
        run(
            &self.seed,
            &["push", "-q", self.bare.to_str().unwrap(), "main"],
        );
        id
    }

    fn head(&self, dir: &Path) -> String {
        run(dir, &["rev-parse", "HEAD"]).trim().to_owned()
    }

    fn configure(&self, key: &str, value: &str) {
        self.each(&["config", key, value]);
    }
}

fn out_of(o: std::process::Output) -> Out {
    Out {
        code: o.status.code().unwrap(),
        stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
    }
}

fn is_remote_head(name: &str) -> bool {
    name.starts_with("refs/remotes/") && name.ends_with("/HEAD")
}

fn tree_files(dir: &Path) -> String {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
        let mut entries: Vec<_> = fs::read_dir(dir).unwrap().flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let name = e.file_name().to_string_lossy().into_owned();
            if name == ".git" || name == ".pyn" {
                continue;
            }
            let path = e.path();
            let rel = path.strip_prefix(base).unwrap().display().to_string();
            let meta = fs::symlink_metadata(&path).unwrap();
            if meta.is_dir() {
                walk(base, &path, out);
            } else if meta.file_type().is_symlink() {
                out.push(format!(
                    "{rel} -> {}",
                    fs::read_link(&path).unwrap().display()
                ));
            } else {
                let mut in_base = false;
                let text: String = fs::read_to_string(&path)
                    .unwrap_or_default()
                    .lines()
                    .filter(|l| {
                        in_base =
                            (in_base || l.starts_with("|||||||")) && !l.starts_with("=======");
                        !in_base
                    })
                    .map(|l| {
                        if l.starts_with("<<<<<<<")
                            || l.starts_with(">>>>>>>")
                            || l.starts_with("|||||||")
                        {
                            l[..7].to_owned()
                        } else {
                            l.to_owned()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\\n");
                let exec = meta.permissions().mode() & 0o111 != 0;
                out.push(format!("{rel} exec={exec} {text}"));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.join("\n")
}

fn head_shape(dir: &Path) -> String {
    let branch = git(dir)
        .args(["symbolic-ref", "-q", "HEAD"])
        .output()
        .unwrap();
    let branch = String::from_utf8_lossy(&branch.stdout).trim().to_owned();
    let probe = git(dir)
        .args(["rev-parse", "-q", "--verify", "HEAD"])
        .output()
        .unwrap();
    if !probe.status.success() {
        return format!("{branch} unborn");
    }
    let parents = run(dir, &["rev-list", "--parents", "-n1", "HEAD"]);
    let parents: Vec<&str> = parents.split_whitespace().skip(1).collect();
    if parents.len() > 1 {
        let tree = run(dir, &["rev-parse", "HEAD^{tree}"]);
        let subject = run(dir, &["log", "-1", "--format=%s"]);
        format!(
            "{branch} merge {} {} {}",
            parents.join(","),
            tree.trim(),
            subject.trim()
        )
    } else {
        format!("{branch} {}", run(dir, &["rev-parse", "HEAD"]).trim())
    }
}

fn fetch_head(dir: &Path) -> String {
    fs::read_to_string(dir.join(".git/FETCH_HEAD"))
        .map(|t| {
            t.lines()
                .map(|l| {
                    let mut f = l.splitn(3, '\t');
                    let (id, marker, note) =
                        (f.next().unwrap(), f.next().unwrap(), f.next().unwrap());
                    let name = note.rfind("' of ").map_or("", |i| &note[..=i]);
                    format!("{id} {marker} {name}")
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_else(|_| "<none>".into())
}

fn state(dir: &Path) -> String {
    let refs: String = run(
        dir,
        &[
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            "refs/remotes",
            "refs/tags",
        ],
    )
    .lines()
    .filter(|l| !is_remote_head(l.split(' ').next().unwrap()))
    .map(|l| format!("{l}\n"))
    .collect();
    let merge_head = fs::read_to_string(dir.join(".git/MERGE_HEAD")).unwrap_or_default();
    let conflicts = run(dir, &["diff", "--name-only", "--diff-filter=U"]);
    format!(
        "{refs}--\n{}\n--\n{}\n--\n{}\n--\nMERGE_HEAD={}\n--\n{conflicts}--\n{}",
        head_shape(dir),
        run(dir, &["ls-files", "-s"]),
        tree_files(dir),
        merge_head.trim(),
        fetch_head(dir)
    )
}

#[test]
fn up_to_date_and_fast_forward_match_git() {
    for (format, via) in ALL_WAYS {
        let t = Twin::new(format, via);
        let out = t.both_ok(&[]);
        assert!(out.stdout.contains("Already up to date."), "{}", out.stdout);
        let new = t.remote("n.txt", "new\n", "remote");
        let out = t.both_ok(&[]);
        assert!(out.stdout.contains("Fast-forward"), "{}", out.stdout);
        assert!(out.stdout.contains("Updating "), "{}", out.stdout);
        assert!(out.stderr.starts_with("From "), "{}", out.stderr);
        assert_eq!(t.head(&t.pyn), new);
        assert_eq!(fs::read_to_string(t.pyn.join("n.txt")).unwrap(), "new\n");
    }
}

#[test]
fn a_clean_merge_makes_a_merge_commit_with_git_s_parents_and_tree() {
    for (format, via) in ALL_WAYS {
        let t = Twin::new(format, via);
        t.local("l.txt", "local\n", "local");
        let remote = t.remote("n.txt", "new\n", "remote");
        t.configure("pull.rebase", "false");
        let out = t.both_ok(&["--no-edit"]);
        assert!(out.stdout.contains("Merge made by the 'ort' strategy."));
        let parents = run(&t.pyn, &["rev-list", "--parents", "-n1", "HEAD"]);
        assert!(parents.contains(&remote));
        let subject = run(&t.pyn, &["log", "-1", "--format=%s"]);
        assert!(subject.starts_with("Merge branch 'main' of "), "{subject}");
    }
}

#[test]
fn ff_modes_match_git() {
    let t = Twin::new("sha1", Via::Ssh);
    t.remote("n.txt", "new\n", "remote");
    let before = t.head(&t.pyn);
    let out = t.both(&["--no-ff", "--no-rebase", "--no-edit"]).1;
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_ne!(t.head(&t.pyn), before);
    assert!(out.stdout.contains("Merge made by the 'ort' strategy."));

    let t = Twin::new("sha1", Via::Ssh);
    t.remote("n.txt", "new\n", "remote");
    t.both_ok(&["--ff-only"]);

    let t = Twin::new("sha1", Via::Ssh);
    t.local("l.txt", "local\n", "local");
    t.remote("n.txt", "new\n", "remote");
    let before = t.head(&t.pyn);
    let (g, p) = t.both(&["--ff-only"]);
    assert_eq!((g.code, p.code), (128, 128));
    assert!(p.stderr.contains("Not possible to fast-forward"));
    assert_eq!(t.head(&t.pyn), before);

    let t = Twin::new("sha1", Via::Ssh);
    t.remote("n.txt", "new\n", "remote");
    t.configure("pull.ff", "only");
    t.both_ok(&[]);
    t.local("l.txt", "local\n", "local");
    t.remote("m.txt", "more\n", "remote 2");
    assert_eq!(t.both(&[]).1.code, 128);
    let p = t.both(&["--no-ff", "--no-edit"]).1;
    assert_eq!(p.code, 0, "{}", p.stderr);

    let t = Twin::new("sha1", Via::Ssh);
    t.local("l.txt", "local\n", "local");
    t.remote("n.txt", "new\n", "remote");
    t.configure("pull.ff", "false");
    t.both_ok(&["--no-edit"]);
}

#[test]
fn divergent_history_needs_a_way_to_reconcile() {
    let t = Twin::new("sha1", Via::Ssh);
    t.local("l.txt", "local\n", "local");
    t.remote("n.txt", "new\n", "remote");
    let (g, p) = t.both(&[]);
    assert_eq!((g.code, p.code), (128, 128));
    assert!(
        p.stderr
            .contains("Need to specify how to reconcile divergent branches.")
    );

    t.both_ok(&["--no-rebase", "--no-edit"]);

    let t = Twin::new("sha1", Via::Ssh);
    t.local("l.txt", "local\n", "local");
    t.remote("n.txt", "new\n", "remote");
    t.configure("pull.rebase", "false");
    t.both_ok(&["--no-edit"]);

    let t = Twin::new("sha1", Via::Ssh);
    t.local("l.txt", "local\n", "local");
    t.remote("n.txt", "new\n", "remote");
    t.configure("pull.rebase", "true");
    t.both_ok(&["--no-rebase", "--no-edit"]);
}

#[test]
fn rebase_is_refused_before_anything_happens() {
    type Case<'a> = (&'a [&'a str], Option<(&'a str, &'a str)>);
    let cases: [Case; 4] = [
        (&["--rebase"], None),
        (&["-r"], None),
        (&[], Some(("pull.rebase", "true"))),
        (&[], Some(("branch.main.rebase", "merges"))),
    ];
    for (args, config) in cases {
        let t = Twin::new("sha1", Via::Ssh);
        if let Some((k, v)) = config {
            t.configure(k, v);
        }
        t.remote("n.txt", "new\n", "remote");
        let before = state(&t.pyn);
        let mut full = vec!["pull"];
        full.extend(args);
        let p = t.pyn_cmd(&t.pyn, &full);
        assert_eq!(p.code, 128, "{args:?} {config:?}: {}", p.stderr);
        assert!(p.stderr.contains("rebase is not built"), "{}", p.stderr);
        assert_eq!(state(&t.pyn), before, "nothing was fetched");
    }
}

#[test]
fn unsupported_flags_are_usage_errors() {
    let t = Twin::new("sha1", Via::Ssh);
    let before = state(&t.pyn);
    for flag in [
        "--squash",
        "--autostash",
        "--all",
        "--edit",
        "--signoff",
        "--stat",
        "--allow-unrelated-histories",
        "--recurse-submodules",
    ] {
        let p = t.pyn_cmd(&t.pyn, &["pull", flag]);
        assert_eq!(p.code, 129, "{flag}: {}", p.stderr);
        assert!(p.stderr.contains("pyn pull"), "{flag}: {}", p.stderr);
    }
    for args in [["-s", "ours"], ["-X", "theirs"], ["--cleanup", "strip"]] {
        let mut full = vec!["pull"];
        full.extend(args);
        assert_eq!(t.pyn_cmd(&t.pyn, &full).code, 129, "{args:?}");
    }
    assert_eq!(state(&t.pyn), before);
}

#[test]
fn no_upstream_detached_and_missing_ref_fail_as_git_does() {
    let t = Twin::new("sha1", Via::Ssh);
    t.each(&["switch", "-q", "-c", "solo"]);
    let (g, p) = t.both(&[]);
    assert_eq!((g.code, p.code), (1, 1));
    assert!(
        p.stderr
            .contains("There is no tracking information for the current branch.")
    );

    let t = Twin::new("sha1", Via::Ssh);
    t.each(&["checkout", "-q", "--detach"]);
    let (g, p) = t.both(&[]);
    assert_eq!((g.code, p.code), (1, 1));
    assert!(p.stderr.contains("You are not currently on a branch."));
    t.remote("n.txt", "new\n", "remote");
    t.both_ok(&["origin", "main", "--no-rebase", "--no-edit"]);

    let t = Twin::new("sha1", Via::Ssh);
    t.configure("branch.main.merge", "refs/heads/gone");
    let (g, p) = t.both(&[]);
    assert_eq!((g.code, p.code), (1, 1));
    assert!(p.stderr.contains("no such ref was fetched"), "{}", p.stderr);
    let (g, p) = t.both(&["origin", "gone"]);
    assert_eq!((g.code, p.code), (1, 1));
    assert!(
        p.stderr.contains("couldn't find remote ref"),
        "{}",
        p.stderr
    );

    let t = Twin::new("sha1", Via::Ssh);
    t.each(&["switch", "-q", "-c", "solo"]);
    t.remote("n.txt", "new\n", "remote");
    let p = t.both(&["origin"]).1;
    assert_eq!(p.code, 1);
    assert!(p.stderr.contains("did not specify"), "{}", p.stderr);
    t.both_ok(&["origin", "main", "--no-rebase", "--no-edit"]);
}

#[test]
fn explicit_remote_branch_and_url_arguments() {
    let t = Twin::new("sha1", Via::Ssh);
    t.each(&["switch", "-q", "-c", "solo", "origin/main"]);
    t.remote("n.txt", "new\n", "remote");
    let out = t.both_ok(&["origin", "main"]);
    assert!(out.stdout.contains("Fast-forward"));

    let t = Twin::new("sha1", Via::Http);
    t.local("l.txt", "local\n", "local");
    t.remote("n.txt", "new\n", "remote");
    let url = t.url.clone();
    t.both_ok(&["--no-rebase", "--no-edit", &url, "main"]);
    let subject = run(&t.pyn, &["log", "-1", "--format=%s"]);
    assert!(
        subject.starts_with("Merge branch 'main' of http://"),
        "{subject}"
    );

    let t = Twin::new("sha1", Via::Ssh);
    t.local("l.txt", "local\n", "local");
    t.remote("n.txt", "new\n", "remote");
    let before = state(&t.pyn);
    let p = t.pyn_cmd(
        &t.pyn,
        &["pull", "--no-rebase", "origin", "main", "feature"],
    );
    assert_eq!(p.code, 128, "{}", p.stderr);
    assert!(p.stderr.contains("octopus"), "{}", p.stderr);
    assert_ne!(state(&t.pyn), before, "the fetch ran");
}

#[test]
fn a_conflict_is_left_for_the_user_and_can_be_finished() {
    for (format, via) in [("sha1", Via::Ssh), ("sha256", Via::Http)] {
        let t = Twin::new(format, via);
        t.local("a.txt", "local\n", "local a");
        t.remote("a.txt", "remote\n", "remote a");
        let (g, p) = t.both(&["--no-rebase", "--no-edit"]);
        assert_eq!((g.code, p.code), (1, 1), "{}", p.stderr);
        assert!(p.stdout.contains("Automatic merge failed"), "{}", p.stdout);
        assert!(t.pyn.join(".git/MERGE_HEAD").exists());
        assert!(t.pyn.join(".git/MERGE_MSG").exists());
        assert_eq!(
            run(&t.pyn, &["diff", "--name-only", "--diff-filter=U"]).trim(),
            "a.txt"
        );

        let again = t.both(&["--no-rebase"]);
        assert_eq!(again.1.code, 128, "{}", again.1.stderr);

        for dir in [&t.git, &t.pyn] {
            fs::write(dir.join("a.txt"), "resolved\n").unwrap();
        }
        run(&t.git, &["add", "a.txt"]);
        let add = t.pyn_cmd(&t.pyn, &["add", "a.txt"]);
        assert_eq!(add.code, 0, "{}", add.stderr);
        run(&t.git, &["commit", "-q", "-m", "merged"]);
        let commit = t.pyn_cmd(&t.pyn, &["commit", "-m", "merged"]);
        assert_eq!(commit.code, 0, "{}", commit.stderr);
        assert_eq!(
            run(&t.pyn, &["rev-list", "--parents", "-n1", "HEAD"])
                .split_whitespace()
                .count(),
            3
        );
        assert_eq!(
            run(&t.git, &["rev-parse", "HEAD^{tree}"]),
            run(&t.pyn, &["rev-parse", "HEAD^{tree}"])
        );
        assert!(!t.pyn.join(".git/MERGE_HEAD").exists());
    }
}

#[test]
fn local_changes_that_the_merge_would_overwrite_stop_it() {
    let t = Twin::new("sha1", Via::Ssh);
    t.remote("a.txt", "remote\n", "remote a");
    for dir in [&t.git, &t.pyn] {
        fs::write(dir.join("a.txt"), "dirty\n").unwrap();
    }
    let before = t.head(&t.pyn);
    let (g, p) = t.both(&[]);
    assert_ne!(p.code, 0);
    assert_eq!(g.code, p.code);
    assert!(
        p.stderr.contains("would be overwritten by merge"),
        "{}",
        p.stderr
    );
    assert_eq!(t.head(&t.pyn), before);
    assert_eq!(fs::read_to_string(t.pyn.join("a.txt")).unwrap(), "dirty\n");

    let t = Twin::new("sha1", Via::Ssh);
    t.local("l.txt", "local\n", "local");
    t.remote("a.txt", "remote\n", "remote a");
    for dir in [&t.git, &t.pyn] {
        fs::write(dir.join("a.txt"), "dirty\n").unwrap();
    }
    let (g, p) = t.both(&["--no-rebase", "--no-edit"]);
    assert_eq!(g.code, p.code);
    assert_ne!(p.code, 0);

    let t = Twin::new("sha1", Via::Ssh);
    t.remote("n.txt", "new\n", "remote");
    for dir in [&t.git, &t.pyn] {
        fs::write(dir.join("a.txt"), "dirty\n").unwrap();
    }
    t.both_ok(&[]);
    assert_eq!(fs::read_to_string(t.pyn.join("a.txt")).unwrap(), "dirty\n");
    assert!(t.pyn.join("n.txt").exists());

    let t = Twin::new("sha1", Via::Ssh);
    t.local("l.txt", "local\n", "local");
    t.remote("n.txt", "new\n", "remote");
    for dir in [&t.git, &t.pyn] {
        fs::write(dir.join("a.txt"), "dirty\n").unwrap();
    }
    t.both_ok(&["--no-rebase", "--no-edit"]);
    assert_eq!(fs::read_to_string(t.pyn.join("a.txt")).unwrap(), "dirty\n");
}

#[test]
fn an_unborn_branch_takes_the_remote_tip() {
    for (format, via) in [("sha1", Via::Ssh), ("sha256", Via::Http)] {
        let t = Twin::unborn(format, via);
        let out = t.both_ok(&["origin", "main"]);
        assert!(!out.stdout.contains("Fast-forward"), "{}", out.stdout);
        assert_eq!(t.head(&t.pyn), t.head(&t.seed));
        assert!(t.pyn.join("b.txt").exists());
    }
}

#[test]
fn fetch_flags_are_forwarded() {
    let t = Twin::new("sha1", Via::Ssh);
    run(&t.seed, &["tag", "-a", "v9", "-m", "nine", "feature"]);
    run(&t.seed, &["push", "-q", t.bare.to_str().unwrap(), "v9"]);
    t.both_ok(&["--tags"]);

    let t = Twin::new("sha1", Via::Ssh);
    run(
        &t.seed,
        &[
            "push",
            "-q",
            t.bare.to_str().unwrap(),
            "--delete",
            "feature",
        ],
    );
    t.both_ok(&["--prune"]);
    assert!(
        !run(&t.pyn, &["branch", "-r"]).contains("origin/feature"),
        "pruned"
    );

    let t = Twin::new("sha1", Via::Ssh);
    t.remote("n.txt", "new\n", "remote");
    let before = state(&t.pyn);
    let (g, p) = t.both(&["--dry-run"]);
    assert_eq!((g.code, p.code), (0, 0));
    assert_eq!(state(&t.pyn), before, "a dry run changes nothing");
    assert!(!t.pyn.join("n.txt").exists());
}

#[test]
fn quiet_prints_nothing() {
    let t = Twin::new("sha1", Via::Ssh);
    t.remote("n.txt", "new\n", "remote");
    let p = t.both_ok(&["-q"]);
    assert_eq!((p.stdout.as_str(), p.stderr.as_str()), ("", ""));

    let t = Twin::new("sha1", Via::Ssh);
    t.local("l.txt", "local\n", "local");
    t.remote("n.txt", "new\n", "remote");
    let p = t.both_ok(&["-q", "--no-rebase", "--no-edit"]);
    assert_eq!((p.stdout.as_str(), p.stderr.as_str()), ("", ""));
}

#[test]
fn modes_and_new_files_come_across() {
    let t = Twin::new("sha1", Via::Ssh);
    fs::write(t.seed.join("run.sh"), "#!/bin/sh\n").unwrap();
    fs::set_permissions(t.seed.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    run(&t.seed, &["add", "run.sh"]);
    run(&t.seed, &["commit", "-q", "-m", "script"]);
    run(&t.seed, &["push", "-q", t.bare.to_str().unwrap(), "main"]);
    t.both_ok(&[]);
    let mode = fs::metadata(t.pyn.join("run.sh"))
        .unwrap()
        .permissions()
        .mode();
    assert_ne!(mode & 0o111, 0);
}

#[test]
fn a_server_wall_is_passed_by_a_token_url() {
    let t = Twin::new("sha1", Via::HttpAuth);
    t.remote("n.txt", "new\n", "remote");
    let out = t.both_ok(&[]);
    assert!(out.stdout.contains("Fast-forward"));
    assert!(!out.stderr.contains("s3cret"), "{}", out.stderr);
}

#[test]
fn unmerged_files_refuse_a_pull() {
    let t = Twin::new("sha1", Via::Ssh);
    t.local("a.txt", "local\n", "local a");
    t.remote("a.txt", "remote\n", "remote a");
    t.both(&["--no-rebase", "--no-edit"]);
    fs::remove_file(t.pyn.join(".git/MERGE_HEAD")).unwrap();
    fs::remove_file(t.git.join(".git/MERGE_HEAD")).unwrap();
    let (g, p) = t.both(&["--no-rebase"]);
    assert_eq!((g.code, p.code), (128, 128));
    assert!(p.stderr.contains("unmerged files"), "{}", p.stderr);
}
