//! `pyn fetch` on a git repository against a real server (ssh through a shim, smart HTTP through `git http-backend`),
//! compared with the real `git fetch` on a twin repository: refs, `FETCH_HEAD`, reflogs, the summary lines and the exit
//! status. Real git is the oracle in these tests only.

#[path = "../../pyn-gittransport/tests/common/gitsrv.rs"]
mod gitsrv;

use std::collections::BTreeMap;
use std::fs;
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

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

struct Twin {
    _tmp: TempDir,
    root: PathBuf,
    bare: PathBuf,
    work: PathBuf,
    git: PathBuf,
    pyn: PathBuf,
    home: PathBuf,
    shim: PathBuf,
    url: String,
    via: Via,
    server: Option<Server>,
}

impl Twin {
    fn new(format: &str, via: Via) -> Self {
        Self::build(format, via, true)
    }

    fn build(format: &str, via: Via, add_remote: bool) -> Self {
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
            work: root.join("seed"),
            git: root.join("git-side"),
            pyn: root.join("pyn-side"),
            _tmp: tmp,
            root,
            bare,
            home,
            shim,
            url,
            via,
            server,
        };
        for dir in [&t.git, &t.pyn] {
            init(dir, format, false);
            if add_remote {
                run(dir, &["remote", "add", "origin", &t.url]);
            }
        }
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

    /// Runs `git <args>` and `pyn <args>` on the twins and requires the same outcome.
    fn both(&self, args: &[&str]) -> (Out, Out) {
        let g = self.git_cmd(&self.git, args);
        let p = self.pyn_cmd(&self.pyn, args);
        let ctx = format!("{args:?} via {:?}", self.via);
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
        assert_eq!(state(&self.git), state(&self.pyn), "{ctx}");
        fsck(&self.pyn);
        (g, p)
    }

    /// `fetch <args>` on both twins; returns pyn's result.
    fn step(&self, args: &[&str]) -> Out {
        let mut full = vec!["fetch"];
        full.extend(args);
        self.both(&full).1
    }

    fn work(&self, args: &[&str]) {
        run(&self.work, args);
    }

    fn push(&self, args: &[&str]) {
        let mut full = vec!["push", "-q", self.bare.to_str().unwrap()];
        full.extend(args);
        run(&self.work, &full);
    }

    fn configure(&self, key: &str, value: &str) {
        for dir in [&self.git, &self.pyn] {
            run(dir, &["config", key, value]);
        }
    }

    /// A second bare repository holding `main`, as remote `o2` on both twins.
    fn second_remote(&self) -> String {
        let bare2 = self.root.join("served2.git");
        init(&bare2, &git_format(&self.bare), true);
        run(
            &self.work,
            &["push", "-q", bare2.to_str().unwrap(), "main:refs/heads/two"],
        );
        let url = match &self.server {
            None => format!("ssh://git@example.invalid{}", bare2.display()),
            Some(s) => format!("{}/served2.git", s.url),
        };
        for dir in [&self.git, &self.pyn] {
            run(dir, &["remote", "add", "o2", &url]);
        }
        url
    }
}

fn git_format(repo: &Path) -> String {
    run(repo, &["rev-parse", "--show-object-format"])
        .trim()
        .to_owned()
}

fn out_of(o: std::process::Output) -> Out {
    Out {
        code: o.status.code().unwrap(),
        stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
    }
}

