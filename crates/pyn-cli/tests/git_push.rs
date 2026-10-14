//! `pyn push` on a git repository against a real server (ssh through a shim, smart HTTP through `git http-backend`),
//! compared with the real `git push` from a twin repository to a twin bare remote: the remote's refs, the local
//! tracking refs and `branch.*` config, the exit status and the summary lines by meaning. Real git is the oracle in
//! these tests only.

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

const SSH: (&str, Via) = ("sha1", Via::Ssh);

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

/// Index 0 is the side real git works on, index 1 the side pyn works on.
struct Twin {
    _tmp: TempDir,
    root: PathBuf,
    seed: PathBuf,
    bare: [PathBuf; 2],
    side: [PathBuf; 2],
    home: PathBuf,
    shim: PathBuf,
    via: Via,
    server: Option<Server>,
}

impl Twin {
    /// Empty bare remotes; `populate` pushes the seed's branches and tags into them first.
    fn new((format, via): (&str, Via), populate: bool) -> Self {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let seed = root.join("seed");
        sample_work(&seed, format);
        let names = ["git", "pyn"];
        let bare = names.map(|n| root.join(format!("served-{n}.git")));
        for b in &bare {
            init(b, format, true);
            run(b, &["symbolic-ref", "HEAD", "refs/heads/main"]);
            if populate {
                run(&seed, &["push", "-q", b.to_str().unwrap(), "--all"]);
                run(&seed, &["push", "-q", b.to_str().unwrap(), "--tags"]);
            }
        }
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
        let side = names.map(|n| root.join(format!("{n}-side")));
        let t = Self {
            _tmp: tmp,
            root,
            seed,
            bare,
            side,
            home,
            shim,
            via,
            server,
        };
        for i in 0..2 {
            init(&t.side[i], format, false);
            run(&t.side[i], &["remote", "add", "origin", &t.url(i, "")]);
            if populate {
                t.git_cmd(&t.side[i], &["fetch", "-q", "origin"]);
            }
        }
        t.sync();
        t
    }

    /// The URL of side `i`'s remote; `suffix` names a second remote.
    fn url(&self, i: usize, suffix: &str) -> String {
        let base = self.bare[i]
            .to_str()
            .unwrap()
            .replace(".git", &format!("{suffix}.git"));
        let name = Path::new(&base)
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        match (&self.server, self.via) {
            (None, _) => format!("ssh://git@example.invalid{base}"),
            (Some(s), Via::HttpAuth) => format!("http://alice:s3cret@{}/{name}", &s.url[7..]),
            (Some(s), _) => format!("{}/{name}", s.url),
        }
    }

    /// Copies the seed's branches and tags into both sides (objects are identical, so ids are too).
    fn sync(&self) {
        for side in &self.side {
            run(
                side,
                &[
                    "fetch",
                    "-q",
                    "-u",
                    self.seed.to_str().unwrap(),
                    "+refs/heads/*:refs/heads/*",
                    "+refs/tags/*:refs/tags/*",
                ],
            );
        }
    }

    fn seed(&self, args: &[&str]) -> String {
        run(&self.seed, args)
    }

