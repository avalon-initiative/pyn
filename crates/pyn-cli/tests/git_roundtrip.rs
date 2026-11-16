//! Cross-tool round trips on one repository (decision #1032): pyn writes and real git reads, git writes and pyn reads,
//! and the two alternate. After every step both tools must agree on state (refs, trees, index, status, graph, blame),
//! and `git fsck --strict` must pass. Never compares messages. Needs `git` on the PATH (tests only).

#[path = "common/gitfix.rs"]
mod gitfix;
#[path = "common/oracle.rs"]
mod oracle;

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use gitfix::FORMATS;
use oracle::status_class;

#[derive(Clone, Copy, PartialEq)]
enum Tool {
    Pyn,
    Git,
}

struct Rig {
    format: &'static str,
    _tmp: tempfile::TempDir,
    dir: PathBuf,
    home: PathBuf,
    tick: std::cell::Cell<u64>,
    skipped: std::cell::RefCell<BTreeSet<&'static str>>,
    alternate: std::cell::Cell<bool>,
}

type Step = (&'static str, fn(&Rig, Tool));

impl Rig {
    fn new(format: &'static str) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let (dir, home) = (root.join("repo"), root.join("home"));
        fs::create_dir_all(&dir).unwrap();
        fs::create_dir_all(&home).unwrap();
        Self {
            format,
            _tmp: tmp,
            dir,
            home,
            tick: 0.into(),
            skipped: Default::default(),
            alternate: false.into(),
        }
    }

    fn command(&self, tool: Tool, cwd: &Path, args: &[&str]) -> Command {
        let mut cmd = match tool {
            Tool::Pyn => Command::new(env!("CARGO_BIN_EXE_pyn")),
            Tool::Git => {
                let mut c = Command::new("git");
                c.args(["-c", "gc.auto=0", "-c", "maintenance.auto=false"]);
                c
            }
        };
        let date = format!("{} +0000", 1_700_000_000 + self.tick.get() * 60);
        cmd.args(args)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .env("HOME", &self.home)
            .env_remove("XDG_CONFIG_HOME")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("TZ", "UTC")
            .env("GIT_EDITOR", "true")
            .env("GIT_AUTHOR_NAME", "Ann Author")
            .env("GIT_AUTHOR_EMAIL", "ann@example.com")
            .env("GIT_COMMITTER_NAME", "Cy Committer")
            .env("GIT_COMMITTER_EMAIL", "cy@example.com")
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
            .env("PYN_CONFIG_DIR", self.home.join("pyn-config"))
            .env_remove("PYN_SERVER")
            .env_remove("PYN_REPO")
            .env_remove("PYN_DIR");
        cmd
    }

    fn run(&self, tool: Tool, args: &[&str]) -> Output {
        self.command(tool, &self.dir, args).output().unwrap()
    }

    fn ok(&self, tool: Tool, args: &[&str]) -> String {
        let out = self.run(tool, args);
        assert!(
            out.status.success(),
            "{} {args:?} failed (exit {:?})\n{}",
            if tool == Tool::Pyn { "pyn" } else { "git" },
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    fn git(&self, args: &[&str]) -> String {
        self.ok(Tool::Git, args)
    }

    fn pyn(&self, args: &[&str]) -> String {
        self.ok(Tool::Pyn, args)
    }

    fn write(&self, name: &str, text: &str) {
        let file = self.dir.join(name);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        let _ = fs::remove_file(&file);
        fs::write(file, text).unwrap();
    }

    fn commit_file(&self, t: Tool, name: &str, text: &str, message: &str) {
        self.write(name, text);
        self.ok(t, &["add", name]);
        self.ok(t, &["commit", "-m", message]);
    }

    fn skip(&self, op: &'static str) {
        self.skipped.borrow_mut().insert(op);
    }

    fn init(&self, t: Tool) {
        match t {
            Tool::Pyn => self.pyn(&["init-git", "--object-format", self.format]),
            Tool::Git => self.git(&[
                "init",
                "-q",
                "-b",
                "main",
                &format!("--object-format={}", self.format),
            ]),
        };
    }

    /// Both tools see the same repository: refs, trees, index, status, graph and blame; fsck is clean.
    fn agree(&self, step: &str) {
        let (g, p) = (Tool::Git, Tool::Pyn);
        let fsck = self.run(g, &["fsck", "--strict", "--no-dangling"]);
        assert!(fsck.status.success(), "{step}: git fsck --strict failed");
        let same = |what: &str, git: &[&str], pyn: &[&str]| {
            let (a, b) = (self.ok(g, git), self.ok(p, pyn));
            assert_eq!(sorted(&a), sorted(&b), "{step}: {what}");
            a
        };
        same("HEAD", &["rev-parse", "HEAD"], &["rev-parse", "HEAD"]);
        same("refs", &["show-ref"], &["show-ref"]);
        same(
            "tree",
            &["ls-tree", "-r", "HEAD"],
            &["ls-tree", "-r", "HEAD"],
        );
        same("index", &["ls-files", "-s"], &["ls-files", "-s"]);
        same("unmerged", &["ls-files", "-u"], &["ls-files", "-u"]);
        let status = same(
            "status",
            &["status", "--porcelain=v1"],
            &["status", "--porcelain"],
        );
        let graph = same(
            "graph",
            &["log", "--all", "--format=%H %T %P"],
            &["log", "--all", "--format=%H %T %P"],
        );
        assert!(!graph.is_empty(), "{step}: empty graph");
        let raw = |t: Tool| sorted(&self.ok(t, &["diff", "--raw", "--no-abbrev"]));
        assert_eq!(raw(g), raw(p), "{step}: diff");
        if status.trim().is_empty() {
            assert!(self.ok(g, &["diff"]).is_empty(), "{step}: git diff");
            assert!(self.ok(p, &["diff"]).is_empty(), "{step}: pyn diff");
        }
        for file in ["a.txt", "dir/c.txt"] {
            if self.dir.join(file).is_file() && status.trim().is_empty() {
                let blame = |t: Tool| blame_map(&self.ok(t, &["blame", "--porcelain", file]));
                assert_eq!(blame(g), blame(p), "{step}: blame {file}");
            }
        }
    }

    fn clean(&self, step: &str) {
        for t in [Tool::Git, Tool::Pyn] {
            let args: &[&str] = if t == Tool::Git {
                &["status", "--porcelain=v1"]
            } else {
                &["status", "--porcelain"]
            };
            assert!(self.ok(t, args).trim().is_empty(), "{step}: not clean");
        }
    }
}

fn other(t: Tool) -> Tool {
    if t == Tool::Pyn { Tool::Git } else { Tool::Pyn }
}

fn sorted(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    lines.sort();
    lines
}

/// The (commit id, final line) pairs of `blame --porcelain`.
fn blame_map(text: &str) -> Vec<(String, String)> {
    let hex = |w: &str| w.len() >= 40 && w.chars().all(|c| c.is_ascii_hexdigit());
    text.lines()
        .filter_map(|l| {
            let w: Vec<&str> = l.split(' ').collect();
            (w.len() >= 3
                && hex(w[0])
                && w[1].parse::<u32>().is_ok()
                && w[2].parse::<u32>().is_ok())
            .then(|| (w[0].to_string(), w[2].to_string()))
        })
        .collect()
}

fn base(r: &Rig, t: Tool) {
    r.write("a.txt", "l1\nl2\nl3\n");
    r.write("run.sh", "#!/bin/sh\n");
    fs::set_permissions(r.dir.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    r.write("dir/c.txt", "c1\nc2\n");
    std::os::unix::fs::symlink("a.txt", r.dir.join("link")).unwrap();
    r.ok(t, &["add", "."]);
    r.ok(t, &["commit", "-m", "base"]);
}

fn clean_merge(r: &Rig, t: Tool) {
    r.ok(t, &["switch", "-c", "side"]);
    r.commit_file(t, "side.txt", "side\n", "side file");
    r.commit_file(t, "dir/c.txt", "c1\nc2\nc3\n", "side edit");
    r.ok(t, &["switch", "main"]);
    r.commit_file(t, "m.txt", "main\n", "main work");
    r.ok(t, &["merge", "--no-ff", "-m", "merge side", "side"]);
    r.clean("clean merge");
}

fn conflict(r: &Rig, t: Tool) {
    for (branch, text) in [("c1", "l1\nONE\nl3\n"), ("c2", "l1\nTWO\nl3\n")] {
        r.ok(t, &["switch", "-c", branch, "main"]);
        r.commit_file(t, "a.txt", text, branch);
    }
    r.ok(t, &["switch", "c1"]);
    let out = r.run(t, &["merge", "c2"]);
    assert_eq!(status_class(out.status.code().unwrap()), 1, "conflict exit");
    r.agree("conflicted merge");
    assert!(!r.git(&["ls-files", "-u"]).is_empty(), "conflict recorded");
    r.write("a.txt", "l1\nBOTH\nl3\n");
    r.ok(t, &["add", "a.txt"]);
    r.ok(t, &["commit", "-m", "resolve"]);
    r.ok(t, &["switch", "main"]);
    r.ok(t, &["merge", "--ff-only", "c1"]);
}

fn octopus(r: &Rig, t: Tool) {
    for n in ["o1", "o2", "o3"] {
        r.ok(t, &["switch", "-c", n, "main"]);
        r.commit_file(t, &format!("{n}.txt"), &format!("{n}\n"), n);
    }
    r.ok(t, &["switch", "main"]);
    r.ok(t, &["merge", "-m", "octopus", "o1", "o2", "o3"]);
}

fn rebase(r: &Rig, t: Tool) {
    r.ok(t, &["switch", "-c", "topic"]);
    r.commit_file(t, "t1.txt", "t1\n", "topic one");
    r.commit_file(t, "t2.txt", "t2\n", "topic two");
    r.ok(t, &["switch", "main"]);
    r.commit_file(t, "m2.txt", "m2\n", "main moves");
    r.ok(t, &["switch", "topic"]);
    r.ok(t, &["rebase", "main"]);
    r.ok(t, &["switch", "main"]);
}

fn pick_revert(r: &Rig, t: Tool) {
    r.ok(t, &["cherry-pick", "topic"]);
    r.agree("cherry-pick");
    r.ok(t, &["revert", "--no-edit", "HEAD"]);
}

fn tags(r: &Rig, t: Tool) {
    r.ok(t, &["tag", "light"]);
    r.ok(t, &["tag", "-a", "-m", "annotated", "v1", "HEAD~1"]);
}

fn notes(r: &Rig, t: Tool) {
    if t == Tool::Pyn {
        r.skip("notes");
        return;
    }
    r.git(&["notes", "add", "-m", "a note", "HEAD"]);
}

fn stash(r: &Rig, t: Tool) {
    r.write("a.txt", "l1\nl2\nstashed\n");
    r.write("m.txt", "changed\n");
    let pushed = r.run(t, &["stash"]);
    if !pushed.status.success() {
        // git cannot stash in a SHA-256 repository before a later release.
        assert!(t == Tool::Git && r.format == "sha256", "stash failed");
        r.skip("git stash (sha256)");
        r.git(&["checkout", "--", "."]);
        return;
    }
    r.clean("stash");
    r.agree("stash pushed");
    let popper = if r.alternate.get() { other(t) } else { t };
    if !r.run(popper, &["stash", "pop"]).status.success() {
        assert!(
            popper == Tool::Git && r.format == "sha256",
            "stash pop failed"
        );
        r.skip("git stash pop (sha256)");
        r.pyn(&["stash", "pop"]);
    }
    r.ok(t, &["add", "."]);
    r.ok(t, &["commit", "-m", "unstashed"]);
}

fn pull(r: &Rig, t: Tool) {
    let remote = r.dir.parent().unwrap().join("remote");
    let _ = fs::remove_dir_all(&remote);
    r.git(&["clone", "-q", ".", remote.to_str().unwrap()]);
    fs::write(remote.join("r.txt"), "remote\n").unwrap();
    for args in [&["add", "r.txt"][..], &["commit", "-m", "remote work"]] {
        let out = r.command(Tool::Git, &remote, args).output().unwrap();
        assert!(out.status.success());
    }
    r.commit_file(t, "l.txt", "local\n", "local work");
    r.ok(t, &["remote", "add", "origin", remote.to_str().unwrap()]);
    r.ok(t, &["pull", "--no-rebase", "--no-edit", "origin", "main"]);
}

fn repack_gc(r: &Rig, t: Tool) {
    r.ok(t, &["repack", "-a", "-d"]);
    r.agree("repack");
    r.ok(t, &["gc"]);
    r.agree("gc");
    r.commit_file(t, "after.txt", "after gc\n", "after gc");
}

const STEPS: [Step; 11] = [
    ("commit", base),
    ("clean merge", clean_merge),
    ("conflicted merge", conflict),
    ("octopus", octopus),
    ("rebase", rebase),
    ("cherry-pick and revert", pick_revert),
    ("tags", tags),
    ("notes", notes),
    ("stash", stash),
    ("pull", pull),
    ("repack and gc", repack_gc),
];

/// Runs every step with the tool `who(step index)` and checks agreement after each.
fn scenario(format: &'static str, who: fn(usize) -> Tool, alternate: bool) {
    let r = Rig::new(format);
    r.alternate.set(alternate);
    r.init(who(0));
    for (i, (name, step)) in STEPS.iter().enumerate() {
        r.tick.set(i as u64 * 100);
        step(&r, who(i));
        r.agree(name);
    }
    for op in r.skipped.borrow().iter() {
        eprintln!("skipped: {op}");
    }
}

fn each_format(who: fn(usize) -> Tool, alternate: bool) {
    for format in FORMATS {
        scenario(format, who, alternate);
    }
}

#[test]
fn pyn_writes_git_reads() {
    each_format(|_| Tool::Pyn, false);
}

#[test]
fn git_writes_pyn_reads() {
    each_format(|_| Tool::Git, false);
}

#[test]
fn alternating_starting_with_pyn() {
    each_format(|i| if i % 2 == 0 { Tool::Pyn } else { Tool::Git }, true);
}

#[test]
fn alternating_starting_with_git() {
    each_format(|i| if i % 2 == 0 { Tool::Git } else { Tool::Pyn }, true);
}