/// The summary lines by meaning: code, summary text, names and reason, and the kind of every other line.
fn summary(stderr: &str) -> Vec<String> {
    let mut lines: Vec<&str> = stderr
        .lines()
        .filter(|l| !l.starts_with("POST ") && !l.starts_with("GET "))
        .filter(|l| *l != "fatal: the remote end hung up unexpectedly")
        .filter(|l| !l.starts_with(" '") && !l.starts_with("Fetching "))
        .collect();
    let mut seen = std::collections::HashSet::new();
    lines.retain(|l| seen.insert(*l));
    lines
        .into_iter()
        .map(|l| {
            if l.starts_with("From ") {
                return "From".to_owned();
            }
            if let Some(prefix) = ["fatal:", "error:", "warning:", "hint:"]
                .iter()
                .find(|p| l.starts_with(**p))
            {
                return prefix.to_string();
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
            let (names, reason) = match rest.rfind("  (") {
                Some(i) => (&rest[..i], rest[i + 3..].trim_end_matches(')')),
                None => (rest, ""),
            };
            let (from, to) = names.split_once(" -> ").unwrap_or((names, ""));
            format!("{code}|{what}|{}|{}|{reason}", from.trim(), to.trim())
        })
        .collect()
}

fn state(dir: &Path) -> String {
    let refs = run(dir, &["for-each-ref", "--format=%(refname) %(objectname)"]);
    let fetch_head = fs::read_to_string(dir.join(".git/FETCH_HEAD"))
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
        .unwrap_or_else(|_| "<none>".into());
    format!("{refs}--\n{fetch_head}\n--\n{:?}", reflogs(dir))
}

fn reflogs(dir: &Path) -> BTreeMap<String, Vec<String>> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<String>>) {
        let Ok(rd) = fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let path = e.path();
            if path.is_dir() {
                walk(base, &path, out);
                continue;
            }
            let name = path
                .strip_prefix(base)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let lines = fs::read_to_string(&path)
                .unwrap()
                .lines()
                .map(|l| {
                    let (head, msg) = l.split_once('\t').unwrap_or((l, ""));
                    let mut ids = head.split(' ');
                    let (old, new) = (ids.next().unwrap(), ids.next().unwrap());
                    let op = msg.split_once(": ").map_or(msg, |(_, op)| op);
                    format!("{old} {new} {op}")
                })
                .collect();
            out.insert(name, lines);
        }
    }
    let base = dir.join(".git/logs");
    let mut out = BTreeMap::new();
    walk(&base, &base, &mut out);
    out
}

fn advance(t: &Twin, file: &str, branch: &str) -> String {
    t.work(&["switch", "-q", branch]);
    commit(&t.work, file, file.as_bytes(), file)
}

#[test]
fn first_fetch_and_incremental_updates_match_git() {
    for (format, via) in ALL_WAYS {
        let t = Twin::new(format, via);
        t.work(&["tag", "-a", "old", "-m", "an old one", "main~1"]);
        t.push(&["old"]);
        let first = t.step(&["origin"]);
        assert_eq!(first.code, 0, "{}", first.stderr);
        assert!(first.stderr.contains("[new branch]") && first.stderr.contains("[new tag]"));
        assert_eq!(t.step(&["origin"]).stderr, "", "up to date prints nothing");
        let verbose = t.step(&["-v", "origin"]);
        assert!(
            verbose.stderr.contains("[up to date]"),
            "{}",
            verbose.stderr
        );

        advance(&t, "d.txt", "main");
        t.work(&["tag", "-a", "v3", "-m", "three"]);
        t.work(&["tag", "light"]);
        t.push(&["main", "v3", "light"]);
        let ff = t.step(&[]);
        assert!(
            ff.stderr.contains("..") && ff.stderr.contains("[new tag]"),
            "{}",
            ff.stderr
        );
        advance(&t, "e.txt", "feature");
        t.work(&["branch", "extra", "main"]);
        t.push(&["feature", "extra"]);
        t.step(&[]);
    }
}

#[test]
fn forced_updates_and_rejections() {
    for (format, via) in [("sha1", Via::Ssh), ("sha256", Via::Http)] {
        let t = Twin::new(format, via);
        t.step(&[]);
        t.work(&["switch", "-q", "main"]);
        t.work(&["reset", "-q", "--hard", "HEAD~1"]);
        commit(&t.work, "x.txt", b"x", "diverged");
        t.push(&["--force", "main"]);
        let forced = t.step(&[]);
        assert!(
            forced.stderr.contains("(forced update)"),
            "{}",
            forced.stderr
        );

        t.work(&["reset", "-q", "--hard", "HEAD~1"]);
        commit(&t.work, "y.txt", b"y", "diverged again");
        t.push(&["--force", "main"]);
        let spec = "refs/heads/main:refs/remotes/origin/main";
        let rejected = t.step(&["origin", spec]);
        assert_eq!(rejected.code, 1, "{}", rejected.stderr);
        assert!(
            rejected.stderr.contains("(non-fast-forward)"),
            "{}",
            rejected.stderr
        );
        let quiet = t.step(&["-q", "origin", spec]);
        assert_eq!((quiet.code, quiet.stderr.as_str()), (1, ""));
        let plus = t.step(&["origin", &format!("+{spec}")]);
        assert_eq!(plus.code, 0, "{}", plus.stderr);
        let up = t.step(&["--force", "origin", spec]);
        assert_eq!(up.code, 0);
    }
}