    fn seed_commit(&self, branch: &str, file: &str) -> String {
        run(&self.seed, &["switch", "-q", branch]);
        commit(&self.seed, file, file.as_bytes(), file)
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

    fn configure(&self, key: &str, value: &str) {
        for side in &self.side {
            run(side, &["config", key, value]);
        }
    }

    fn configure_bare(&self, key: &str, value: &str) {
        for b in &self.bare {
            run(b, &["config", key, value]);
        }
    }

    fn hook(&self, name: &str, script: &str) {
        for b in &self.bare {
            let path = b.join("hooks").join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    /// A second pair of bare remotes, `o2` on both sides, holding what the first pair holds.
    fn second_remote(&self) {
        for i in 0..2 {
            let b2 = self.bare[i].to_str().unwrap().replace(".git", "2.git");
            let b2 = Path::new(&b2);
            let format = run(&self.bare[i], &["rev-parse", "--show-object-format"]);
            init(b2, format.trim(), true);
            run(b2, &["symbolic-ref", "HEAD", "refs/heads/main"]);
            run(&self.side[i], &["remote", "add", "o2", &self.url(i, "2")]);
        }
    }

    /// Advances `main` on both remotes from a separate clone, so the sides do not have the new commit.
    fn remote_advance(&self, file: &str) -> String {
        let other = self.root.join("other");
        if !other.exists() {
            run(
                &self.root,
                &[
                    "clone",
                    "-q",
                    self.seed.to_str().unwrap(),
                    other.to_str().unwrap(),
                ],
            );
        }
        run(&other, &["switch", "-q", "main"]);
        let id = commit(&other, file, file.as_bytes(), file);
        for b in &self.bare {
            run(
                &other,
                &["push", "-q", b.to_str().unwrap(), "main:refs/heads/main"],
            );
        }
        id
    }

    /// `push <args>` with git and with pyn; requires the same outcome everywhere it can be compared.
    fn cmp(&self, args: &[&str]) -> (Out, Out) {
        self.cmp_cmd("push", args)
    }

    fn cmp_cmd(&self, cmd: &str, args: &[&str]) -> (Out, Out) {
        let mut full = vec![cmd];
        full.extend(args);
        let g = self.git_cmd(&self.side[0], &full);
        let p = self.pyn_cmd(&self.side[1], &full);
        let ctx = format!("{full:?} via {:?}", self.via);
        assert_eq!(
            g.code, p.code,
            "{ctx}\ngit: {}\npyn: {}",
            g.stderr, p.stderr
        );
        assert_eq!(
            summary(&g.stderr),
            summary(&p.stderr),
            "{ctx}\ngit: {}\npyn: {}",
            g.stderr,
            p.stderr
        );
        assert_eq!(
            refs(&self.bare[0]),
            refs(&self.bare[1]),
            "remote refs: {ctx}"
        );
        assert_eq!(
            side_state(&self.side[0]),
            side_state(&self.side[1]),
            "{ctx}"
        );
        for b in &self.bare {
            fsck(b);
        }
        (g, p)
    }

    fn remote_refs(&self) -> String {
        refs(&self.bare[1])
    }

    fn rev(&self, name: &str) -> String {
        run(&self.bare[1], &["rev-parse", name]).trim().to_owned()
    }

    fn pyn_only(&self, args: &[&str]) -> Out {
        self.pyn_cmd(&self.side[1], args)
    }
}

fn refs(dir: &Path) -> String {
    run(dir, &["for-each-ref", "--format=%(refname) %(objectname)"])
}

/// Every ref, the `branch.*` configuration, and each remote-tracking reflog as ids only.
fn side_state(dir: &Path) -> String {
    let all = refs(dir);
    let config = git(dir)
        .args(["config", "--get-regexp", "^branch\\."])
        .output()
        .unwrap();
    let logs: Vec<String> = all
        .lines()
        .filter_map(|l| l.split(' ').next())
        .filter(|n| n.starts_with("refs/remotes/"))
        .map(|n| {
            let log = git(dir)
                .args(["reflog", "show", "--format=%H %gs", n])
                .output()
                .unwrap();
            let log = String::from_utf8_lossy(&log.stdout).into_owned();
            format!("{n}: {}", log.replace("update by push", "P").trim_end())
        })
        .collect();
    let mut config: Vec<&str> = std::str::from_utf8(&config.stdout)
        .unwrap()
        .lines()
        .collect();
    config.sort_unstable();
    format!("{all}--\n{config:?}--\n{logs:#?}")
}

/// The summary lines by meaning: code, summary text, names and reason, and the kind of every other line.
/// `error:`, `hint:` and `warning:` lines are dropped: their wording and count vary by git version.
fn summary(stderr: &str) -> Vec<String> {
    let row = |l: &str| {
        let b = l.as_bytes();
        let flagged = b.len() > 3 && b[0] == b' ' && b"*+-!=tT".contains(&b[1]) && b[2] == b' ';
        let fast_forward = l.starts_with("   ")
            && l.split_whitespace()
                .next()
                .is_some_and(|w| w.contains(".."));
        flagged || fast_forward
    };
    let mut fatal_seen = false;
    stderr
        .lines()
        .take_while(|l| {
            !std::mem::replace(
                &mut fatal_seen,
                l.starts_with("fatal:") && !l.contains("hung up"),
            )
        })
        .filter(|l| {
            l.starts_with("To ")
                || l.starts_with("Pushing to ")
                || l.starts_with("fatal:")
                || l.starts_with("remote:")
                || *l == "Everything up-to-date"
                || row(l)
        })
        .filter(|l| *l != "fatal: the remote end hung up unexpectedly")
        .map(|l| {
            if l.starts_with("To ") || l.starts_with("Pushing to ") {
                return "To".to_owned();
            }
            if l.starts_with("fatal:") {
                return "fatal:".to_owned();
            }
            if l.starts_with("remote:") {
                return "remote:".to_owned();
            }
            if !l.starts_with(' ') {
                return l.to_owned();
            }
            let l = &l[1..];
            let code = &l[..1];
            let rest = l[2..].trim_start();
            let (what, rest) = if rest.starts_with('[') {
                let end = rest.find(']').unwrap() + 1;
                (&rest[..end], &rest[end..])
            } else {
                rest.split_once(' ').unwrap_or((rest, ""))
            };
            let (names, reason) = match rest.rfind(" (") {
                Some(i) if rest.ends_with(')') => (&rest[..i], rest[i + 2..].trim_end_matches(')')),
                _ => (rest, ""),
            };
            let (from, to) = names.split_once(" -> ").unwrap_or((names, ""));
            format!("{code}|{what}|{}|{}|{reason}", from.trim(), to.trim())
        })
        .collect()
}

#[test]
fn first_push_into_an_empty_repository_branches_tags_and_up_to_date() {
    for way in ALL_WAYS {
        let t = Twin::new(way, false);
        let first = t.cmp(&["origin", "main"]).1;
        assert_eq!(first.code, 0, "{}", first.stderr);
        assert!(first.stderr.contains("* [new branch]"), "{}", first.stderr);
        assert!(first.stderr.starts_with("To "), "{}", first.stderr);
        assert!(!first.stderr.contains("s3cret"));
        let tags = t.cmp(&["origin", "--tags"]).1;
        assert!(tags.stderr.contains("* [new tag]"), "{}", tags.stderr);
        t.cmp(&["origin", "--all"]);
        let again = t.cmp(&["origin", "main"]).1;
        assert!(
            again.stderr.contains("Everything up-to-date"),
            "{}",
            again.stderr
        );
        t.cmp(&["-v", "origin", "main", "v1"]);
        let refs = t.remote_refs();
        for name in [
            "refs/heads/main",
            "refs/heads/feature",
            "refs/tags/v1",
            "refs/tags/v2",
        ] {
            assert!(refs.contains(name), "{refs}");
        }
    }
}

#[test]
fn a_token_in_the_url_gets_through_an_http_auth_wall() {
    for format in ["sha1", "sha256"] {
        let t = Twin::new((format, Via::HttpAuth), false);
        let out = t.cmp(&["origin", "main", "v1"]).1;
        assert_eq!(out.code, 0, "{}", out.stderr);
        assert!(!out.stderr.contains("s3cret"), "{}", out.stderr);
        let bad = t.pyn_only(&["push", "http://127.0.0.1:9/served.git", "main"]);
        assert_eq!(bad.code, 128, "{}", bad.stderr);
    }
}

#[test]
fn fast_forward_forced_updates_and_existing_tags() {
    for way in ALL_WAYS {
        let t = Twin::new(way, true);
        t.seed_commit("main", "d.txt");
        t.sync();
        let ff = t.cmp(&["origin", "main"]).1;
        assert!(ff.stderr.contains(".."), "{}", ff.stderr);

        t.seed(&["branch", "-f", "feature", "main~1"]);
        t.sync();
        let rejected = t.cmp(&["origin", "feature"]).1;
        assert_eq!(rejected.code, 1, "{}", rejected.stderr);
        assert!(
            rejected.stderr.contains("(non-fast-forward)"),
            "{}",
            rejected.stderr
        );
        let forced = t.cmp(&["origin", "+feature"]).1;
        assert!(
            forced.stderr.contains("(forced update)"),
            "{}",
            forced.stderr
        );

        t.seed(&["branch", "-f", "feature", "main"]);
        t.seed_commit("feature", "f.txt");
        t.seed(&["switch", "-q", "main"]);
        t.seed(&["branch", "-f", "feature", "main~1"]);
        t.sync();
        let forced = t.cmp(&["--force", "origin", "feature"]).1;
        assert_eq!(forced.code, 0, "{}", forced.stderr);

        t.seed(&["tag", "-f", "v1", "main~2"]);
        t.seed(&["tag", "-a", "-f", "v2", "-m", "again", "main"]);
        t.seed(&["tag", "fresh"]);
        t.sync();
        let tag = t.cmp(&["origin", "v1"]).1;
        assert_eq!(tag.code, 1, "{}", tag.stderr);
        assert!(tag.stderr.contains("(already exists)"), "{}", tag.stderr);
        t.cmp(&["origin", "fresh", "main"]);
        let forced = t.cmp(&["-f", "origin", "v1", "v2"]).1;
        assert!(
            forced.stderr.contains("(forced update)"),
            "{}",
            forced.stderr
        );
    }
}

#[test]
fn a_remote_that_moved_is_fetch_first_then_non_fast_forward_and_leases_decide() {
    for way in [SSH, ("sha256", Via::Http)] {
        let t = Twin::new(way, true);
        t.seed_commit("main", "local.txt");
        t.sync();
        t.remote_advance("remote.txt");
        let first = t.cmp(&["origin", "main"]).1;
        assert_eq!(first.code, 1);
        assert!(first.stderr.contains("(fetch first)"), "{}", first.stderr);
        for i in 0..2 {
            t.git_cmd(&t.side[i], &["fetch", "-q", "origin"]);
        }
        let second = t.cmp(&["origin", "main"]).1;
        assert!(
            second.stderr.contains("(non-fast-forward)"),
            "{}",
            second.stderr
        );

        let lease = t.cmp(&["--force-with-lease", "origin", "main"]).1;
        assert_eq!(lease.code, 0, "{}", lease.stderr);
        assert!(lease.stderr.contains("(forced update)"), "{}", lease.stderr);

        t.seed(&["reset", "-q", "--hard", "HEAD~1"]);
        t.seed_commit("main", "other.txt");
        t.sync();
        t.remote_advance("again.txt");
        let stale = t.cmp(&["--force-with-lease", "origin", "main"]).1;
        assert_eq!(stale.code, 1);
        assert!(stale.stderr.contains("(stale info)"), "{}", stale.stderr);
        let old = t.rev("main~1");
        let named = format!("--force-with-lease=main:{old}");
        assert_eq!(t.cmp(&[&named, "origin", "main"]).1.code, 1);
        let right = t.rev("main");
        let named = format!("--force-with-lease=main:{right}");
        let ok = t.cmp(&[&named, "origin", "main"]).1;
        assert_eq!(ok.code, 0, "{}", ok.stderr);
        let pinned = t.cmp(&["--force-with-lease=feature", "origin", "main"]).1;
        assert_eq!(pinned.code, 0, "{}", pinned.stderr);
        t.cmp(&[
            "--force-with-lease",
            "--no-force-with-lease",
            "origin",
            "main",
        ]);
    }
}

#[test]
fn deleting_refs_by_flag_and_by_empty_source() {
    for way in [SSH, ("sha256", Via::Http)] {
        let t = Twin::new(way, true);
        for i in 0..2 {
            t.git_cmd(&t.side[i], &["fetch", "-q", "origin"]);
        }
        let gone = t.cmp(&["origin", "--delete", "feature"]).1;
        assert!(gone.stderr.contains("- [deleted]"), "{}", gone.stderr);
        assert!(!t.remote_refs().contains("refs/heads/feature"));
        t.cmp(&["origin", ":v1"]);
        t.cmp(&["origin", "-d", "tag", "v2"]);
        let missing = t.cmp(&["origin", ":nope"]).1;
        assert_eq!(missing.code, 1, "{}", missing.stderr);
        let current = t.cmp(&["origin", ":main"]).1;
        assert_eq!(current.code, 1, "{}", current.stderr);
        assert!(
            current.stderr.contains("[remote rejected]"),
            "{}",
            current.stderr
        );
        let none = t.cmp(&["--delete", "origin"]).1;
        assert_eq!(none.code, 128);
    }
}

#[test]
fn follow_tags_atomic_and_dry_run() {
    for way in ALL_WAYS {
        let t = Twin::new(way, true);
        let id = t.seed_commit("main", "d.txt");
        t.seed(&["tag", "-a", "v3", "-m", "three", &id]);
        t.seed(&["tag", "light", &id]);
        t.sync();
        let dry = t.cmp(&["-n", "origin", "main"]).1;
        assert!(dry.stderr.contains(".."), "{}", dry.stderr);
        assert!(!t.remote_refs().contains(&id));
        let follow = t.cmp(&["--follow-tags", "origin", "main"]).1;
        assert!(follow.stderr.contains("v3"), "{}", follow.stderr);
        assert!(!t.remote_refs().contains("refs/tags/light"));

        t.seed_commit("main", "e.txt");
        t.seed(&["branch", "-f", "feature", "main~2"]);
        t.sync();
        let before = t.remote_refs();
        let atomic = t.cmp(&["--atomic", "origin", "main", "feature"]).1;
        assert_eq!(atomic.code, 1, "{}", atomic.stderr);
        assert!(
            atomic.stderr.contains("(atomic push failed)"),
            "{}",
            atomic.stderr
        );
        assert_eq!(t.remote_refs(), before, "nothing changed");
        let some = t.cmp(&["origin", "main", "feature"]).1;
        assert_eq!(some.code, 1);
        assert_ne!(t.remote_refs(), before, "main still went through");
    }
}

#[test]
fn set_upstream_and_what_a_dry_run_leaves_alone() {
    for way in [SSH, ("sha256", Via::Http)] {
        let t = Twin::new(way, false);
        t.cmp(&["-n", "-u", "origin", "main"]);
        let out = t.cmp(&["-u", "origin", "main"]).1;
        assert!(out.stdout.contains("set up to track"), "{}", out.stdout);
        t.cmp(&["-u", "origin", "feature:dev"]);
        t.cmp(&["origin", "HEAD:refs/heads/viahead"]);
        t.cmp(&["-u", "origin", "HEAD"]);
        assert!(t.rev("viahead") == t.rev("main"));
    }
}

#[test]
fn push_default_variants_and_configured_refspecs() {
    for way in [SSH, ("sha256", Via::Http)] {
        let t = Twin::new(way, true);
        for i in 0..2 {
            t.git_cmd(&t.side[i], &["fetch", "-q", "origin"]);
        }
        t.seed_commit("main", "d.txt");
        t.sync();
        let none = t.cmp(&[]).1;
        assert_eq!(none.code, 128, "{}", none.stderr);
        t.configure("branch.main.remote", "origin");
        t.configure("branch.main.merge", "refs/heads/other");
        assert_eq!(t.cmp(&[]).1.code, 128, "simple refuses a different name");
        t.configure("push.default", "upstream");
        let up = t.cmp(&[]).1;
        assert_eq!(up.code, 0, "{}", up.stderr);
        assert!(t.remote_refs().contains("refs/heads/other"));
        t.configure("push.default", "nothing");
        assert_eq!(t.cmp(&[]).1.code, 128);
        t.configure("push.default", "current");
        t.seed_commit("main", "e.txt");
        t.sync();
        assert_eq!(t.cmp(&[]).1.code, 0);
        t.configure("push.default", "simple");
        t.configure("branch.main.merge", "refs/heads/main");
        t.seed_commit("main", "f.txt");
        t.sync();
        assert_eq!(t.cmp(&[]).1.code, 0);
        t.configure("push.default", "matching");
        t.seed_commit("feature", "g.txt");
        t.sync();
        let matching = t.cmp(&[]).1;
        assert_eq!(matching.code, 0, "{}", matching.stderr);
        assert!(matching.stderr.contains("feature"), "{}", matching.stderr);
        t.cmp(&["origin", ":"]);
        t.configure("remote.origin.push", "refs/heads/feature:refs/heads/mapped");
        let mapped = t.cmp(&[]).1;
        assert!(mapped.stderr.contains("mapped"), "{}", mapped.stderr);
        run(&t.seed, &["switch", "-q", "--detach", "HEAD"]);
        for side in &t.side {
            run(side, &["switch", "-q", "--detach", "HEAD"]);
        }
        t.configure("push.default", "simple");
        for side in &t.side {
            run(side, &["config", "--unset-all", "remote.origin.push"]);
        }
        assert_eq!(t.cmp(&[]).1.code, 128, "detached head with simple");
    }
}

#[test]
fn the_push_remote_follows_branch_pushremote_and_remote_pushdefault() {
    let t = Twin::new(SSH, true);
    t.second_remote();
    for i in 0..2 {
        t.git_cmd(&t.side[i], &["fetch", "-q", "origin"]);
    }
    t.seed_commit("main", "d.txt");
    t.sync();
    t.configure("branch.main.remote", "origin");
    t.configure("branch.main.merge", "refs/heads/main");
    t.configure("remote.pushDefault", "o2");
    let triangular = t.cmp(&[]).1;
    assert_eq!(triangular.code, 0, "{}", triangular.stderr);
    let o2 = |i: usize| {
        let p = t.bare[i].to_str().unwrap().replace(".git", "2.git");
        refs(Path::new(&p))
    };
    assert_eq!(o2(0), o2(1));
    assert!(o2(1).contains("refs/heads/main"), "{}", o2(1));
    assert_eq!(
        t.rev("main"),
        run(&t.bare[1], &["rev-parse", "main"]).trim()
    );
    t.seed_commit("main", "e.txt");
    t.sync();
    t.configure("branch.main.pushRemote", "origin");
    let origin = t.cmp(&[]).1;
    assert_eq!(origin.code, 0, "{}", origin.stderr);
    t.cmp(&["o2", "main"]);
    t.cmp(&["-u", "o2", "main"]);
}

#[test]
fn a_server_that_refuses_reports_its_reason_and_atomic_holds() {
    for way in [SSH, ("sha256", Via::Http)] {
        let t = Twin::new(way, true);
        for i in 0..2 {
            t.git_cmd(&t.side[i], &["fetch", "-q", "origin"]);
        }
        t.configure_bare("receive.denyNonFastForwards", "true");
        t.seed(&["branch", "-f", "feature", "main~1"]);
        t.sync();
        let denied = t.cmp(&["origin", "+feature"]).1;
        assert_eq!(denied.code, 1, "{}", denied.stderr);
        assert!(
            denied.stderr.contains("[remote rejected]"),
            "{}",
            denied.stderr
        );

        t.configure_bare("receive.denyNonFastForwards", "false");
        t.hook(
            "pre-receive",
            "while read old new ref; do case \"$ref\" in refs/heads/blocked) echo 'no thanks' >&2; exit 1;; esac; done",
        );
        t.seed_commit("main", "d.txt");
        t.seed(&["branch", "blocked", "main"]);
        t.sync();
        let hook = t.cmp(&["origin", "main", "blocked"]).1;
        assert_eq!(hook.code, 1, "{}", hook.stderr);
        assert!(hook.stderr.contains("no thanks"), "{}", hook.stderr);
        assert!(hook.stderr.contains("[remote rejected]"), "{}", hook.stderr);
        assert!(!t.remote_refs().contains("refs/heads/blocked"));
        let atomic = t.cmp(&["--atomic", "origin", "main", "blocked"]).1;
        assert_eq!(atomic.code, 1, "{}", atomic.stderr);
        assert!(
            !t.remote_refs()
                .contains(&t.seed(&["rev-parse", "main"]).trim().to_owned())
        );
    }
}

#[test]
fn a_history_with_merges_and_annotated_tags_arrives_whole() {
    for way in ALL_WAYS {
        let t = Twin::new(way, false);
        t.seed(&["merge", "-q", "--no-ff", "-m", "merge feature", "feature"]);
        t.seed(&["tag", "-a", "release", "-m", "release notes"]);
        t.sync();
        let out = t.cmp(&["origin", "--all"]).1;
        assert_eq!(out.code, 0, "{}", out.stderr);
        t.cmp(&["origin", "--tags"]);
        let log = run(&t.bare[1], &["log", "--oneline", "main"]);
        assert!(log.contains("merge feature"), "{log}");
        assert!(t.remote_refs().contains("refs/tags/release"));
    }
}

#[test]
fn usage_and_fatal_errors_have_gits_exit_codes() {
    let t = Twin::new(SSH, false);
    for flag in [
        vec!["--mirror"],
        vec!["--prune", "origin"],
        vec!["--signed"],
        vec!["-o", "x", "origin", "main"],
        vec!["--recurse-submodules=check"],
        vec!["--receive-pack=x", "origin", "main"],
        vec!["--exec=x", "origin", "main"],
        vec!["--force-if-includes", "origin", "main"],
        vec!["--repo=x"],
    ] {
        let mut args = vec!["push"];
        args.extend(&flag);
        let out = t.pyn_only(&args);
        assert_eq!(out.code, 129, "{flag:?}: {}", out.stderr);
        assert!(
            out.stderr.contains(flag[0].split('=').next().unwrap()),
            "{}",
            out.stderr
        );
    }
    for flag in [
        "--no-verify",
        "--verify",
        "--thin",
        "--no-thin",
        "-4",
        "-6",
        "--progress",
        "--no-progress",
        "-q",
    ] {
        let out = t.pyn_only(&["push", "-n", flag, "origin", "main"]);
        assert_eq!(out.code, 0, "{flag}: {}", out.stderr);
    }
    let fatal = |args: &[&str]| t.cmp(args).1;
    assert_eq!(fatal(&["--all", "--tags", "origin"]).code, 128);
    assert_eq!(fatal(&["--all", "origin", "main"]).code, 128);
    assert_eq!(fatal(&["--delete", "--tags", "origin", "x"]).code, 128);
    assert_eq!(fatal(&["origin", "-d", "x:y"]).code, 128);
    assert_eq!(fatal(&["origin", "nope"]).code, 1);
    assert_eq!(fatal(&["origin", "main:refs/heads/*"]).code, 128);
    for side in &t.side {
        run(side, &["remote", "remove", "origin"]);
    }
    let nowhere = t.cmp(&[]).1;
    assert_eq!(nowhere.code, 128, "{}", nowhere.stderr);
    assert!(nowhere.stderr.contains("No configured push destination"));
}

#[test]
fn quiet_porcelain_and_tracking_ref_updates() {
    let t = Twin::new(SSH, false);
    let quiet = t.cmp(&["-q", "origin", "main"]).1;
    assert_eq!(quiet.stderr, "");
    t.git_cmd(&t.side[0], &["push", "origin", "feature"]);
    let porcelain = t.pyn_only(&["push", "--porcelain", "origin", "feature"]);
    assert_eq!(porcelain.code, 0, "{}", porcelain.stderr);
    assert!(
        porcelain
            .stdout
            .contains("*\trefs/heads/feature:refs/heads/feature\t[new branch]")
    );
    assert!(
        porcelain.stdout.trim_end().ends_with("Done"),
        "{}",
        porcelain.stdout
    );
    let tracking = run(&t.side[1], &["rev-parse", "refs/remotes/origin/feature"]);
    assert_eq!(tracking.trim(), t.rev("feature"));
    t.cmp(&["origin", ":feature"]);
    assert!(
        git(&t.side[1])
            .args(["rev-parse", "--verify", "-q", "refs/remotes/origin/feature"])
            .output()
            .unwrap()
            .status
            .code()
            == Some(1)
    );
}