#[test]
fn tags_follow_clobber_and_force_like_git() {
    for (format, via) in [("sha1", Via::Ssh), ("sha256", Via::Http)] {
        let t = Twin::new(format, via);
        t.step(&["--no-tags"]);
        assert!(!run(&t.pyn, &["tag", "-l"]).contains("v1"));
        t.step(&[]);
        t.step(&["--tags"]);

        let main = advance(&t, "d.txt", "main");
        t.work(&["tag", "-f", "v1", &main]);
        t.work(&["tag", "-a", "-f", "v2", "-m", "again"]);
        t.work(&["tag", "fresh"]);
        t.push(&["--force", "v1", "v2", "fresh", "main"]);
        t.step(&["-n"]);
        t.step(&[]);
        let clobber = t.step(&["--tags"]);
        assert_eq!(clobber.code, 1, "{}", clobber.stderr);
        assert!(clobber.stderr.contains("would clobber existing tag"));
        let forced = t.step(&["--tags", "--force"]);
        assert!(forced.stderr.contains("[tag update]"), "{}", forced.stderr);
        t.configure("remote.origin.tagOpt", "--no-tags");
        t.work(&["tag", "later"]);
        t.push(&["later"]);
        t.step(&[]);
        t.step(&["--tags"]);
    }
}

#[test]
fn prune_and_prune_tags_remove_what_the_remote_lost() {
    for (format, via) in [("sha1", Via::Ssh), ("sha256", Via::Http)] {
        let t = Twin::new(format, via);
        t.step(&["--tags"]);
        run(&t.bare, &["branch", "-D", "feature"]);
        run(&t.bare, &["tag", "-d", "v1"]);
        let dry = t.step(&["--prune", "--dry-run"]);
        assert!(dry.stderr.contains("[deleted]"), "{}", dry.stderr);
        assert!(run(&t.pyn, &["branch", "-r"]).contains("origin/feature"));
        t.step(&["--prune", "origin"]);
        assert!(!run(&t.pyn, &["branch", "-r"]).contains("origin/feature"));
        assert!(
            run(&t.pyn, &["tag", "-l"]).contains("v1"),
            "tags stay without --prune-tags"
        );
        let tags = t.step(&["--prune", "--prune-tags"]);
        assert!(tags.stderr.contains("v1"), "{}", tags.stderr);
        assert!(!run(&t.pyn, &["tag", "-l"]).contains("v1"));

        t.work(&["branch", "gone", "main"]);
        t.push(&["gone"]);
        t.step(&[]);
        run(&t.bare, &["branch", "-D", "gone"]);
        t.configure("fetch.prune", "true");
        let configured = t.step(&[]);
        assert!(
            configured.stderr.contains("[deleted]"),
            "{}",
            configured.stderr
        );
        t.work(&["branch", "gone2", "main"]);
        t.push(&["gone2"]);
        t.step(&[]);
        run(&t.bare, &["branch", "-D", "gone2"]);
        let off = t.step(&["--no-prune"]);
        assert_eq!(off.stderr, "");
        t.configure("remote.origin.prune", "true");
        t.step(&[]);
    }
}

#[test]
fn all_fetches_every_remote_and_reports_failures() {
    for (format, via) in [("sha1", Via::Ssh), ("sha256", Via::Http)] {
        let t = Twin::new(format, via);
        t.second_remote();
        let all = t.step(&["--all"]);
        assert!(
            all.stdout.contains("Fetching origin\nFetching o2"),
            "{}",
            all.stdout
        );
        let jobs = t.pyn_cmd(&t.pyn, &["fetch", "--all", "-j2", "-v"]);
        assert_eq!(jobs.code, 0, "{}", jobs.stderr);
        assert_eq!(t.git_cmd(&t.git, &["fetch", "--all", "-j2", "-v"]).code, 0);
        t.configure("remote.o2.skipFetchAll", "true");
        let skipped = t.step(&["--all"]);
        assert!(!skipped.stdout.contains("o2"), "{}", skipped.stdout);
        for dir in [&t.git, &t.pyn] {
            run(
                dir,
                &["config", "remote.o2.url", "ssh://git@example.invalid/nope"],
            );
            run(dir, &["config", "--unset", "remote.o2.skipFetchAll"]);
        }
        let failed = t.pyn_cmd(&t.pyn, &["fetch", "--all"]);
        assert_eq!(failed.code, 1, "{}", failed.stderr);
        assert!(
            failed.stderr.contains("error: Could not fetch o2"),
            "{}",
            failed.stderr
        );
        let g = t.git_cmd(&t.git, &["fetch", "--all"]);
        assert_eq!(g.code, 1);
        let bad = t.pyn_cmd(&t.pyn, &["fetch", "--all", "origin"]);
        assert_eq!(bad.code, 128);
        let bad = t.pyn_cmd(&t.pyn, &["fetch", "--all", "--", "main"]);
        assert_eq!(bad.code, 128);
    }
}

#[test]
fn groups_and_the_default_remote_follow_configuration() {
    let t = Twin::new("sha1", Via::Ssh);
    t.second_remote();
    t.configure("remotes.both", "origin o2");
    t.step(&["both"]);
    t.configure("branch.main.remote", "o2");
    t.work(&["branch", "plain", "main"]);
    t.push(&["plain"]);
    t.step(&[]);
    t.step(&["--multiple", "origin", "o2"]);
}

#[test]
fn refspecs_on_the_command_line_and_fetch_head() {
    for (format, via) in [("sha1", Via::Ssh), ("sha256", Via::Http)] {
        let t = Twin::new(format, via);
        t.step(&["origin", "main"]);
        t.step(&["origin", "main", "v1"]);
        t.step(&["origin", "main:refs/remotes/origin/xx", "feature:newbranch"]);
        t.step(&["origin", "+feature:refs/remotes/origin/xx"]);
        t.step(&["origin", "tag", "v2"]);
        t.step(&["origin", "refs/heads/*:refs/remotes/other/*"]);
        t.step(&["origin", "HEAD:refs/remotes/origin/headcopy"]);
        t.step(&["origin", "refs/tags/v*:refs/tags/v*"]);
        t.step(&["-v", "origin", "main", "feature"]);
        let missing = t.step(&["origin", "nonexistent"]);
        assert_eq!(missing.code, 128);
        assert!(
            missing
                .stderr
                .contains("couldn't find remote ref nonexistent")
        );
        let clash = t.step(&["origin", "main:refs/remotes/c", "feature:refs/remotes/c"]);
        assert_eq!(clash.code, 128);
        let bad = t.step(&["origin", "a:b:c"]);
        assert_eq!(bad.code, 128);
    }
}

#[test]
fn merge_candidates_and_opportunistic_updates() {
    let t = Twin::new("sha1", Via::Ssh);
    t.configure("branch.main.remote", "origin");
    t.configure("branch.main.merge", "refs/heads/feature");
    t.step(&[]);
    t.configure("branch.main.merge", "refs/heads/nothing-here");
    t.step(&[]);
    t.configure("branch.main.merge", "refs/heads/main");
    t.step(&["origin"]);
    t.work(&["branch", "other", "main"]);
    t.push(&["other"]);
    t.step(&["origin", "other"]);
    let url = t.url.clone();
    t.step(&[&url]);
    t.step(&[&url, "main"]);
    t.step(&["--append", &url, "feature"]);
}

#[test]
fn dry_run_changes_no_ref_and_writes_no_fetch_head() {
    let t = Twin::new("sha1", Via::Ssh);
    let dry = t.step(&["--dry-run"]);
    assert!(dry.stderr.contains("[new branch]"), "{}", dry.stderr);
    assert!(run(&t.pyn, &["for-each-ref"]).is_empty());
    assert!(!t.pyn.join(".git/FETCH_HEAD").exists());
    t.step(&[]);
    advance(&t, "dry.txt", "main");
    t.push(&["main"]);
    let dry = t.step(&["--dry-run", "-v"]);
    assert!(dry.stderr.contains(".."), "{}", dry.stderr);
    t.step(&[]);
}

#[test]
fn remote_head_unborn_or_detached() {
    let t = Twin::new("sha1", Via::Ssh);
    let url = t.url.clone();
    run(&t.bare, &["symbolic-ref", "HEAD", "refs/heads/unborn"]);
    let unborn = t.step(&[&url]);
    assert_eq!(unborn.code, 128);
    assert!(unborn.stderr.contains("couldn't find remote ref HEAD"));
    t.step(&["origin"]);
    let main = run(&t.bare, &["rev-parse", "main"]);
    run(&t.bare, &["update-ref", "--no-deref", "HEAD", main.trim()]);
    t.step(&[&url]);
    t.step(&["origin", "HEAD:refs/remotes/origin/HEAD2"]);
}

#[test]
fn the_checked_out_branch_is_protected() {
    let t = Twin::new("sha1", Via::Ssh);
    t.step(&[]);
    for dir in [&t.git, &t.pyn] {
        run(dir, &["switch", "-q", "-c", "main", "origin/main"]);
    }
    let refused = t.step(&["origin", "feature:main"]);
    assert_eq!(refused.code, 128);
    assert!(
        refused.stderr.contains("refusing to fetch into branch"),
        "{}",
        refused.stderr
    );
    t.step(&["origin", "feature:other"]);
    t.step(&["--update-head-ok", "origin", "+feature:main"]);
}

#[test]
fn the_server_decides_what_fails_and_secrets_stay_out_of_output() {
    let t = Twin::new("sha1", Via::HttpAuth);
    let first = t.step(&["origin"]);
    assert_eq!(first.code, 0, "{}", first.stderr);
    for secret_holder in [&first.stderr, &first.stdout] {
        assert!(!secret_holder.contains("s3cret"));
    }
    assert!(
        !fs::read_to_string(t.pyn.join(".git/FETCH_HEAD"))
            .unwrap()
            .contains("s3cret")
    );
    let log = t.server.as_ref().unwrap().log();
    assert!(log.iter().any(|s| s.status == 401));
    assert!(
        log.iter()
            .any(|s| s.status == 200 && s.headers.contains_key("authorization"))
    );
    let wrong = t.url.replace("s3cret", "wr0ng");
    for dir in [&t.git, &t.pyn] {
        run(dir, &["config", "remote.origin.url", &wrong]);
    }
    let denied = t.pyn_cmd(&t.pyn, &["fetch", "origin"]);
    assert_eq!(denied.code, 128, "{}", denied.stderr);
    assert!(!denied.stderr.contains("wr0ng"), "{}", denied.stderr);
    assert!(denied.stderr.starts_with("fatal: "));
}

#[test]
fn an_empty_repository_adopts_the_remote_object_format_and_a_used_one_refuses() {
    let t = Twin::build("sha256", Via::Ssh, false);
    let empty = t.root.join("empty-sha1");
    init(&empty, "sha1", false);
    run(&empty, &["remote", "add", "origin", &t.url]);
    let oracle = t.root.join("oracle");
    init(&oracle, "sha256", false);
    run(&oracle, &["remote", "add", "origin", &t.url]);
    let g = t.git_cmd(&oracle, &["fetch", "origin"]);
    assert_eq!(g.code, 0, "{}", g.stderr);

    let p = t.pyn_cmd(&empty, &["fetch", "origin"]);
    assert_eq!(p.code, 0, "{}", p.stderr);
    assert_eq!(
        run(&empty, &["rev-parse", "--show-object-format"]).trim(),
        "sha256"
    );
    assert_eq!(state(&empty), state(&oracle));
    fsck(&empty);

    let used = t.root.join("used-sha1");
    init(&used, "sha1", false);
    commit(&used, "a", b"a", "a");
    run(&used, &["remote", "add", "origin", &t.url]);
    let p = t.pyn_cmd(&used, &["fetch", "origin"]);
    let g = t.git_cmd(&used, &["fetch", "origin"]);
    assert_eq!((p.code, g.code), (128, 128), "{}", p.stderr);
    assert!(p.stderr.contains("mismatched algorithms"), "{}", p.stderr);
    assert_eq!(
        run(&used, &["rev-parse", "--show-object-format"]).trim(),
        "sha1"
    );
}

#[test]
fn remote_add_f_fetches_after_adding() {
    for via in [Via::Ssh, Via::Http] {
        let t = Twin::build("sha1", via, false);
        let (g, p) = t.both(&["remote", "add", "-f", "origin", &t.url]);
        assert_eq!(p.code, 0, "{}", p.stderr);
        assert!(g.stderr.contains("[new branch]") && p.stderr.contains("[new branch]"));
        assert!(run(&t.pyn, &["config", "remote.origin.url"]).trim() == t.url);
    }
}

#[test]
fn unsupported_and_invalid_options_are_clear_usage_errors() {
    let t = Twin::new("sha1", Via::Ssh);
    for args in [
        vec!["fetch", "--depth", "1"],
        vec!["fetch", "--depth=1", "origin"],
        vec!["fetch", "--shallow-since", "2020-01-01"],
        vec!["fetch", "--shallow-exclude", "main"],
        vec!["fetch", "--unshallow"],
        vec!["fetch", "--deepen", "2"],
        vec!["fetch", "--filter=blob:none"],
        vec!["fetch", "--atomic"],
        vec!["fetch", "--recurse-submodules"],
    ] {
        let p = t.pyn_cmd(&t.pyn, &args);
        assert_eq!(p.code, 129, "{args:?}: {}", p.stderr);
        assert!(
            p.stderr.contains("not built") || p.stderr.contains("not supported"),
            "{args:?}: {}",
            p.stderr
        );
    }
    assert!(run(&t.pyn, &["for-each-ref"]).is_empty());
    let p = t.pyn_cmd(&t.pyn, &["fetch", "--bogus"]);
    assert_eq!(p.code, 129);
    let p = t.pyn_cmd(&t.pyn, &["remote", "update"]);
    assert_eq!(p.code, 129, "remote update stays unsupported: {}", p.stderr);
    let none = Twin::build("sha1", Via::Ssh, false);
    let p = none.pyn_cmd(&none.pyn, &["fetch"]);
    let g = none.git_cmd(&none.git, &["fetch"]);
    assert_eq!(
        (p.code, g.code),
        (0, 0),
        "no remote at all is nothing to do"
    );
    assert_eq!((p.stderr.as_str(), g.stderr.as_str()), ("", ""));
}

#[test]
fn progress_is_shown_only_on_request_and_never_garbled() {
    let t = Twin::new("sha1", Via::Ssh);
    let plain = t.pyn_cmd(&t.pyn, &["fetch"]);
    assert!(
        !plain.stderr.contains("remote:") && !plain.stderr.contains('\r'),
        "{}",
        plain.stderr
    );
    advance(&t, "p.txt", "main");
    t.push(&["main"]);
    let shown = t.pyn_cmd(&t.pyn, &["fetch", "--progress"]);
    assert_eq!(shown.code, 0, "{}", shown.stderr);
    for line in shown.stderr.lines() {
        assert!(
            line.starts_with("remote: ")
                || line.starts_with("From ")
                || line.starts_with(' ')
                || line.contains('\r'),
            "{line:?}"
        );
    }
    advance(&t, "q.txt", "main");
    t.push(&["main"]);
    let quiet = t.pyn_cmd(&t.pyn, &["fetch", "-q", "--progress"]);
    assert_eq!(quiet.code, 0);
    assert!(!quiet.stderr.contains("From "), "{}", quiet.stderr);
    advance(&t, "r.txt", "main");
    t.push(&["main"]);
    let none = t.pyn_cmd(&t.pyn, &["fetch", "--no-progress"]);
    assert!(!none.stderr.contains("remote:"), "{}", none.stderr);
}

#[test]
fn outside_a_git_home_fetch_keeps_its_message() {
    let tmp = tempdir().unwrap();
    let p = Command::new(env!("CARGO_BIN_EXE_pyn"))
        .current_dir(tmp.path())
        .args(["fetch", "--depth", "1"])
        .env("HOME", tmp.path())
        .output()
        .unwrap();
    assert_eq!(p.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&p.stderr)
            .contains("'fetch' is a git command pyn does not support yet"),
        "{}",
        String::from_utf8_lossy(&p.stderr)
    );
}

#[test]
fn reflogs_follow_core_log_all_ref_updates() {
    let t = Twin::new("sha1", Via::Ssh);
    t.configure("core.logAllRefUpdates", "always");
    t.step(&["--tags"]);
    advance(&t, "l.txt", "main");
    t.work(&["tag", "-f", "v1"]);
    t.push(&["--force", "main", "v1"]);
    t.step(&["--tags", "--force"]);
    t.configure("core.logAllRefUpdates", "false");
    t.work(&["branch", "nolog", "main"]);
    t.push(&["nolog"]);
    t.step(&[]);
    assert!(!t.pyn.join(".git/logs/refs/remotes/origin/nolog").exists());
}

#[test]
fn a_ref_that_cannot_be_stored_fails_the_fetch_but_not_the_rest() {
    let t = Twin::new("sha1", Via::Ssh);
    t.work(&["branch", "dir", "main"]);
    t.push(&["dir"]);
    t.step(&[]);
    run(&t.bare, &["branch", "-D", "dir"]);
    t.work(&["branch", "-D", "dir"]);
    t.work(&["branch", "dir/x", "main"]);
    t.work(&["branch", "aaa", "main"]);
    t.push(&["dir/x", "aaa"]);
    let blocked = t.step(&[]);
    assert_eq!(blocked.code, 1, "{}", blocked.stderr);
    assert!(
        blocked.stderr.contains("unable to update local ref"),
        "{}",
        blocked.stderr
    );
    assert!(run(&t.pyn, &["branch", "-r"]).contains("origin/aaa"));
    t.step(&["--prune"]);
}
