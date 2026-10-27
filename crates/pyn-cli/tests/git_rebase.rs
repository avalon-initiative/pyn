//! `pyn rebase` on a git repository, compared with the real `git rebase` on a copy of the same repository: the exit
//! status, the commits made (same ids), the refs, the index, the work tree, the stash and the `rebase-merge` state
//! (never the messages; the reflog subjects of `HEAD` only where a test sets `Pair::reflog`). Needs `git` on the PATH
//! (tests only).

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use common::gitfix::copy_tree;

const IDENTITY: [(&str, &str); 6] = [
    ("GIT_AUTHOR_NAME", "T"),
    ("GIT_AUTHOR_EMAIL", "t@example.com"),
    ("GIT_COMMITTER_NAME", "C"),
    ("GIT_COMMITTER_EMAIL", "c@example.com"),
    ("GIT_AUTHOR_DATE", "1700000000 +0000"),
    ("GIT_COMMITTER_DATE", "1700000500 +0000"),
];

struct Env {
    home: PathBuf,
}

impl Env {
    fn git(&self, cwd: &Path, args: &[&str]) -> Output {
        self.git_env(cwd, args, &[])
    }

    fn git_env(&self, cwd: &Path, args: &[&str], envs: &[(&str, &str)]) -> Output {
        Command::new("git")
            .args([
                "-c",
                "gc.auto=0",
                "-c",
                "maintenance.auto=false",
                "-c",
                "merge.conflictStyle=diff3",
                "-c",
                "advice.detachedHead=false",
            ])
            .args(args)
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join("xdg"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_EDITOR", "true")
            .env("GIT_SSH_COMMAND", self.home.join("../ssh-shim"))
            .envs(IDENTITY)
            .envs(envs.iter().copied())
            .output()
            .expect("git is installed")
    }

    fn ok(&self, cwd: &Path, args: &[&str]) -> String {
        let out = self.git(cwd, args);
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn pyn(&self, cwd: &Path, args: &[&str]) -> Output {
        self.pyn_env(cwd, args, &[])
    }

    fn pyn_env(&self, cwd: &Path, args: &[&str], envs: &[(&str, &str)]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_pyn"))
            .args(args)
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join("xdg"))
            .env("TZ", "UTC")
            .env("GIT_EDITOR", "true")
            .env("GIT_SSH_COMMAND", self.home.join("../ssh-shim"))
            .envs(IDENTITY)
            .env("PYN_CONFIG_DIR", self.home.join("pyn-config"))
            .env_remove("PYN_SERVER")
            .env_remove("PYN_REPO")
            .envs(envs.iter().copied())
            .output()
            .unwrap()
    }
}

fn write(dir: &Path, name: &str, text: &str) {
    let file = dir.join(name);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    let _ = fs::remove_file(&file);
    fs::write(file, text).unwrap();
}

fn write_bytes(dir: &Path, name: &str, bytes: &[u8]) {
    let file = dir.join(name);
    let _ = fs::remove_file(&file);
    fs::write(file, bytes).unwrap();
}

fn set_exec(dir: &Path, name: &str, on: bool) {
    let file = dir.join(name);
    let mut perms = fs::metadata(&file).unwrap().permissions();
    perms.set_mode(if on { 0o755 } else { 0o644 });
    fs::set_permissions(&file, perms).unwrap();
}

fn files_under(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let rel = path.strip_prefix(root).unwrap().display().to_string();
        if rel == ".git" {
            continue;
        }
        let meta = fs::symlink_metadata(&path).unwrap();
        if meta.is_dir() {
            files_under(root, &path, out);
        } else {
            let exec = meta.permissions().mode() & 0o111 != 0;
            out.insert(rel, format!("{exec} {:?}", fs::read(&path).unwrap()));
        }
    }
}

fn read(dir: &Path, name: &str) -> String {
    fs::read_to_string(dir.join(".git").join(name)).unwrap_or_else(|_| "(absent)".into())
}

/// `fixup -C <id> <subject>` and the other lines with the id cut to seven digits; git 2.55 writes `<id> # <subject>`.
fn normalized_line(l: &str) -> String {
    let mut w = l.splitn(2, ' ');
    let cmd = w.next().unwrap_or("");
    let mut rest = w.next().unwrap_or("");
    let mut flag = String::new();
    for f in ["-C ", "-c "] {
        if let Some(r) = rest.strip_prefix(f) {
            flag = format!(" {}", f.trim());
            rest = r;
        }
    }
    let mut w = rest.splitn(2, ' ');
    let id = w.next().unwrap_or("");
    let subject = w.next().unwrap_or("");
    let subject = subject.strip_prefix("# ").unwrap_or(subject);
    format!("{cmd}{flag} {} {subject}", &id[..id.len().min(7)])
}

/// The todo list with every id cut to seven digits (git abbreviates them, pyn writes them whole).
fn sheet(dir: &Path, name: &str) -> String {
    read(dir, &format!("rebase-merge/{name}"))
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(normalized_line)
        .collect::<Vec<_>>()
        .join("\n")
}

/// The text without git 2.55's `# Conflicts:` list, which 2.43 does not write.
fn without_conflict_list(text: String) -> String {
    match text.split_once("\n# Conflicts:\n") {
        Some((before, _)) => format!("{before}\n"),
        None => text,
    }
}

/// What a rebase may change: refs, pseudo-refs, the state folder, the index, the work tree and the stash.
fn state(env: &Env, dir: &Path) -> Vec<String> {
    let rebasing = dir.join(".git/rebase-merge").is_dir();
    let mut files = BTreeMap::new();
    files_under(dir, dir, &mut files);
    let mut out = vec![
        read(dir, "HEAD"),
        env.ok(dir, &["for-each-ref"]),
        read(dir, "ORIG_HEAD"),
        // git leaves REBASE_HEAD behind after some interactive rebases; pyn removes it.
        if rebasing {
            read(dir, "REBASE_HEAD")
        } else {
            String::new()
        },
        read(dir, "MERGE_HEAD"),
        without_conflict_list(read(dir, "MERGE_MSG")),
        format!("{rebasing}"),
    ];
    for name in [
        "head-name",
        "onto",
        "orig-head",
        "msgnum",
        "end",
        "stopped-sha",
        "message",
        "author-script",
        "autostash",
    ] {
        let text = read(dir, &format!("rebase-merge/{name}"));
        // git 2.43 ends `message` with an extra newline; 2.55 appends the `# Conflicts:` comment lines. Neither is
        // part of the commit message.
        out.push(if name == "message" {
            text.lines()
                .filter(|l| !l.starts_with('#'))
                .collect::<Vec<_>>()
                .join("\n")
                .trim_end()
                .to_string()
        } else {
            text
        });
    }
    out.push(sheet(dir, "git-rebase-todo"));
    out.push(sheet(dir, "done"));
    out.push(env.ok(dir, &["ls-files", "-s"]));
    // git leaves the chain's message files behind after the chain; they only matter while it is stopped.
    let in_chain = sheet(dir, "done")
        .lines()
        .last()
        .is_some_and(|l| l.starts_with("squash") || l.starts_with("fixup"));
    for name in ["amend", "message-squash", "message-fixup", "current-fixups"] {
        let text = read(dir, &format!("rebase-merge/{name}"))
            .trim_end()
            .to_string();
        out.push(if name == "amend" || in_chain {
            text
        } else {
            String::new()
        });
    }
    out.push(without_conflict_list(read(dir, "SQUASH_MSG")));
    out.push(
        env.ok(dir, &["stash", "list", "--format=%H %s"])
            .replace(' ', "_"),
    );
    out.push(format!("{files:?}"));
    for name in ["patch", "rewritten-list", "rewritten-pending"] {
        out.push(read(dir, &format!("rebase-merge/{name}")));
    }
    for name in [
        "rebase-merge/squash-onto",
        "rebase-merge/refs-to-delete",
        "rebase-merge/update-refs",
        "MERGE_MODE",
    ] {
        out.push(read(dir, name));
    }
    // git also writes AUTO_MERGE for clean picks; it only matters while a merge waits for its conflicts.
    out.push(if dir.join(".git/MERGE_HEAD").exists() {
        read(dir, "AUTO_MERGE")
    } else {
        String::new()
    });
    let log = env.ok(dir, &["log", "-g", "--format=%gs", "HEAD"]);
    // git before 2.55 logs a merge under the action of the command before it, which is a pick in these tests.
    out.push(if git_before_2_55() {
        log.replace("(merge): ", "(pick): ")
    } else {
        log
    });
    out
}

fn git_before_2_55() -> bool {
    static OLD: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OLD.get_or_init(|| {
        let out = Command::new("git").arg("--version").output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let minor: u32 = text
            .split('.')
            .nth(1)
            .and_then(|m| m.parse().ok())
            .unwrap_or(0);
        minor < 55
    })
}

fn part_name(i: usize) -> String {
    const FIXED: [&str; 7] = [
        "HEAD",
        "refs",
        "ORIG_HEAD",
        "REBASE_HEAD",
        "MERGE_HEAD",
        "MERGE_MSG",
        "rebase-merge exists",
    ];
    const FILES: [&str; 9] = [
        "head-name",
        "onto",
        "orig-head",
        "msgnum",
        "end",
        "stopped-sha",
        "message",
        "author-script",
        "autostash",
    ];
    match i {
        0..=6 => FIXED[i].into(),
        7..=15 => format!("rebase-merge/{}", FILES[i - 7]),
        16 => "git-rebase-todo".into(),
        17 => "done".into(),
        18 => "ls-files".into(),
        19 => "rebase-merge/amend".into(),
        20 => "rebase-merge/message-squash".into(),
        21 => "rebase-merge/message-fixup".into(),
        22 => "rebase-merge/current-fixups".into(),
        23 => "SQUASH_MSG".into(),
        24 => "stash list".into(),
        25 => "work tree".into(),
        26 => "rebase-merge/patch".into(),
        27 => "rebase-merge/rewritten-list".into(),
        28 => "rebase-merge/rewritten-pending".into(),
        29 => "rebase-merge/squash-onto".into(),
        30 => "rebase-merge/refs-to-delete".into(),
        31 => "rebase-merge/update-refs".into(),
        32 => "MERGE_MODE".into(),
        33 => "AUTO_MERGE (merging)".into(),
        _ => "HEAD reflog subjects".into(),
    }
}

struct Pair {
    _tmp: tempfile::TempDir,
    env: Env,
    git: PathBuf,
    pyn: PathBuf,
    /// Also compare the reflog subjects of `HEAD`.
    reflog: std::cell::Cell<bool>,
}

impl Pair {
    fn root() -> (tempfile::TempDir, Env, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let git = root.join("git");
        fs::create_dir_all(&git).unwrap();
        (tmp, Env { home }, git)
    }

    fn copied(tmp: tempfile::TempDir, env: Env, git: PathBuf) -> Self {
        let pyn = git.parent().unwrap().join("pyn");
        let _ = fs::remove_dir_all(&pyn);
        copy_tree(&git, &pyn);
        let _ = env.git(&git, &["update-index", "-q", "--refresh"]);
        Self {
            _tmp: tmp,
            env,
            git,
            pyn,
            reflog: std::cell::Cell::new(false),
        }
    }

    /// Runs `rebase <args>` in both; the exit status and every piece of state must agree. Returns the status.
    fn same(&self, args: &[&str], label: &str) -> i32 {
        self.same_env(args, label, &[])
    }

    /// [`Pair::same`] with extra environment variables (the editors) for both.
    fn same_env(&self, args: &[&str], label: &str, envs: &[(&str, &str)]) -> i32 {
        let mut full = vec!["rebase"];
        full.extend(args);
        self.same_command(&full, label, envs)
    }

    /// Runs any command in both; the exit status and the state must agree.
    fn same_command(&self, full: &[&str], label: &str, envs: &[(&str, &str)]) -> i32 {
        let args = &full[1..];
        let g = self.env.git_env(&self.git, full, envs);
        let p = self.env.pyn_env(&self.pyn, full, envs);
        let what = format!("{label}: {} {args:?}", full[0]);
        assert_eq!(
            p.status.code(),
            g.status.code(),
            "{what}: exit status; pyn said {}; git said {}",
            String::from_utf8_lossy(&p.stderr),
            String::from_utf8_lossy(&g.stderr)
        );
        let (gs, ps) = (state(&self.env, &self.git), state(&self.env, &self.pyn));
        for (i, (a, b)) in ps.iter().zip(&gs).enumerate() {
            // git 2.56 records `fixup -c <id>` where earlier versions wrote `fixup <id>`
            let same_fixups =
                i == 22 && a.replace("fixup -c ", "fixup ") == b.replace("fixup -c ", "fixup ");
            if a != b && !same_fixups && (i != 34 || self.reflog.get()) {
                panic!(
                    "{what}: state part {i} ({}) differs\npyn: {a:?}\ngit: {b:?}\npyn said {}",
                    part_name(i),
                    String::from_utf8_lossy(&p.stderr)
                );
            }
        }
        let fsck = self.env.git(&self.pyn, &["fsck", "--strict"]);
        assert!(
            fsck.status.success(),
            "{what}: fsck: {}",
            String::from_utf8_lossy(&fsck.stderr)
        );
        g.status.code().unwrap()
    }

    /// Writes a file in both and stages it, as a person resolving a conflict does.
    fn resolve(&self, name: &str, text: &str) {
        for dir in [&self.git, &self.pyn] {
            write(dir, name, text);
            let out = self.env.pyn(dir, &["add", name]);
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }

    fn both(&self, f: impl Fn(&Env, &Path)) {
        f(&self.env, &self.git);
        f(&self.env, &self.pyn);
    }

    fn on(&self, branch: &str) {
        self.both(|env, d| {
            env.ok(d, &["checkout", "-q", branch]);
        });
    }
}

fn init(env: &Env, dir: &Path, format: &str) {
    env.ok(
        dir,
        &[
            "init",
            "-q",
            "-b",
            "main",
            &format!("--object-format={format}"),
        ],
    );
    env.ok(dir, &["config", "gc.auto", "0"]);
    env.ok(dir, &["config", "maintenance.auto", "false"]);
}

static TICK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1_600_000_000);

/// Commits everything with a later date each time, so history order never depends on ties.
fn commit_all(env: &Env, dir: &Path, message: &str) {
    let tick = TICK.fetch_add(10, std::sync::atomic::Ordering::SeqCst);
    commit_at(env, dir, message, tick);
}

fn commit_at(env: &Env, dir: &Path, message: &str, tick: u64) {
    env.ok(dir, &["add", "-A"]);
    let out = Command::new("git")
        .args([
            "-c",
            "gc.auto=0",
            "-c",
            "maintenance.auto=false",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            message,
        ])
        .current_dir(dir)
        .env("HOME", &env.home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .envs(IDENTITY)
        .env("GIT_AUTHOR_DATE", format!("{tick} +0000"))
        .env("GIT_COMMITTER_DATE", format!("{tick} +0000"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

const FILE: &str = "one\ntwo\nthree\nfour\nfive\n";

fn with_line(n: usize, text: &str) -> String {
    FILE.lines()
        .enumerate()
        .map(|(i, l)| if i + 1 == n { text } else { l })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

/// From `base`: `main` (`m1` to `m3`), `side` (`s1` to `s4`, then the merge `mc` of `topic`'s `t1`), `clean` (`c1`,
/// `c2`, no conflicts), `dup` (`d1` repeats `m1`'s change, then `d2`) and `old` (just `base`).
/// `s2` conflicts with `m2` in `g`, `s4` with `m3` in the binary `bin`.
fn history(format: &str) -> Pair {
    let (tmp, env, git) = Pair::root();
    init(&env, &git, format);
    write(&git, "f", FILE);
    write(&git, "g", "g0\n");
    write(&git, "x", "#!/bin/sh\n");
    set_exec(&git, "x", true);
    write_bytes(&git, "bin", &[0, 1, 2, 3]);
    commit_all(&env, &git, "base");
    env.ok(&git, &["tag", "base"]);
    env.ok(&git, &["branch", "old"]);
    env.ok(&git, &["checkout", "-q", "-b", "clean", "base"]);
    write(&git, "c1", "c1\n");
    commit_all(
        &env,
        &git,
        "c1 first line\n\nbody of c1\n\nReviewed-by: Q <q@example.com>",
    );
    write(&git, "c2", "c2\n");
    set_exec(&git, "c2", true);
    commit_all(&env, &git, "c2");
    env.ok(&git, &["checkout", "-q", "-b", "dup", "base"]);
    write(&git, "f", &with_line(5, "M5"));
    commit_all(&env, &git, "d1 same as m1");
    write(&git, "d2", "d2\n");
    commit_all(&env, &git, "d2");
    env.ok(&git, &["checkout", "-q", "-b", "topic", "base"]);
    write(&git, "k", "topic\n");
    commit_all(&env, &git, "t1");
    env.ok(&git, &["checkout", "-q", "-b", "side", "base"]);
    write(&git, "f", &with_line(2, "S1"));
    commit_all(&env, &git, "s1");
    write(&git, "h", "h\n");
    write(&git, "g", "gS\n");
    commit_all(&env, &git, "s2 adds h and changes g");
    write(&git, "f", &with_line(3, "S3"));
    commit_all(&env, &git, "s3");
    write_bytes(&git, "bin", &[0, 9, 9]);
    set_exec(&git, "x", false);
    commit_all(&env, &git, "s4 binary and mode");
    env.ok(&git, &["tag", "s4"]);
    env.ok(&git, &["merge", "-q", "--no-ff", "-m", "mc", "topic"]);
    env.ok(&git, &["checkout", "-q", "main"]);
    write(&git, "f", &with_line(5, "M5"));
    commit_all(&env, &git, "m1");
    env.ok(&git, &["tag", "m1"]);
    write(&git, "g", "gM\n");
    commit_all(&env, &git, "m2");
    write_bytes(&git, "bin", &[0, 5, 5]);
    commit_all(&env, &git, "m3");
    Pair::copied(tmp, env, git)
}

fn both_formats(run: impl Fn(&str)) {
    for format in ["sha1", "sha256"] {
        run(format);
    }
}

#[test]
fn a_clean_rebase_replays_every_commit_with_the_same_ids() {
    both_formats(|f| {
        let p = history(f);
        p.on("clean");
        assert_eq!(p.same(&["main"], &format!("{f} clean")), 0);
        let p = history(f);
        p.on("clean");
        assert_eq!(p.same(&["-q", "main"], &format!("{f} quiet")), 0);
        let p = history(f);
        assert_eq!(
            p.same(&["main", "clean"], &format!("{f} branch argument")),
            0
        );
        let p = history(f);
        p.on("clean");
        assert_eq!(p.same(&["--onto", "m1", "base"], &format!("{f} onto")), 0);
    });
}

#[test]
fn up_to_date_fast_forward_and_no_ff() {
    both_formats(|f| {
        let p = history(f);
        assert_eq!(p.same(&["base"], &format!("{f} ahead of upstream")), 0);
        assert_eq!(p.same(&["main"], &format!("{f} same")), 0);
        assert_eq!(p.same(&["--no-ff", "main"], &format!("{f} no-ff")), 0);
        let p = history(f);
        p.on("old");
        assert_eq!(p.same(&["main"], &format!("{f} behind")), 0);
        let p = history(f);
        p.on("clean");
        assert_eq!(p.same(&["base"], &format!("{f} on its base")), 0);
        assert_eq!(p.same(&["--no-ff", "base"], &format!("{f} forced")), 0);
        assert_eq!(p.same(&["-f", "base"], &format!("{f} forced again")), 0);
    });
}

#[test]
fn commits_whose_change_is_upstream_are_dropped() {
    both_formats(|f| {
        let p = history(f);
        p.on("dup");
        assert_eq!(p.same(&["main"], &format!("{f} dup")), 0);
    });
}

#[test]
fn merge_commits_are_flattened() {
    both_formats(|f| {
        let p = history(f);
        p.on("side");
        p.both(|env, d| {
            env.ok(d, &["reset", "-q", "--hard", "s4"]);
        });
        p.both(|env, d| {
            env.ok(d, &["merge", "-q", "--no-ff", "-m", "mc", "topic"]);
        });
        assert_eq!(
            p.same(&["--onto", "base", "base"], &format!("{f} noop onto")),
            0
        );
        assert_eq!(
            p.same(&["--onto", "clean", "base"], &format!("{f} onto clean")),
            0
        );
    });
}

#[test]
fn a_conflict_stops_and_continue_commits_the_resolution() {
    both_formats(|f| {
        let p = history(f);
        p.on("side");
        assert_eq!(p.same(&["main"], &format!("{f} stop")), 1);
        assert_ne!(p.same(&["--continue"], &format!("{f} unresolved")), 0);
        assert_ne!(p.same(&["main"], &format!("{f} while stopped")), 0);
        p.resolve("g", "gM+gS\n");
        assert_eq!(p.same(&["--continue"], &format!("{f} resolved")), 1);
        p.resolve("bin", "resolved\n");
        assert_eq!(p.same(&["--continue"], &format!("{f} last")), 0);
        assert_ne!(p.same(&["--continue"], &format!("{f} nothing left")), 0);
    });
}

#[test]
fn skip_drops_the_stopped_commit_and_goes_on() {
    both_formats(|f| {
        let p = history(f);
        p.on("side");
        assert_eq!(p.same(&["main"], &format!("{f} stop")), 1);
        assert_eq!(p.same(&["--skip"], &format!("{f} skip")), 1);
        assert_eq!(p.same(&["--skip"], &format!("{f} skip last")), 0);
        assert_ne!(p.same(&["--skip"], &format!("{f} skip none")), 0);
    });
}

#[test]
fn abort_puts_everything_back() {
    both_formats(|f| {
        let p = history(f);
        p.on("side");
        assert_eq!(p.same(&["main"], &format!("{f} stop")), 1);
        assert_eq!(p.same(&["--abort"], &format!("{f} abort")), 0);
        assert_ne!(p.same(&["--abort"], &format!("{f} abort none")), 0);
        let p = history(f);
        p.on("side");
        assert_eq!(p.same(&["main"], &format!("{f} stop")), 1);
        p.resolve("g", "gM+gS\n");
        assert_eq!(p.same(&["--continue"], &format!("{f} second stop")), 1);
        assert_eq!(p.same(&["--abort"], &format!("{f} abort later")), 0);
        let p = history(f);
        assert_eq!(
            p.same(&["main", "side"], &format!("{f} branch argument stop")),
            1
        );
        assert_eq!(p.same(&["--abort"], &format!("{f} abort to branch")), 0);
    });
}

#[test]
fn quit_forgets_the_rebase_and_keeps_the_files() {
    both_formats(|f| {
        let p = history(f);
        p.on("side");
        assert_ne!(p.same(&["--quit"], &format!("{f} quit none")), 0);
        assert_eq!(p.same(&["main"], &format!("{f} stop")), 1);
        assert_eq!(p.same(&["--quit"], &format!("{f} quit")), 0);
    });
}

#[test]
fn a_detached_head_is_rebased_and_stays_detached() {
    both_formats(|f| {
        let p = history(f);
        p.both(|env, d| {
            env.ok(d, &["checkout", "-q", "--detach", "clean"]);
        });
        assert_eq!(p.same(&["main"], &format!("{f} detached")), 0);
        let p = history(f);
        p.both(|env, d| {
            env.ok(d, &["checkout", "-q", "--detach", "side"]);
        });
        assert_eq!(p.same(&["main"], &format!("{f} detached stop")), 1);
        assert_eq!(p.same(&["--abort"], &format!("{f} detached abort")), 0);
    });
}

#[test]
fn local_changes_refuse_a_rebase_unless_autostash_is_given() {
    both_formats(|f| {
        let p = history(f);
        p.on("clean");
        p.both(|_, d| write(d, "c1", "edited\n"));
        assert_eq!(p.same(&["main"], &format!("{f} unstaged")), 1);
        p.both(|env, d| {
            env.ok(d, &["add", "c1"]);
        });
        assert_eq!(p.same(&["main"], &format!("{f} staged")), 1);
        assert_eq!(
            p.same(&["--autostash", "main"], &format!("{f} autostash")),
            0
        );
        let p = history(f);
        p.on("clean");
        p.both(|env, d| {
            env.ok(d, &["config", "rebase.autoStash", "true"]);
            write(d, "c1", "edited\n");
        });
        assert_eq!(p.same(&["main"], &format!("{f} configured")), 0);
        p.both(|_, d| write(d, "c1", "again\n"));
        assert_eq!(
            p.same(&["--no-autostash", "base"], &format!("{f} declined")),
            1
        );
    });
}

#[test]
fn an_autostash_that_conflicts_is_kept_in_the_stash() {
    both_formats(|f| {
        let p = history(f);
        p.on("clean");
        p.both(|_, d| write(d, "f", "mine\n"));
        let g = p.env.git(&p.git, &["rebase", "--autostash", "main"]);
        let y = p.env.pyn(&p.pyn, &["rebase", "--autostash", "main"]);
        assert_eq!(
            g.status.code(),
            y.status.code(),
            "{}",
            String::from_utf8_lossy(&y.stderr)
        );
        let ids = |d: &Path| p.env.ok(d, &["rev-parse", "HEAD", "refs/heads/clean"]);
        assert_eq!(ids(&p.git), ids(&p.pyn));
        assert_eq!(
            p.env.ok(&p.git, &["stash", "list"]).lines().count(),
            p.env.ok(&p.pyn, &["stash", "list"]).lines().count()
        );
        let tracked = |d: &Path| read(d, "../f");
        assert_eq!(tracked(&p.git).len(), tracked(&p.pyn).len());
    });
}

#[test]
fn refusals_leave_nothing_behind() {
    both_formats(|f| {
        let p = history(f);
        p.on("clean");
        assert_ne!(p.same(&["nonesuch"], &format!("{f} bad upstream")), 0);
        assert_ne!(
            p.same(&["--onto", "nonesuch", "main"], &format!("{f} bad onto")),
            0
        );
        assert_ne!(p.same(&["main", "nonesuch"], &format!("{f} bad branch")), 0);
        assert_ne!(p.same(&[], &format!("{f} no upstream")), 0);
        assert_ne!(p.same(&["--continue"], &format!("{f} continue none")), 0);
        assert_ne!(p.same(&["--skip"], &format!("{f} skip none")), 0);
    });
}

#[test]
fn git_continues_what_pyn_stopped() {
    both_formats(|f| {
        let p = history(f);
        p.on("side");
        let stopped = p.env.pyn(&p.pyn, &["rebase", "main"]);
        assert_eq!(stopped.status.code(), Some(1));
        write(&p.pyn, "g", "resolved\n");
        p.env.ok(&p.pyn, &["add", "g"]);
        let again = p.env.git(&p.pyn, &["rebase", "--continue"]);
        assert_eq!(
            again.status.code(),
            Some(1),
            "{}",
            String::from_utf8_lossy(&again.stderr)
        );
        write(&p.pyn, "bin", "resolved\n");
        p.env.ok(&p.pyn, &["add", "bin"]);
        p.env.ok(&p.pyn, &["rebase", "--continue"]);
        assert!(!p.pyn.join(".git/rebase-merge").exists());
        assert_eq!(
            p.env.ok(&p.pyn, &["log", "--format=%s", "-6"]),
            "t1\ns4 binary and mode\ns3\ns2 adds h and changes g\ns1\nm3\n"
        );
    });
}

#[test]
fn pyn_continues_what_git_stopped() {
    both_formats(|f| {
        let p = history(f);
        p.on("side");
        let g = p.env.git(&p.git, &["rebase", "main"]);
        assert_eq!(g.status.code(), Some(1));
        write(&p.git, "g", "resolved\n");
        p.env.ok(&p.git, &["add", "g"]);
        let out = p.env.pyn(&p.git, &["rebase", "--continue"]);
        assert_eq!(
            out.status.code(),
            Some(1),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        write(&p.git, "bin", "resolved\n");
        p.env.ok(&p.git, &["add", "bin"]);
        p.env
            .ok(&p.git, &["-c", "core.editor=true", "rebase", "--continue"]);
        assert!(!p.git.join(".git/rebase-merge").exists());
        assert_eq!(
            p.env.ok(&p.git, &["log", "--format=%s", "-6"]),
            "t1\ns4 binary and mode\ns3\ns2 adds h and changes g\ns1\nm3\n"
        );
        let p = history(f);
        p.on("side");
        assert_eq!(
            p.env.git(&p.git, &["rebase", "main"]).status.code(),
            Some(1)
        );
        let out = p.env.pyn(&p.git, &["rebase", "--abort"]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            p.env.ok(&p.git, &["rev-parse", "side"]),
            p.env.ok(&p.git, &["rev-parse", "HEAD"])
        );
    });
}

#[test]
fn the_other_unbuilt_options_are_usage_errors() {
    let p = history("sha1");
    p.on("clean");
    for flag in ["--apply", "--keep-base"] {
        let out = p.env.pyn(&p.pyn, &["rebase", flag, "main"]);
        assert_eq!(out.status.code(), Some(129), "{flag}");
    }
}

fn seq(expr: &str) -> String {
    format!("sed -i -e '{expr}'")
}

impl Pair {
    /// A commit with the same dates in both.
    fn commit(&self, name: &str, text: &str, message: &str) {
        let tick = TICK.fetch_add(10, std::sync::atomic::Ordering::SeqCst);
        for dir in [&self.git, &self.pyn] {
            write(dir, name, text);
            commit_at(&self.env, dir, message, tick);
        }
    }

    /// `clean` (c1, c2) with a `fixup!`, a `squash!` and a nested `fixup!` commit after them.
    fn with_fixups(&self) {
        self.on("clean");
        self.commit("y1", "y1\n", "fixup! c1 first line");
        self.commit("y2", "y2\n", "squash! c2");
        self.commit("y3", "y3\n", "fixup! fixup! c1 first line");
    }

    fn sheet(&self, expr: &str) -> [(&'static str, String); 1] {
        [("GIT_SEQUENCE_EDITOR", seq(expr))]
    }
}

#[test]
fn an_unedited_sheet_replays_like_a_plain_rebase() {
    both_formats(|f| {
        let p = history(f);
        p.on("clean");
        let keep = p.sheet("1s/^pick/pick/");
        let envs: Vec<(&str, &str)> = keep.iter().map(|(k, v)| (*k, v.as_str())).collect();
        assert_eq!(p.same_env(&["-i", "main"], &format!("{f} -i"), &envs), 0);
        let p = history(f);
        p.on("clean");
        assert_eq!(
            p.same_env(&["-i", "base"], &format!("{f} -i on its base"), &envs),
            0
        );
        let p = history(f);
        p.on("clean");
        assert_eq!(
            p.same_env(&["-i", "clean"], &format!("{f} -i nothing"), &envs),
            0
        );
    });
}

fn run_sheet(p: &Pair, expr: &str, args: &[&str], label: &str, more: &[(&str, &str)]) -> i32 {
    let sheet = seq(expr);
    let mut envs = vec![("GIT_SEQUENCE_EDITOR", sheet.as_str())];
    envs.extend_from_slice(more);
    let mut full = vec!["-i"];
    full.extend_from_slice(args);
    p.same_env(&full, label, &envs)
}

#[test]
fn reorder_and_drop() {
    both_formats(|f| {
        let p = history(f);
        p.on("clean");
        assert_eq!(
            run_sheet(&p, "1{h;d};2{G}", &["main"], &format!("{f} swap"), &[]),
            0
        );
        let p = history(f);
        p.on("clean");
        assert_eq!(
            run_sheet(&p, "1s/^pick/drop/", &["main"], &format!("{f} drop"), &[]),
            0
        );
        let p = history(f);
        p.on("clean");
        assert_eq!(
            run_sheet(
                &p,
                "1s/^pick/d/;2s/^pick/p/",
                &["main"],
                &format!("{f} letters"),
                &[]
            ),
            0
        );
    });
}

#[test]
fn reword_uses_the_message_the_editor_leaves() {
    both_formats(|f| {
        let p = history(f);
        p.on("clean");
        let editor = seq("1s/^/Reworded: /");
        let more = [("GIT_EDITOR", editor.as_str())];
        assert_eq!(
            run_sheet(
                &p,
                "1s/^pick/reword/;2s/^pick/r/",
                &["main"],
                &format!("{f} reword"),
                &more
            ),
            0
        );
        let p = history(f);
        p.on("clean");
        assert_eq!(
            run_sheet(
                &p,
                "1s/^pick/reword/",
                &["base"],
                &format!("{f} reword in place"),
                &more
            ),
            0
        );
        let p = history(f);
        p.on("clean");
        let empty = seq("1,$d");
        let none = [("GIT_EDITOR", empty.as_str())];
        let code = run_sheet(
            &p,
            "1s/^pick/reword/",
            &["main"],
            &format!("{f} empty message"),
            &none,
        );
        assert_ne!(code, 0);
    });
}

#[test]
fn edit_stops_and_continue_goes_on() {
    both_formats(|f| {
        let p = history(f);
        p.on("clean");
        assert_eq!(
            run_sheet(&p, "1s/^pick/edit/", &["main"], &format!("{f} edit"), &[]),
            0
        );
        assert_eq!(p.same(&["--continue"], &format!("{f} continue")), 0);
        let p = history(f);
        p.on("clean");
        assert_eq!(
            run_sheet(&p, "1s/^pick/edit/", &["main"], &format!("{f} edit"), &[]),
            0
        );
        p.both(|_, d| write(d, "extra", "x\n"));
        p.both(|env, d| {
            env.ok(d, &["add", "extra"]);
        });
        let editor = seq("1s/^/Amended: /");
        let more = [("GIT_EDITOR", editor.as_str())];
        assert_eq!(
            p.same_env(&["--continue"], &format!("{f} staged"), &more),
            0
        );
        let p = history(f);
        p.on("clean");
        assert_eq!(
            run_sheet(&p, "1s/^pick/edit/", &["main"], &format!("{f} edit"), &[]),
            0
        );
        assert_eq!(
            p.same_command(
                &["commit", "--amend", "-m", "by hand"],
                &format!("{f} amend"),
                &[]
            ),
            0
        );
        p.both(|_, d| write(d, "more", "m\n"));
        p.both(|env, d| {
            env.ok(d, &["add", "more"]);
        });
        assert_eq!(
            p.same_command(
                &["commit", "-m", "another"],
                &format!("{f} new commit"),
                &[]
            ),
            0
        );
        assert_eq!(p.same(&["--continue"], &format!("{f} continue after")), 0);
        let p = history(f);
        p.on("clean");
        assert_eq!(
            run_sheet(&p, "1s/^pick/edit/", &["main"], &format!("{f} edit"), &[]),
            0
        );
        p.both(|_, d| write(d, "c1", "dirty\n"));
        assert_eq!(p.same(&["--continue"], &format!("{f} dirty")), 1);
        let p = history(f);
        p.on("clean");
        assert_eq!(
            run_sheet(&p, "1s/^pick/edit/", &["main"], &format!("{f} edit"), &[]),
            0
        );
        assert_eq!(p.same(&["--skip"], &format!("{f} skip")), 0);
        let p = history(f);
        p.on("clean");
        assert_eq!(
            run_sheet(&p, "1s/^pick/edit/", &["main"], &format!("{f} edit"), &[]),
            0
        );
        assert_eq!(p.same(&["--abort"], &format!("{f} abort")), 0);
    });
}

#[test]
fn squash_and_fixup_meld_commits_and_their_messages() {
    both_formats(|f| {
        for expr in [
            "2s/^pick/squash/",
            "2s/^pick/fixup/",
            "2s/^pick/s/;1s/^pick/r/",
        ] {
            let p = history(f);
            p.on("clean");
            let editor = seq("1s/^/Melded: /");
            let more = [("GIT_EDITOR", editor.as_str())];
            assert_eq!(
                run_sheet(&p, expr, &["main"], &format!("{f} {expr}"), &more),
                0
            );
        }
        let p = history(f);
        p.on("clean");
        assert_eq!(
            run_sheet(
                &p,
                "1,2s/^pick/edit/",
                &["base"],
                &format!("{f} edit twice"),
                &[]
            ),
            0
        );
        let p = history(f);
        p.on("side");
        assert_eq!(
            run_sheet(
                &p,
                "3s/^pick/fixup/;4s/^pick/squash/",
                &["main"],
                &format!("{f} chain"),
                &[]
            ),
            1
        );
    });
}

#[test]
fn a_conflict_in_a_chain_keeps_the_messages_for_continue_and_skip() {
    both_formats(|f| {
        for resolution in ["continue", "skip"] {
            for word in ["squash", "fixup"] {
                let p = history(f);
                p.on("side");
                let expr = format!("2s/^pick/{word}/");
                let label = format!("{f} {word} {resolution}");
                assert_eq!(run_sheet(&p, &expr, &["main"], &label, &[]), 1);
                if resolution == "continue" {
                    p.resolve("g", "gM+gS\n");
                    assert_eq!(p.same(&["--continue"], &format!("{label} resolved")), 1);
                } else {
                    assert_eq!(p.same(&["--skip"], &format!("{label} skipped")), 1);
                }
                p.resolve("bin", "resolved\n");
                assert_eq!(p.same(&["--continue"], &format!("{label} last")), 0);
            }
        }
        let p = history(f);
        p.on("side");
        assert_eq!(
            run_sheet(
                &p,
                "2s/^pick/reword/",
                &["main"],
                &format!("{f} reword stop"),
                &[]
            ),
            1
        );
        p.resolve("g", "gM+gS\n");
        let editor = seq("1s/^/Fixed: /");
        let more = [("GIT_EDITOR", editor.as_str())];
        assert_eq!(
            p.same_env(&["--continue"], &format!("{f} reword resolved"), &more),
            1
        );
    });
}

#[test]
fn exec_runs_commands_and_stops_when_one_fails() {
    both_formats(|f| {
        let p = history(f);
        p.on("clean");
        assert_eq!(
            run_sheet(
                &p,
                "1a exec test -f c1",
                &["main"],
                &format!("{f} exec"),
                &[]
            ),
            0
        );
        let p = history(f);
        p.on("clean");
        assert_eq!(
            run_sheet(&p, "1a exec false", &["main"], &format!("{f} failing"), &[]),
            1
        );
        assert_eq!(p.same(&["--continue"], &format!("{f} continue past it")), 0);
        let p = history(f);
        p.on("clean");
        assert_eq!(
            run_sheet(
                &p,
                "1a exec echo dirt >> c1",
                &["main"],
                &format!("{f} dirty"),
                &[]
            ),
            1
        );
        let p = history(f);
        p.on("clean");
        assert_eq!(
            run_sheet(&p, "1a break", &["main"], &format!("{f} break"), &[]),
            0
        );
        assert_eq!(p.same(&["--continue"], &format!("{f} after break")), 0);
        let p = history(f);
        p.on("clean");
        assert_eq!(
            p.same(
                &["-x", "test -f c1", "-x", "true", "main"],
                &format!("{f} -x")
            ),
            0
        );
        let p = history(f);
        p.on("clean");
        assert_eq!(
            p.same(&["--exec", "false", "main"], &format!("{f} --exec fails")),
            1
        );
        assert_eq!(p.same(&["--continue"], &format!("{f} --exec continue")), 1);
    });
}

#[test]
fn autosquash_moves_fixup_commits_under_their_targets() {
    both_formats(|f| {
        let p = history(f);
        p.with_fixups();
        assert_eq!(
            p.same_env(
                &["-i", "--autosquash", "main"],
                &format!("{f} autosquash"),
                &[("GIT_SEQUENCE_EDITOR", ":")]
            ),
            0
        );
        let p = history(f);
        p.with_fixups();
        p.both(|env, d| {
            env.ok(d, &["config", "rebase.autosquash", "true"]);
        });
        let editor = seq("1s/^/Squashed: /");
        let envs = [
            ("GIT_SEQUENCE_EDITOR", ":"),
            ("GIT_EDITOR", editor.as_str()),
        ];
        assert_eq!(
            p.same_env(&["-i", "main"], &format!("{f} configured"), &envs),
            0
        );
        let p = history(f);
        p.with_fixups();
        p.both(|env, d| {
            env.ok(d, &["config", "rebase.autosquash", "true"]);
        });
        assert_eq!(
            p.same_env(
                &["-i", "--no-autosquash", "main"],
                &format!("{f} declined"),
                &[("GIT_SEQUENCE_EDITOR", ":")]
            ),
            0
        );
        // git 2.43 ignores --autosquash without -i and later versions apply it, so only pyn is checked.
        let p = history(f);
        p.with_fixups();
        let out = p.env.pyn(&p.pyn, &["rebase", "--autosquash", "main"]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            p.env.ok(
                &p.pyn,
                &["log", "--format=%s", "-3", "--reverse", "main..HEAD"]
            ),
            "c1 first line\nc2\n"
        );
    });
}

#[test]
fn a_bad_sheet_stops_and_edit_todo_fixes_it() {
    both_formats(|f| {
        let p = history(f);
        p.on("clean");
        assert_eq!(
            run_sheet(&p, "2s/^pick/bogus/", &["main"], &format!("{f} bogus"), &[]),
            1
        );
        assert_eq!(p.same(&["--continue"], &format!("{f} still bogus")), 1);
        let fix = seq("s/^bogus/pick/");
        let envs = [("GIT_SEQUENCE_EDITOR", fix.as_str())];
        assert_eq!(
            p.same_env(&["--edit-todo"], &format!("{f} edit-todo"), &envs),
            0
        );
        assert_eq!(p.same(&["--continue"], &format!("{f} fixed")), 0);
        let p = history(f);
        p.on("clean");
        assert_eq!(
            run_sheet(
                &p,
                "1s/^pick/squash/",
                &["main"],
                &format!("{f} squash first"),
                &[]
            ),
            1
        );
        let p = history(f);
        p.on("clean");
        assert_eq!(
            run_sheet(
                &p,
                "/^pick/d",
                &["main"],
                &format!("{f} nothing to do"),
                &[]
            ),
            1
        );
    });
}

#[test]
fn git_and_pyn_continue_each_others_interactive_stops() {
    both_formats(|f| {
        for edit_word in ["edit", "squash"] {
            let p = history(f);
            p.on("side");
            let expr = format!("2s/^pick/{edit_word}/");
            let sheet = seq(&expr);
            let envs = [("GIT_SEQUENCE_EDITOR", sheet.as_str())];
            let stopped = p.env.pyn_env(&p.pyn, &["rebase", "-i", "main"], &envs);
            let reference = p.env.git_env(&p.git, &["rebase", "-i", "main"], &envs);
            assert_eq!(stopped.status.code(), reference.status.code());
            for (dir, git_first) in [(&p.pyn, true), (&p.git, false)] {
                write(dir, "g", "gM+gS\n");
                p.env.ok(dir, &["add", "g"]);
                let step = |who_git: bool, dir: &Path| {
                    if who_git {
                        p.env.git(dir, &["rebase", "--continue"])
                    } else {
                        p.env.pyn(dir, &["rebase", "--continue"])
                    }
                };
                let first = step(git_first, dir);
                write(dir, "bin", "resolved\n");
                p.env.ok(dir, &["add", "bin"]);
                let second = step(git_first, dir);
                assert!(
                    first.status.code().is_some() && second.status.code().is_some(),
                    "{f} {edit_word}"
                );
                let _ = (first, second);
            }
            let ids = |d: &Path| p.env.ok(d, &["rev-parse", "HEAD"]);
            let log = |d: &Path| p.env.ok(d, &["log", "--format=%H %s"]);
            assert_eq!(log(&p.git), log(&p.pyn), "{f} {edit_word}: {}", ids(&p.git));
            assert!(!p.git.join(".git/rebase-merge").exists());
            assert!(!p.pyn.join(".git/rebase-merge").exists());
        }
    });
}

impl Pair {
    /// `clean` (c1, c2) with two more commits, then an `amend!` commit for c1 and a `fixup!` one.
    fn with_amend(&self) {
        self.on("clean");
        self.commit("c3", "c3\n", "c3 third\n\nbody of c3");
        self.commit("c4", "c4\n", "c4 fourth");
        self.commit(
            "y1",
            "y1\n",
            "amend! c1 first line\n\nc1 reworded\n\nnew body",
        );
        self.commit("y2", "y2\n", "fixup! c2");
    }

    fn config(&self, key: &str, value: &str) {
        self.both(|env, d| {
            env.ok(d, &["config", key, value]);
        });
    }
}

fn editor_env<'a>(sheet: &'a str, message: &'a str) -> [(&'a str, &'a str); 2] {
    [("GIT_SEQUENCE_EDITOR", sheet), ("GIT_EDITOR", message)]
}

#[test]
fn amend_commits_are_autosquashed_as_fixup_dash_c() {
    both_formats(|f| {
        let p = history(f);
        p.with_amend();
        assert_eq!(
            p.same_env(
                &["-i", "--autosquash", "main"],
                &format!("{f} amend! autosquash"),
                &[("GIT_SEQUENCE_EDITOR", ":")]
            ),
            0
        );
        let p = history(f);
        p.with_amend();
        let editor = seq("1s/^/Edited: /");
        let sheet = seq("s/^fixup -C/fixup -c/");
        assert_eq!(
            p.same_env(
                &["-i", "--autosquash", "main"],
                &format!("{f} amend! autosquash edited"),
                &editor_env(&sheet, &editor)
            ),
            0
        );
    });
}

#[test]
fn fixup_dash_c_and_dash_lowercase_c_replace_the_message() {
    both_formats(|f| {
        let sheets = [
            "2s/^pick/fixup -C/",
            "2s/^pick/fixup -c/",
            "2s/^pick/fixup/;3s/^pick/fixup -C/",
            "2s/^pick/fixup -c/;3s/^pick/fixup -C/",
            "2s/^pick/fixup -C/;3s/^pick/fixup -c/",
            "2s/^pick/fixup -C/;3s/^pick/squash/",
            "2s/^pick/squash/;3s/^pick/fixup -C/",
            "2s/^pick/fixup -C/;3s/^pick/fixup/;4s/^pick/fixup -c/",
            "2s/^pick/fixup -c/;3s/^pick/fixup/;4s/^pick/fixup/",
            "3s/^pick/fixup -C/",
            "3s/^pick/fixup -c/;4s/^pick/squash/",
            "2s/^pick/squash/;3s/^pick/fixup -C/;4s/^pick/fixup/",
        ];
        for expr in sheets {
            let p = history(f);
            p.with_amend();
            let editor = seq("1s/^/Edited: /");
            let sheet = seq(expr);
            // The branch has c1 to c4, an `amend!` and a `fixup!` commit; lines 5 and 6 stay picks.
            assert_eq!(
                p.same_env(
                    &["-i", "main"],
                    &format!("{f} {expr}"),
                    &editor_env(&sheet, &editor)
                ),
                0,
                "{expr}"
            );
        }
    });
}

#[test]
fn marker_subjects_are_commented_out_after_a_squash() {
    both_formats(|f| {
        for expr in [
            "5s/^pick/squash/",
            "5s/^pick/fixup -C/",
            "2s/^pick/squash/;5s/^pick/fixup -C/",
            "2s/^pick/squash/;6s/^pick/fixup -c/",
            "6s/^pick/fixup -C/",
            "5s/^pick/fixup -c/",
            "5s/^pick/fixup/;6s/^pick/squash/",
        ] {
            let p = history(f);
            p.with_amend();
            let editor = seq("1s/^/Edited: /");
            let sheet = seq(expr);
            assert_eq!(
                p.same_env(
                    &["-i", "main"],
                    &format!("{f} {expr}"),
                    &editor_env(&sheet, &editor)
                ),
                0,
                "{expr}"
            );
        }
    });
}

#[test]
fn a_conflict_in_a_fixup_dash_c_chain_keeps_the_messages() {
    both_formats(|f| {
        for resolution in ["continue", "skip"] {
            for word in ["fixup -C", "fixup -c"] {
                let p = history(f);
                p.on("side");
                let expr = format!("2s/^pick/{word}/");
                let label = format!("{f} {word} {resolution}");
                let editor = seq("1s/^/Edited: /");
                let sheet = seq(&expr);
                let envs = editor_env(&sheet, &editor);
                assert_eq!(p.same_env(&["-i", "main"], &label, &envs), 1);
                if resolution == "continue" {
                    p.resolve("g", "gM+gS\n");
                    assert_eq!(
                        p.same_env(&["--continue"], &format!("{label} resolved"), &envs),
                        1
                    );
                } else {
                    assert_eq!(
                        p.same_env(&["--skip"], &format!("{label} skipped"), &envs),
                        1
                    );
                }
                p.resolve("bin", "resolved\n");
                assert_eq!(
                    p.same_env(&["--continue"], &format!("{label} last"), &envs),
                    0
                );
            }
        }
    });
}

#[test]
fn a_bad_flag_on_a_fixup_is_an_invalid_line() {
    both_formats(|f| {
        let p = history(f);
        p.on("clean");
        let sheet = seq("2s/^pick/fixup -x/");
        let envs = [("GIT_SEQUENCE_EDITOR", sheet.as_str())];
        let g = p.env.git_env(&p.git, &["rebase", "-i", "main"], &envs);
        let q = p.env.pyn_env(&p.pyn, &["rebase", "-i", "main"], &envs);
        assert_eq!(q.status.code(), g.status.code(), "{f}");
        assert!(p.pyn.join(".git/rebase-merge").is_dir());
    });
}

#[test]
fn missing_commits_check_warns_or_stops_on_a_dropped_line() {
    both_formats(|f| {
        for level in ["ignore", "warn", "error", "bogus"] {
            let p = history(f);
            p.on("clean");
            p.config("rebase.missingCommitsCheck", level);
            let label = format!("{f} {level}");
            let expected = i32::from(level == "error");
            assert_eq!(
                run_sheet(&p, "1d", &["main"], &label, &[]),
                expected,
                "{label}"
            );
            if level != "error" {
                continue;
            }
            assert_eq!(
                p.same_env(
                    &["--continue"],
                    &format!("{label} continue"),
                    &[("GIT_SEQUENCE_EDITOR", ":")]
                ),
                1
            );
            assert_eq!(
                p.same_env(
                    &["--edit-todo"],
                    &format!("{label} edit-todo"),
                    &[("GIT_SEQUENCE_EDITOR", ":")]
                ),
                1
            );
            let restore = "cp .git/rebase-merge/git-rebase-todo.backup";
            let fix = format!("{restore} \"$1\"; sed -i -e '1s/^pick/drop/' \"$1\" #");
            assert_eq!(
                p.same_env(
                    &["--edit-todo"],
                    &format!("{label} fixed"),
                    &[("GIT_SEQUENCE_EDITOR", fix.as_str())]
                ),
                0
            );
            assert_eq!(p.same(&["--continue"], &format!("{label} done")), 0);
        }
        let p = history(f);
        p.on("clean");
        p.config("rebase.missingCommitsCheck", "error");
        assert_eq!(
            run_sheet(
                &p,
                "1s/^pick/drop/",
                &["main"],
                &format!("{f} drop ok"),
                &[]
            ),
            0
        );
    });
}

/// The `#` lines of the sheet the editor was given, saved by the editor command itself.
fn seen_comments(dir: &Path) -> Vec<String> {
    read(dir, "seen-sheet")
        .lines()
        .filter(|l| l.starts_with('#'))
        .map(|l| l.replace("git rebase", "pyn rebase"))
        .collect()
}

fn seen_commands(dir: &Path) -> Vec<String> {
    read(dir, "seen-sheet")
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
        .map(normalized_line)
        .collect()
}

#[test]
fn the_editor_sees_git_s_full_comment_block_and_the_instruction_format() {
    both_formats(|f| {
        for (strict, format) in [(false, None), (true, Some("%s (%an) %ae"))] {
            let p = history(f);
            p.with_amend();
            p.commit("e1", "e1\n", "empty one");
            p.both(|env, d| {
                env.ok(d, &["commit", "-q", "--allow-empty", "-m", "empty two"]);
            });
            if strict {
                p.config("rebase.missingCommitsCheck", "warn");
            }
            if let Some(format) = format {
                p.config("rebase.instructionFormat", format);
            }
            let save = ("GIT_SEQUENCE_EDITOR", "cp \"$1\" .git/seen-sheet #");
            assert_eq!(
                p.same_env(&["-i", "main"], &format!("{f} sheet"), &[save]),
                0
            );
            assert_eq!(seen_comments(&p.pyn), seen_comments(&p.git), "{f} {strict}");
            assert_eq!(seen_commands(&p.pyn), seen_commands(&p.git), "{f} {strict}");
            assert!(seen_comments(&p.pyn).len() > 20);
        }
        let p = history(f);
        p.on("clean");
        p.config("rebase.instructionFormat", "%s [%an]");
        let envs = [("GIT_SEQUENCE_EDITOR", "sed -i -e '1s/^pick/edit/'")];
        assert_eq!(
            p.same_env(&["-i", "main"], &format!("{f} format state"), &envs),
            0
        );
    });
}

#[test]
fn the_progress_line_counts_the_commands() {
    both_formats(|f| {
        let p = history(f);
        p.on("clean");
        let run = |quiet: bool| {
            let mut args = vec!["rebase", "main"];
            if quiet {
                args.push("-q");
            }
            let g = p.env.git(&p.git, &args);
            let q = p.env.pyn(&p.pyn, &args);
            (
                String::from_utf8_lossy(&g.stderr).into_owned(),
                String::from_utf8_lossy(&q.stderr).into_owned(),
            )
        };
        let (g, q) = run(false);
        for out in [&g, &q] {
            assert!(
                out.contains("Rebasing (1/2)") && out.contains("Rebasing (2/2)"),
                "{f}: {out:?}"
            );
        }
        let p2 = history(f);
        p2.on("clean");
        let g = p2.env.git(&p2.git, &["rebase", "-q", "main"]);
        let q = p2.env.pyn(&p2.pyn, &["rebase", "-q", "main"]);
        assert!(!String::from_utf8_lossy(&g.stderr).contains("Rebasing"));
        assert!(!String::from_utf8_lossy(&q.stderr).contains("Rebasing"));
    });
}

impl Pair {
    /// A note on each revision, made by git in both repositories.
    fn note(&self, rev: &str, text: &str) {
        self.both(|env, d| {
            env.ok(d, &["notes", "add", "-m", text, rev]);
        });
    }

    fn notes(&self, label: &str) -> String {
        let (g, p) = (
            self.env.ok(&self.git, &["notes", "list"]),
            self.env.ok(&self.pyn, &["notes", "list"]),
        );
        assert_eq!(p, g, "{label}: notes list");
        g
    }
}

#[test]
fn rewritten_list_follows_each_command_and_stop() {
    both_formats(|f| {
        let cases: [(&str, &str, &[&str]); 7] = [
            ("pick break", "1a break", &["main"]),
            ("squash break", "2s/^pick/squash/;2a break", &["main"]),
            (
                "fixup chain break",
                "2s/^pick/fixup/;3s/^pick/fixup -C/;3a break",
                &["main"],
            ),
            ("drop", "1s/^pick/drop/;2a break", &["main"]),
            ("exec", "1a exec true\n1a break", &["main"]),
            ("unchanged leading picks", "1a break", &["base"]),
            ("reordered ff pick", "1i break", &["base"]),
        ];
        for (name, expr, args) in cases {
            let p = history(f);
            p.on("clean");
            let label = format!("{f} {name}");
            assert_eq!(run_sheet(&p, expr, args, &label, &[]), 0, "{label}");
            assert_eq!(p.same(&["--continue"], &format!("{label} continue")), 0);
        }
        let p = history(f);
        p.on("dup");
        assert_eq!(
            run_sheet(
                &p,
                "$a break",
                &["main"],
                &format!("{f} dropped as upstream"),
                &[]
            ),
            0
        );
        assert_eq!(p.same(&["--continue"], &format!("{f} dup continue")), 0);
    });
}

#[test]
fn rewritten_list_after_edit_conflict_and_skip() {
    both_formats(|f| {
        let p = history(f);
        p.on("side");
        assert_eq!(
            run_sheet(&p, "1s/^pick/edit/", &["main"], &format!("{f} edit"), &[]),
            0
        );
        p.both(|env, d| {
            env.ok(d, &["commit", "-q", "--amend", "-m", "amended"]);
        });
        assert_eq!(p.same(&["--continue"], &format!("{f} edit continue")), 1);
        assert_eq!(p.same(&["--skip"], &format!("{f} skip")), 1);
        p.resolve("bin", "resolved\n");
        assert_eq!(p.same(&["--continue"], &format!("{f} last")), 0);
        let p = history(f);
        p.on("side");
        assert_eq!(p.same(&["main"], &format!("{f} plain conflict")), 1);
        p.resolve("g", "gM+gS\n");
        assert_eq!(p.same(&["--continue"], &format!("{f} plain resolved")), 1);
    });
}

#[test]
fn notes_are_copied_to_the_rewritten_commits() {
    both_formats(|f| {
        for (name, args, expr) in [
            ("plain", vec!["main"], None),
            ("squashed", vec!["-i", "main"], Some("2s/^pick/squash/")),
            ("fixup c", vec!["-i", "main"], Some("2s/^pick/fixup -C/")),
            ("dropped", vec!["-i", "main"], Some("2s/^pick/drop/")),
            ("onto base", vec!["base"], None),
        ] {
            let p = history(f);
            p.on("clean");
            p.config("notes.rewriteRef", "refs/notes/commits");
            p.note("clean~1", "note one");
            p.note("clean", "note two");
            let editor = seq("1s/^/Edited: /");
            let sheet = seq(expr.unwrap_or("1s/^pick/pick/"));
            let label = format!("{f} {name}");
            let mut full: Vec<&str> = args.clone();
            if args[0] != "-i" {
                full.insert(0, "--no-fork-point");
            }
            let envs = editor_env(&sheet, &editor);
            assert_eq!(p.same_env(&full, &label, &envs), 0, "{label}");
            let listed = p.notes(&label);
            if name != "dropped" {
                assert!(listed.lines().count() >= 2, "{label}: {listed}");
            }
        }
    });
}

#[test]
fn note_rewriting_follows_its_settings() {
    both_formats(|f| {
        let setups: [(&str, &[(&str, &str)]); 7] = [
            ("off", &[("notes.rewrite.rebase", "false")]),
            ("no ref", &[]),
            ("glob", &[("notes.rewriteRef", "refs/notes/*")]),
            ("overwrite", &[("notes.rewriteMode", "overwrite")]),
            ("ignore", &[("notes.rewriteMode", "ignore")]),
            ("cat_sort_uniq", &[("notes.rewriteMode", "cat_sort_uniq")]),
            ("concatenate", &[("notes.rewriteMode", "concatenate")]),
        ];
        for (name, settings) in setups {
            let p = history(f);
            p.on("clean");
            if name != "no ref" {
                p.config("notes.rewriteRef", "refs/notes/commits");
            }
            for (k, v) in settings {
                p.config(k, v);
            }
            p.note("clean~1", "shared\nfirst");
            p.note("clean", "shared\nsecond");
            let editor = seq("1s/^/Edited: /");
            let sheet = seq("2s/^pick/squash/");
            let label = format!("{f} {name}");
            let envs = editor_env(&sheet, &editor);
            assert_eq!(p.same_env(&["-i", "main"], &label, &envs), 0);
            p.notes(&label);
        }
    });
}

// Rebasing merges (`--rebase-merges`), `--root`, `--update-refs` and the fork point.

/// From `base`: `main` (`m1`, `m2` on other files), `side` (`s1`, the merge `Merge branch 'topic' into side` of
/// `topic` (`t1`, `t2`), `s2`), `csid` (`cs1` on `m1`, then the merge of `cousin`, a branch from `base`), `conf`
/// (`x1`, the merge of `ytopic` resolved by hand after a conflict in `f`, `x2`) and `two` (`o1`, a merge of `ta`, then
/// a merge of `tb`).
fn merge_history(format: &str) -> Pair {
    let (tmp, env, git) = Pair::root();
    init(&env, &git, format);
    write(&git, "f", FILE);
    commit_all(&env, &git, "base");
    env.ok(&git, &["tag", "base"]);
    let branch = |name: &str, from: &str| {
        env.ok(&git, &["checkout", "-q", "-b", name, from]);
    };
    let merge = |what: &str, message: &str| {
        env.ok(&git, &["merge", "-q", "--no-ff", "-m", message, what]);
    };
    branch("topic", "base");
    write(&git, "t1", "t1\n");
    commit_all(&env, &git, "t1");
    write(&git, "t2", "t2\n");
    commit_all(&env, &git, "t2");
    branch("cousin", "base");
    write(&git, "c1", "c1\n");
    commit_all(&env, &git, "c1 on a cousin branch");
    branch("ytopic", "base");
    write(&git, "f", &with_line(2, "Y"));
    commit_all(&env, &git, "y1");
    branch("ta", "base");
    write(&git, "ta", "ta\n");
    commit_all(&env, &git, "ta1");
    branch("tb", "base");
    write(&git, "tb", "tb\n");
    commit_all(&env, &git, "tb1");
    branch("main2", "base");
    write(&git, "m1", "m1\n");
    commit_all(&env, &git, "m1");
    env.ok(&git, &["tag", "m1"]);
    write(&git, "m2", "m2\n");
    commit_all(&env, &git, "m2");
    env.ok(&git, &["branch", "-M", "main"]);
    branch("side", "base");
    write(&git, "s1", "s1\n");
    commit_all(&env, &git, "s1");
    merge("topic", "Merge branch 'topic' into side");
    write(&git, "s2", "s2\n");
    commit_all(&env, &git, "s2");
    branch("csid", "m1");
    write(&git, "cs1", "cs1\n");
    commit_all(&env, &git, "cs1");
    merge("cousin", "Merge branch 'cousin'");
    write(&git, "cs2", "cs2\n");
    commit_all(&env, &git, "cs2");
    branch("conf", "base");
    write(&git, "f", &with_line(2, "X"));
    commit_all(&env, &git, "x1");
    let out = env.git(
        &git,
        &[
            "merge",
            "--no-ff",
            "-m",
            "Merge branch 'ytopic' into conf",
            "ytopic",
        ],
    );
    assert_eq!(out.status.code(), Some(1));
    write(&git, "f", &with_line(2, "R"));
    env.ok(&git, &["add", "f"]);
    env.ok(&git, &["commit", "-q", "--no-edit"]);
    write(&git, "x2", "x2\n");
    commit_all(&env, &git, "x2");
    branch("two", "base");
    write(&git, "o1", "o1\n");
    commit_all(&env, &git, "o1");
    merge("ta", "Merge branch 'ta' into two");
    write(&git, "o2", "o2\n");
    commit_all(&env, &git, "o2");
    merge("tb", "Merge branch 'tb' into two");
    env.ok(&git, &["checkout", "-q", "main"]);
    Pair::copied(tmp, env, git)
}

/// The editor of a sheet: a script that writes `lines` (with `$(git rev-parse ..)` expanded) to the file it is given.
fn sheet_script(p: &Pair, lines: &str) -> String {
    let script = p.git.parent().unwrap().join("sheet.sh");
    fs::write(&script, format!("cat > \"$1\" <<EOF\n{lines}EOF\n")).unwrap();
    format!("sh {}", script.display())
}

#[test]
fn merges_are_recreated_with_the_same_ids() {
    both_formats(|f| {
        for (branch, name) in [("side", "side"), ("csid", "cousin"), ("two", "two merges")] {
            for args in [
                &["-r", "main"][..],
                &["--rebase-merges", "main"],
                &["--rebase-merges=no-rebase-cousins", "main"],
                &["--rebase-merges=rebase-cousins", "main"],
                &["-r", "--no-ff", "main"],
                &["-r", "-q", "main"],
            ] {
                let p = merge_history(f);
                p.reflog.set(true);
                p.on(branch);
                let label = format!("{f} {name} {args:?}");
                assert_eq!(p.same(args, &label), 0, "{label}");
            }
        }
        let p = merge_history(f);
        p.reflog.set(true);
        p.on("side");
        assert_eq!(
            p.same_env(&["-i", "-r", "main"], &format!("{f} -i"), &[]),
            0
        );
        let p = merge_history(f);
        p.reflog.set(true);
        p.on("side");
        assert_eq!(
            p.same(&["--no-rebase-merges", "main"], &format!("{f} flat")),
            0
        );
        let p = merge_history(f);
        p.reflog.set(true);
        p.on("side");
        p.config("rebase.rebaseMerges", "true");
        assert_eq!(p.same(&["main"], &format!("{f} configured")), 0);
        p.config("rebase.rebaseMerges", "rebase-cousins");
        let p = merge_history(f);
        p.on("csid");
        p.config("rebase.rebaseMerges", "rebase-cousins");
        assert_eq!(p.same(&["main"], &format!("{f} configured cousins")), 0);
    });
}

#[test]
fn the_editor_sees_the_merge_sheet_and_the_cousin_modes() {
    both_formats(|f| {
        for (branch, mode) in [
            ("side", "-r"),
            ("csid", "--rebase-merges=no-rebase-cousins"),
            ("csid", "--rebase-merges=rebase-cousins"),
            ("two", "-r"),
        ] {
            let p = merge_history(f);
            p.on(branch);
            let save = ("GIT_SEQUENCE_EDITOR", "cp \"$1\" .git/seen-sheet #");
            let label = format!("{f} {branch} {mode}");
            assert_eq!(
                p.same_env(&["-i", mode, "main"], &label, &[save]),
                0,
                "{label}"
            );
            assert_eq!(seen_comments(&p.pyn), seen_comments(&p.git), "{label}");
            assert_eq!(seen_commands(&p.pyn), seen_commands(&p.git), "{label}");
            assert!(
                seen_commands(&p.git)
                    .iter()
                    .any(|l| l.starts_with("merge -C")),
                "{label}"
            );
        }
    });
}

#[test]
fn a_conflicting_merge_stops_and_continue_skip_abort_work() {
    both_formats(|f| {
        let resolved = with_line(2, "R2");
        let p = merge_history(f);
        p.reflog.set(true);
        p.on("conf");
        assert_eq!(p.same(&["-r", "main"], &format!("{f} stop")), 1);
        assert!(p.pyn.join(".git/MERGE_HEAD").exists());
        p.resolve("f", &resolved);
        assert_eq!(p.same(&["--continue"], &format!("{f} continue")), 0);

        let p = merge_history(f);
        p.reflog.set(true);
        p.on("conf");
        assert_eq!(p.same(&["-r", "main"], &format!("{f} stop for skip")), 1);
        assert_eq!(p.same(&["--skip"], &format!("{f} skip")), 0);

        let p = merge_history(f);
        p.reflog.set(true);
        p.on("conf");
        assert_eq!(p.same(&["-r", "main"], &format!("{f} stop for abort")), 1);
        assert_eq!(p.same(&["--abort"], &format!("{f} abort")), 0);

        let p = merge_history(f);
        p.on("conf");
        assert_eq!(p.same(&["-r", "main"], &format!("{f} stop for quit")), 1);
        assert_eq!(p.same(&["--quit"], &format!("{f} quit")), 0);

        let p = merge_history(f);
        p.on("conf");
        assert_eq!(p.same(&["-r", "main"], &format!("{f} stop, unresolved")), 1);
        assert_eq!(p.same(&["--continue"], &format!("{f} unresolved")), 1);

        let p = merge_history(f);
        p.on("conf");
        assert_eq!(
            p.same(&["-r", "main"], &format!("{f} resolved as first parent")),
            1
        );
        p.resolve("f", &with_line(2, "X"));
        assert_eq!(p.same(&["--continue"], &format!("{f} same tree")), 0);
    });
}

#[test]
fn git_and_pyn_continue_each_others_merge_stops() {
    both_formats(|f| {
        let p = merge_history(f);
        p.on("conf");
        assert_eq!(
            p.env.pyn(&p.pyn, &["rebase", "-r", "main"]).status.code(),
            Some(1)
        );
        assert_eq!(
            p.env.git(&p.git, &["rebase", "-r", "main"]).status.code(),
            Some(1)
        );
        for dir in [&p.pyn, &p.git] {
            write(dir, "f", &with_line(2, "R2"));
            p.env.ok(dir, &["add", "f"]);
        }
        let out = p.env.git(&p.pyn, &["rebase", "--continue"]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let out = p.env.pyn(&p.git, &["rebase", "--continue"]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        for dir in [&p.pyn, &p.git] {
            assert!(!dir.join(".git/rebase-merge").exists());
        }
        let log = |d: &Path| p.env.ok(d, &["log", "--format=%H %P", "-3"]);
        assert_eq!(log(&p.pyn), log(&p.git), "{f}");
    });
}

/// The sheet that rebuilds `side` onto the new base by hand, ending with `tail`.
const SIDE_SHEET: &str = "label onto
reset onto
pick $(git rev-parse topic~1) t1
pick $(git rev-parse topic) t2
label topic
reset onto
pick $(git rev-parse side~2) s1
";

fn run_text(p: &Pair, text: &str, args: &[&str], label: &str, more: &[(&str, &str)]) -> i32 {
    let editor = sheet_script(p, text);
    let mut envs = vec![("GIT_SEQUENCE_EDITOR", editor.as_str())];
    envs.extend_from_slice(more);
    let mut full = vec!["-i"];
    full.extend_from_slice(args);
    p.same_env(&full, label, &envs)
}

#[test]
fn label_reset_and_merge_lines_run_like_git() {
    both_formats(|f| {
        let cases: [(&str, String, i32); 9] = [
            (
                "bare merge",
                format!("{SIDE_SHEET}merge topic # Merge branch 'topic' of here\n"),
                0,
            ),
            (
                "bare merge without a oneline",
                format!("{SIDE_SHEET}merge topic\n"),
                0,
            ),
            (
                "merge -C",
                format!(
                    "{SIDE_SHEET}merge -C $(git rev-parse side~1) topic # x\npick $(git rev-parse side)\n"
                ),
                0,
            ),
            (
                "merge -c",
                format!("{SIDE_SHEET}merge -c $(git rev-parse side~1) topic\n"),
                0,
            ),
            (
                "merge twice",
                format!(
                    "{SIDE_SHEET}merge -C $(git rev-parse side~1) topic\nmerge -C $(git rev-parse side~1) topic\n"
                ),
                0,
            ),
            (
                "unknown label in a merge",
                format!("{SIDE_SHEET}merge -C $(git rev-parse side~1) nolabel\n"),
                1,
            ),
            (
                "unknown label in a reset",
                format!("{SIDE_SHEET}reset nolabel\n"),
                1,
            ),
            (
                "nothing to merge",
                format!("{SIDE_SHEET}merge -C $(git rev-parse side~1)\n"),
                1,
            ),
            (
                "a revision as the label",
                format!("{SIDE_SHEET}merge -C $(git rev-parse side~1) $(git rev-parse topic)\n"),
                0,
            ),
        ];
        for (name, text, status) in cases {
            let p = merge_history(f);
            p.reflog.set(true);
            p.on("side");
            let label = format!("{f} {name}");
            assert_eq!(
                run_text(&p, &text, &["main"], &label, &[]),
                status,
                "{label}"
            );
            if status == 1 {
                assert!(p.pyn.join(".git/rebase-merge").exists(), "{label}");
                assert_eq!(p.same(&["--abort"], &format!("{label} abort")), 0);
            }
        }
    });
}

#[test]
fn an_edited_merge_message_and_an_invalid_label() {
    both_formats(|f| {
        let p = merge_history(f);
        p.reflog.set(true);
        p.on("side");
        let text = format!("{SIDE_SHEET}merge -c $(git rev-parse side~1) topic\n");
        let editor = ("GIT_EDITOR", "sed -i -e '1s/.*/Edited merge/'");
        assert_eq!(
            run_text(&p, &text, &["main"], &format!("{f} edited"), &[editor]),
            0
        );
        let p = merge_history(f);
        p.on("side");
        let text = "label onto\nlabel a..b\n".to_string();
        assert_eq!(
            run_text(&p, &text, &["main"], &format!("{f} bad label"), &[]),
            1
        );
        assert_eq!(p.same(&["--abort"], &format!("{f} abort")), 0);
        let p = merge_history(f);
        p.on("side");
        let text = format!(
            "{SIDE_SHEET}merge -C $(git rev-parse side~1) topic\nbreak\npick $(git rev-parse side)\n"
        );
        assert_eq!(
            run_text(&p, &text, &["main"], &format!("{f} break"), &[]),
            0
        );
        assert_eq!(p.same(&["--continue"], &format!("{f} continue")), 0);
    });
}

#[test]
fn unchanged_merges_fast_forward_and_no_ff_recreates_them() {
    both_formats(|f| {
        for (branch, args) in [
            ("side", vec!["-r", "base"]),
            ("side", vec!["-r", "--no-ff", "base"]),
            ("two", vec!["-r", "base"]),
            ("csid", vec!["-r", "m1"]),
            ("csid", vec!["-r", "--rebase-merges=rebase-cousins", "m1"]),
        ] {
            let p = merge_history(f);
            p.reflog.set(true);
            p.on(branch);
            let label = format!("{f} {branch} {args:?}");
            assert_eq!(p.same(&args, &label), 0, "{label}");
            let p = merge_history(f);
            p.reflog.set(true);
            p.on(branch);
            let label = format!("{f} {branch} -i {args:?}");
            assert_eq!(
                p.same_env(&[&["-i"][..], &args].concat(), &label, &[]),
                0,
                "{label}"
            );
        }
    });
}

#[test]
fn exec_and_break_lines_mix_with_merges() {
    both_formats(|f| {
        for args in [
            vec!["-r", "-x", "true", "main"],
            vec!["-r", "--exec", "false", "main"],
        ] {
            let p = merge_history(f);
            p.reflog.set(true);
            p.on("side");
            let label = format!("{f} {args:?}");
            let status = p.same(&args, &label);
            if status == 1 {
                assert!(p.same(&["--skip"], &format!("{label} skip")) <= 1);
            }
        }
        let p = merge_history(f);
        p.on("side");
        let edit = ("GIT_SEQUENCE_EDITOR", "sed -i -e '/^merge/i break'");
        assert_eq!(
            p.same_env(&["-i", "-r", "main"], &format!("{f} break"), &[edit]),
            0
        );
        let edit_todo = ("GIT_SEQUENCE_EDITOR", "sed -i -e 's/^pick/reword/'");
        let out = p.same_command(
            &["rebase", "--edit-todo"],
            &format!("{f} edit-todo"),
            &[edit_todo],
        );
        assert_eq!(out, 0);
        assert_eq!(p.same(&["--continue"], &format!("{f} continue")), 0);
    });
}

impl Pair {
    /// Branches that point into the commits of `side`: two at `s1`, one at the tip.
    fn with_update_refs_branches(&self) {
        self.both(|env, d| {
            for (name, rev) in [("mid", "side~2"), ("mid2", "side~2"), ("tip", "side")] {
                env.ok(d, &["branch", name, rev]);
            }
        });
    }
}

#[test]
fn update_refs_moves_the_branches_that_point_into_the_replayed_commits() {
    both_formats(|f| {
        type Case<'a> = (&'a str, &'a [&'a str], &'a [(&'a str, &'a str)]);
        let cases: [Case; 6] = [
            ("-r", &["-r", "--update-refs", "main"], &[]),
            ("-i -r", &["-i", "-r", "--update-refs", "main"], &[]),
            ("flat", &["--update-refs", "main"], &[]),
            ("flat -i", &["-i", "--update-refs", "main"], &[]),
            (
                "break",
                &["-i", "-r", "--update-refs", "main"],
                &[("GIT_SEQUENCE_EDITOR", "sed -i -e '/^pick.*s2/i break'")],
            ),
            (
                "exec",
                &["-i", "-r", "--update-refs", "-x", "true", "main"],
                &[],
            ),
        ];
        for (name, args, envs) in cases {
            let p = merge_history(f);
            p.reflog.set(true);
            p.on("side");
            p.with_update_refs_branches();
            let label = format!("{f} {name}");
            assert_eq!(
                p.same_command(&[&["rebase"][..], args].concat(), &label, envs),
                0,
                "{label}"
            );
            if name == "break" {
                assert_eq!(p.same(&["--continue"], &format!("{label} continue")), 0);
            }
            for branch in ["mid", "tip", "topic"] {
                let log = |d: &Path| p.env.ok(d, &["log", "-g", "--format=%gs", branch]);
                assert_eq!(log(&p.pyn), log(&p.git), "{label} {branch}");
            }
        }
        let p = merge_history(f);
        p.on("side");
        p.with_update_refs_branches();
        p.config("rebase.updateRefs", "true");
        assert_eq!(p.same(&["-r", "main"], &format!("{f} configured")), 0);
        let p = merge_history(f);
        p.on("side");
        p.with_update_refs_branches();
        p.config("rebase.updateRefs", "true");
        assert_eq!(
            p.same(
                &["-r", "--no-update-refs", "main"],
                &format!("{f} disabled")
            ),
            0
        );
        let p = merge_history(f);
        p.on("side");
        p.with_update_refs_branches();
        assert_eq!(
            p.same(
                &["-r", "--update-refs", "-q", "main"],
                &format!("{f} quiet")
            ),
            0
        );
    });
}

#[test]
fn update_refs_with_a_stop_and_abort() {
    both_formats(|f| {
        let p = merge_history(f);
        p.on("conf");
        p.both(|env, d| {
            env.ok(d, &["branch", "xone", "conf~2"]);
        });
        assert_eq!(
            p.same(&["-r", "--update-refs", "main"], &format!("{f} stop")),
            1
        );
        assert_eq!(p.same(&["--abort"], &format!("{f} abort")), 0);
        assert_eq!(
            p.same(&["-r", "--update-refs", "main"], &format!("{f} stop again")),
            1
        );
        p.resolve("f", &with_line(2, "R2"));
        assert_eq!(p.same(&["--continue"], &format!("{f} continue")), 0);
    });
}

#[test]
fn root_rebases_every_commit_onto_a_new_root_or_onto_a_base() {
    both_formats(|f| {
        let cases: [(&str, &str, &[&str]); 12] = [
            ("topic", "root", &["--root"]),
            ("topic", "root -i", &["-i", "--root"]),
            ("topic", "root no-ff", &["--root", "--no-ff"]),
            ("topic", "root onto", &["--root", "--onto", "main"]),
            ("topic", "root -i onto", &["-i", "--root", "--onto", "main"]),
            ("side", "root -r", &["-r", "--root"]),
            ("side", "root -r -i", &["-i", "-r", "--root"]),
            ("side", "root -r onto", &["-r", "--root", "--onto", "main"]),
            (
                "csid",
                "root -r cousins",
                &["--rebase-merges=rebase-cousins", "--root"],
            ),
            ("two", "root -r onto", &["-r", "--root", "--onto", "m1"]),
            ("side", "root flat", &["--root"]),
            ("side", "root flat onto", &["--root", "--onto", "main"]),
        ];
        for (branch, name, args) in cases {
            let p = merge_history(f);
            p.reflog.set(true);
            p.on(branch);
            let label = format!("{f} {branch} {name}");
            assert_eq!(p.same(args, &label), 0, "{label}");
        }
        let p = merge_history(f);
        p.reflog.set(true);
        p.on("main");
        assert_eq!(
            p.same(&["--root", "topic"], &format!("{f} branch argument")),
            0
        );
        let p = merge_history(f);
        p.on("topic");
        let brk = ("GIT_SEQUENCE_EDITOR", "sed -i -e '2i break'");
        assert_eq!(
            p.same_env(&["-i", "--root"], &format!("{f} break"), &[brk]),
            0
        );
        assert_eq!(p.same(&["--continue"], &format!("{f} continue")), 0);
        let p = merge_history(f);
        p.on("side");
        let brk = ("GIT_SEQUENCE_EDITOR", "sed -i -e '/^merge/i break'");
        assert_eq!(
            p.same_env(&["-i", "-r", "--root"], &format!("{f} -r break"), &[brk]),
            0
        );
        assert_eq!(p.same(&["--abort"], &format!("{f} abort")), 0);
        let p = merge_history(f);
        p.on("topic");
        assert_eq!(
            p.same(
                &["--root", "--fork-point"],
                &format!("{f} root and fork point")
            ),
            128
        );
        assert_eq!(
            p.same(&["--root", "main", "topic"], &format!("{f} two arguments")),
            129
        );
    });
}

impl Pair {
    /// `up` was at `m1` when `side2` (`k1`, `k2`) was cut from it; then `up` was rewritten to `n1`, `n2` from `base`.
    fn with_rewritten_upstream(&self) {
        let tick = TICK.fetch_add(40, std::sync::atomic::Ordering::SeqCst);
        for dir in [&self.git, &self.pyn] {
            let e = &self.env;
            e.ok(dir, &["branch", "up", "m1"]);
            e.ok(dir, &["checkout", "-q", "-b", "side2", "m1"]);
            write(dir, "k1", "k1\n");
            commit_at(e, dir, "k1", tick);
            write(dir, "k2", "k2\n");
            commit_at(e, dir, "k2", tick + 10);
            e.ok(dir, &["checkout", "-q", "up"]);
            e.ok(dir, &["reset", "-q", "--hard", "base"]);
            write(dir, "n1", "n1\n");
            commit_at(e, dir, "n1", tick + 20);
            write(dir, "n2", "n2\n");
            commit_at(e, dir, "n2", tick + 30);
            e.ok(dir, &["checkout", "-q", "side2"]);
            e.ok(dir, &["config", "branch.side2.remote", "."]);
            e.ok(dir, &["config", "branch.side2.merge", "refs/heads/up"]);
        }
    }
}

#[test]
fn the_fork_point_of_a_rewritten_upstream_limits_the_commits() {
    both_formats(|f| {
        let cases: [(&str, &[&str]); 8] = [
            ("default", &[]),
            ("default -i", &["-i"]),
            ("no fork point", &["--no-fork-point"]),
            ("explicit", &["--fork-point", "up"]),
            ("explicit -i", &["-i", "--fork-point", "up"]),
            ("explicit upstream", &["up"]),
            (
                "fork point and onto",
                &["--fork-point", "--onto", "main", "up"],
            ),
            ("-r", &["-r"]),
        ];
        for (name, args) in cases {
            let p = merge_history(f);
            p.with_rewritten_upstream();
            p.reflog.set(true);
            let label = format!("{f} {name}");
            assert_eq!(p.same(args, &label), 0, "{label}");
        }
        let p = merge_history(f);
        p.with_rewritten_upstream();
        let base = p.env.ok(&p.git, &["rev-parse", "base"]);
        assert_eq!(
            p.same(&["--fork-point", base.trim()], &format!("{f} not a ref")),
            128
        );
        let p = merge_history(f);
        p.with_rewritten_upstream();
        p.both(|env, d| {
            env.ok(d, &["reflog", "expire", "--expire=now", "refs/heads/up"]);
        });
        assert_eq!(p.same(&[], &format!("{f} no reflog")), 0);
    });
}

#[test]
fn the_editor_sees_the_sheet_of_root_and_fork_point_rebases() {
    both_formats(|f| {
        let save = ("GIT_SEQUENCE_EDITOR", "cp \"$1\" .git/seen-sheet #");
        let cases: [(&str, bool, &[&str]); 6] = [
            ("topic", false, &["-i", "--root"]),
            ("topic", false, &["-i", "--root", "--onto", "main"]),
            ("side", false, &["-i", "-r", "--root"]),
            ("side", false, &["-i", "-r", "--root", "--onto", "main"]),
            ("side2", true, &["-i", "-r"]),
            ("side2", true, &["-i", "--rebase-merges=rebase-cousins"]),
        ];
        for (branch, rewritten, args) in cases {
            let p = merge_history(f);
            if rewritten {
                p.with_rewritten_upstream();
            }
            p.on(branch);
            let label = format!("{f} {branch} {args:?}");
            assert_eq!(p.same_env(args, &label, &[save]), 0, "{label}");
            assert_eq!(seen_comments(&p.pyn), seen_comments(&p.git), "{label}");
            assert_eq!(seen_commands(&p.pyn), seen_commands(&p.git), "{label}");
        }
    });
}

#[test]
fn a_bad_mode_is_fatal_and_the_octopus_is_refused() {
    let p = merge_history("sha1");
    p.on("side");
    for dir in [&p.git, &p.pyn] {
        let out = if dir == &p.git {
            p.env
                .git(dir, &["rebase", "--rebase-merges=sideways", "main"])
        } else {
            p.env
                .pyn(dir, &["rebase", "--rebase-merges=sideways", "main"])
        };
        assert_eq!(out.status.code(), Some(128), "{dir:?}");
    }
    // git recreates an octopus merge; pyn stops on it and leaves the sheet for git to continue.
    let p = merge_history("sha1");
    p.both(|env, d| {
        env.ok(d, &["checkout", "-q", "-b", "oct", "base"]);
        write(d, "oc", "oc\n");
        commit_at(env, d, "oc", 1_600_100_000);
        env.ok(d, &["merge", "-q", "--no-ff", "-m", "Octopus", "ta", "tb"]);
    });
    let out = p.env.pyn(&p.pyn, &["rebase", "-r", "main"]);
    assert_eq!(out.status.code(), Some(1));
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(text.contains("octopus"), "{text}");
    assert!(p.pyn.join(".git/rebase-merge").exists());
    let done = read(&p.pyn, "rebase-merge/git-rebase-todo");
    assert!(
        done.lines().next().unwrap().starts_with("merge -C"),
        "{done}"
    );
    let out = p.env.git(&p.pyn, &["rebase", "--continue"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_merged_branch_is_labelled_by_the_local_branch_at_its_tip() {
    both_formats(|f| {
        if git_before_2_55() {
            return;
        }
        let p = merge_history(f);
        p.on("side");
        p.both(|env, d| {
            env.ok(d, &["branch", "zzz", "topic"]);
            env.ok(d, &["branch", "-D", "topic"]);
        });
        let save = ("GIT_SEQUENCE_EDITOR", "cp \"$1\" .git/seen-sheet #");
        assert_eq!(
            p.same_env(&["-i", "-r", "main"], &format!("{f} label"), &[save]),
            0
        );
        assert_eq!(seen_commands(&p.pyn), seen_commands(&p.git));
        assert!(
            seen_commands(&p.pyn)
                .iter()
                .any(|l| l.starts_with("label zzz")),
            "{:?}",
            seen_commands(&p.pyn)
        );
    });
}

impl Pair {
    fn root_dir(&self) -> &Path {
        self.git.parent().unwrap()
    }

    /// `origin` is a bare copy of the repository as it stands, fetched into both; `side`, `csid`, `two` and `conf`
    /// track its `main`.
    fn with_origin(&self) {
        let origin = self.root_dir().join("origin.git");
        self.env.ok(
            self.root_dir(),
            &[
                "clone",
                "-q",
                "--bare",
                self.git.to_str().unwrap(),
                origin.to_str().unwrap(),
            ],
        );
        let shim = self.root_dir().join("ssh-shim");
        fs::write(
            &shim,
            "#!/bin/sh\nwhile [ $# -gt 0 ]; do case \"$1\" in -o|-p|-P) shift 2;; --|-batch) shift;; *) break;; esac; done\ncmd=\"$2\"\nPATH=\"$(git --exec-path):$PATH\"; export PATH\nexec sh -c \"$cmd\"\n",
        )
        .unwrap();
        fs::set_permissions(&shim, fs::Permissions::from_mode(0o755)).unwrap();
        let url = format!("ssh://git@example.invalid{}", origin.display());
        self.both(|env, d| {
            env.ok(d, &["remote", "add", "origin", &url]);
            env.ok(d, &["fetch", "-q", "origin"]);
            env.ok(d, &["remote", "set-head", "origin", "main"]);
            for b in ["side", "csid", "two", "conf"] {
                env.ok(d, &["config", &format!("branch.{b}.remote"), "origin"]);
                env.ok(
                    d,
                    &["config", &format!("branch.{b}.merge"), "refs/heads/main"],
                );
            }
        });
    }

    /// Two commits (`r1`, `r2`) on the origin's `main`, or, with `rewrite`, `main` replaced by `n1`, `n2` from `base`.
    fn advance_origin(&self, rewrite: bool) {
        let origin = self.root_dir().join("origin.git");
        let work = self.root_dir().join("work");
        self.env.ok(
            self.root_dir(),
            &[
                "clone",
                "-q",
                origin.to_str().unwrap(),
                work.to_str().unwrap(),
            ],
        );
        if rewrite {
            self.env
                .ok(&work, &["checkout", "-q", "-B", "main", "base"]);
        }
        let names = if rewrite { ["n1", "n2"] } else { ["r1", "r2"] };
        for name in names {
            let tick = TICK.fetch_add(10, std::sync::atomic::Ordering::SeqCst);
            write(&work, name, &format!("{name}\n"));
            commit_at(&self.env, &work, name, tick);
        }
        self.env
            .ok(&work, &["push", "-q", "--force", "origin", "main"]);
    }

    /// A commit on the origin's `main` that writes `text` over `file`.
    fn origin_commit(&self, file: &str, text: &str, message: &str) {
        let origin = self.root_dir().join("origin.git");
        let work = self.root_dir().join("work2");
        self.env.ok(
            self.root_dir(),
            &[
                "clone",
                "-q",
                origin.to_str().unwrap(),
                work.to_str().unwrap(),
            ],
        );
        let tick = TICK.fetch_add(10, std::sync::atomic::Ordering::SeqCst);
        write(&work, file, text);
        commit_at(&self.env, &work, message, tick);
        self.env.ok(&work, &["push", "-q", "origin", "main"]);
    }

    fn pull(&self, args: &[&str], label: &str) -> i32 {
        self.pull_env(args, label, &[])
    }

    fn pull_env(&self, args: &[&str], label: &str, envs: &[(&str, &str)]) -> i32 {
        let mut full = vec!["pull"];
        full.extend(args);
        self.same_command(&full, label, envs)
    }

    /// `up` is `side2` (`k1`, `k2`) cut from the origin's `main`, which is then rewritten to `n1`, `n2`.
    fn with_rewritten_origin(&self) {
        self.with_origin();
        let tick = TICK.fetch_add(40, std::sync::atomic::Ordering::SeqCst);
        self.both(|e, d| {
            e.ok(d, &["checkout", "-q", "-b", "fp", "origin/main"]);
            write(d, "k1", "k1\n");
            commit_at(e, d, "k1", tick);
            write(d, "k2", "k2\n");
            commit_at(e, d, "k2", tick + 10);
            e.ok(d, &["config", "branch.fp.remote", "origin"]);
            e.ok(d, &["config", "branch.fp.merge", "refs/heads/main"]);
        });
        self.advance_origin(true);
    }
}

type PullCase = (
    &'static str,
    Vec<&'static str>,
    Vec<(&'static str, &'static str)>,
);

fn pull_modes() -> Vec<PullCase> {
    vec![
        ("plain", vec!["--rebase"], vec![]),
        ("interactive", vec!["--rebase=interactive"], vec![]),
        ("i", vec!["--rebase=i"], vec![]),
        ("merges", vec!["--rebase=merges"], vec![]),
        ("m", vec!["--rebase=m"], vec![]),
        ("-r", vec!["-r"], vec![]),
        ("-rmerges", vec!["-rmerges"], vec![]),
        ("-ri", vec!["-ri"], vec![]),
        ("quiet merges", vec!["--rebase=merges", "-q"], vec![]),
        (
            "pull.rebase merges",
            vec![],
            vec![("pull.rebase", "merges")],
        ),
        ("pull.rebase m", vec![], vec![("pull.rebase", "m")]),
        (
            "pull.rebase interactive",
            vec![],
            vec![("pull.rebase", "interactive")],
        ),
        ("pull.rebase i", vec![], vec![("pull.rebase", "i")]),
        ("pull.rebase yes", vec![], vec![("pull.rebase", "yes")]),
        (
            "branch beats pull",
            vec![],
            vec![("pull.rebase", "false"), ("branch.side.rebase", "merges")],
        ),
        (
            "option beats config",
            vec!["--rebase=merges"],
            vec![("pull.rebase", "interactive")],
        ),
        (
            "plain with rebase.rebaseMerges",
            vec!["--rebase"],
            vec![("rebase.rebaseMerges", "true")],
        ),
        (
            "interactive with rebase.rebaseMerges",
            vec!["--rebase=interactive"],
            vec![("rebase.rebaseMerges", "true")],
        ),
        (
            "plain with rebase.updateRefs",
            vec!["--rebase"],
            vec![("rebase.updateRefs", "true")],
        ),
        (
            "explicit remote and branch",
            vec!["--rebase=merges", "origin", "main"],
            vec![],
        ),
        (
            "remote only",
            vec!["--rebase=interactive", "origin"],
            vec![],
        ),
    ]
}

#[test]
fn pull_rebase_modes_replay_like_git() {
    both_formats(|f| {
        for (name, args, config) in pull_modes() {
            let all = ["plain", "interactive", "merges"].contains(&name);
            for branch in ["side", "csid", "two"].iter().take(if all { 3 } else { 1 }) {
                let p = merge_history(f);
                p.with_origin();
                p.advance_origin(false);
                p.on(branch);
                p.reflog.set(true);
                for (k, v) in &config {
                    p.config(k, v);
                }
                let label = format!("{f} {name} {branch}");
                assert_eq!(p.pull(&args, &label), 0, "{label}");
                assert!(!p.pyn.join(".git/rebase-merge").exists());
            }
        }
    });
}

#[test]
fn pull_rebase_with_nothing_to_replay_or_nothing_new() {
    both_formats(|f| {
        for mode in ["--rebase", "--rebase=merges", "--rebase=interactive"] {
            // up to date with local commits, and behind with none
            let p = merge_history(f);
            p.with_origin();
            p.on("side");
            p.reflog.set(true);
            assert_eq!(p.pull(&[mode], &format!("{f} {mode} up to date")), 0);
            let p = merge_history(f);
            p.with_origin();
            p.advance_origin(false);
            p.on("main");
            p.both(|env, d| {
                env.ok(d, &["config", "branch.main.remote", "origin"]);
                env.ok(d, &["config", "branch.main.merge", "refs/heads/main"]);
            });
            p.reflog.set(true);
            assert_eq!(p.pull(&[mode], &format!("{f} {mode} fast-forward")), 0);
        }
    });
}

#[test]
fn an_interactive_pull_that_is_up_to_date_still_opens_the_editor() {
    both_formats(|f| {
        let p = merge_history(f);
        p.with_origin();
        p.on("side");
        let save = ("GIT_SEQUENCE_EDITOR", "cp \"$1\" .git/seen-sheet #");
        assert_eq!(
            p.pull_env(
                &["--rebase=interactive"],
                &format!("{f} up to date"),
                &[save]
            ),
            0
        );
        assert_eq!(
            p.pyn.join(".git/seen-sheet").exists(),
            p.git.join(".git/seen-sheet").exists()
        );
        assert_eq!(seen_commands(&p.pyn), seen_commands(&p.git), "{f}");
        let envs = [("GIT_SEQUENCE_EDITOR", "sed -i -e '1s/^pick/reword/;2d'")];
        let p = merge_history(f);
        p.with_origin();
        p.on("side");
        p.reflog.set(true);
        assert_eq!(
            p.pull_env(
                &["--rebase=interactive"],
                &format!("{f} up to date edited"),
                &envs
            ),
            0
        );
    });
}

#[test]
fn the_editor_of_a_rebasing_pull_sees_the_sheet_and_can_change_it() {
    both_formats(|f| {
        let p = merge_history(f);
        p.with_origin();
        p.advance_origin(false);
        p.on("side");
        let save = ("GIT_SEQUENCE_EDITOR", "cp \"$1\" .git/seen-sheet #");
        assert_eq!(
            p.pull_env(&["--rebase=interactive"], &format!("{f} sheet"), &[save]),
            0
        );
        assert_eq!(seen_comments(&p.pyn), seen_comments(&p.git), "{f}");
        assert_eq!(seen_commands(&p.pyn), seen_commands(&p.git), "{f}");
        assert!(seen_comments(&p.pyn).len() > 10);

        let p = merge_history(f);
        p.with_origin();
        p.advance_origin(false);
        p.on("side");
        let save = ("GIT_SEQUENCE_EDITOR", "cp \"$1\" .git/seen-sheet #");
        assert_eq!(
            p.pull_env(
                &["--rebase=merges"],
                &format!("{f} merges ran no editor"),
                &[save]
            ),
            0
        );
        assert!(!p.pyn.join(".git/seen-sheet").exists());
        assert!(!p.git.join(".git/seen-sheet").exists());

        let p = merge_history(f);
        p.with_origin();
        p.advance_origin(false);
        p.on("side");
        p.reflog.set(true);
        let envs = [("GIT_SEQUENCE_EDITOR", "sed -i -e '1s/^pick/reword/;2d'")];
        assert_eq!(
            p.pull_env(&["--rebase=interactive"], &format!("{f} edited"), &envs),
            0
        );
    });
}

#[test]
fn a_stopped_rebasing_pull_goes_on_with_rebase_continue_skip_and_abort() {
    both_formats(|f| {
        for mode in ["--rebase=interactive", "--rebase=merges", "--rebase"] {
            for finish in ["continue", "skip", "abort", "quit"] {
                let p = merge_history(f);
                p.with_origin();
                p.origin_commit("f", &with_line(2, "R"), "origin changes f");
                p.on("conf");
                let label = format!("{f} {mode} {finish}");
                assert_eq!(p.pull(&[mode], &label), 1, "{label}: stops");
                assert!(p.pyn.join(".git/rebase-merge").is_dir(), "{label}");
                match finish {
                    "continue" | "skip" => {
                        for _ in 0..6 {
                            if finish == "continue" {
                                p.resolve("f", &with_line(2, "RX"));
                            }
                            let word = format!("--{finish}");
                            let code = p.same_command(&["rebase", &word], &label, &[]);
                            if code == 0 {
                                break;
                            }
                        }
                    }
                    _ => {
                        let word = format!("--{finish}");
                        assert_eq!(p.same_command(&["rebase", &word], &label, &[]), 0);
                    }
                }
                assert!(!p.pyn.join(".git/rebase-merge").exists(), "{label}");
            }
        }
    });
}

#[test]
fn the_fork_point_of_a_rewritten_origin_limits_the_commits_of_a_pull() {
    both_formats(|f| {
        let cases: [(&str, &[&str]); 8] = [
            ("plain", &["--rebase"]),
            ("merges", &["--rebase=merges"]),
            ("interactive", &["--rebase=interactive"]),
            ("remote and branch", &["--rebase", "origin", "main"]),
            ("remote only", &["--rebase", "origin"]),
            ("heads/ spelling", &["--rebase", "origin", "heads/main"]),
            ("full ref", &["--rebase", "origin", "refs/heads/main"]),
            ("other branch", &["--rebase", "origin", "side"]),
        ];
        for (name, args) in cases {
            let p = merge_history(f);
            p.with_rewritten_origin();
            p.reflog.set(true);
            let label = format!("{f} {name}");
            let code = p.pull(args, &label);
            if name == "other branch" {
                assert!(code == 0 || code == 1, "{label}: {code}");
            } else {
                assert_eq!(code, 0, "{label}");
                let log = p.env.ok(&p.pyn, &["log", "--format=%s", "-4"]);
                assert_eq!(
                    log, "k2\nk1\nn2\nn1\n",
                    "{label}: only the commits after the fork point"
                );
            }
        }
        let p = merge_history(f);
        p.with_rewritten_origin();
        let url2 = format!(
            "ssh://git@example.invalid{}",
            p.root_dir().join("origin.git").display()
        );
        p.both(|env, d| {
            env.ok(d, &["remote", "add", "second", &url2]);
            env.ok(d, &["fetch", "-q", "second"]);
        });
        p.reflog.set(true);
        let code = p.pull(
            &["--rebase", "second", "main"],
            &format!("{f} second remote"),
        );
        assert!(code == 0 || code == 1);
    });
}

#[test]
fn bad_rebase_values_are_refused_before_anything_changes() {
    both_formats(|f| {
        for args in [
            &["--rebase=bogus"][..],
            &["--rebase=preserve"],
            &["--rebase=p"],
            &["--rebase=Merges"],
        ] {
            let p = merge_history(f);
            p.with_origin();
            p.advance_origin(false);
            p.on("side");
            assert_eq!(p.pull(args, &format!("{f} {args:?}")), 129);
        }
        for value in ["bogus", "preserve", "Merges", "Interactive"] {
            for key in ["pull.rebase", "branch.side.rebase"] {
                let p = merge_history(f);
                p.with_origin();
                p.advance_origin(false);
                p.on("side");
                p.config(key, value);
                let label = format!("{f} {key}={value}");
                assert_eq!(p.pull(&[], &label), 128, "{label}");
                assert_eq!(p.pull(&["--no-rebase", "--no-edit"], &label), 0, "{label}");
            }
        }
    });
}

#[test]
fn strategies_with_a_rebasing_pull_are_a_usage_error() {
    // git hands -s and -X to its rebase; pyn's rebase has no strategies yet.
    let p = merge_history("sha1");
    p.with_origin();
    p.advance_origin(false);
    p.on("side");
    for args in [
        &["--rebase", "-s", "ours"][..],
        &["--rebase=merges", "-X", "theirs"],
        &["--rebase=interactive", "-s", "ort"],
    ] {
        let out = p.env.pyn(&p.pyn, &[&["pull"], args].concat());
        assert_eq!(out.status.code(), Some(129), "{args:?}");
        assert!(!p.pyn.join(".git/rebase-merge").exists());
    }
}

#[test]
fn a_dirty_tree_refuses_a_rebasing_pull_unless_autostash() {
    both_formats(|f| {
        for mode in ["--rebase=interactive", "--rebase=merges"] {
            let p = merge_history(f);
            p.with_origin();
            p.advance_origin(false);
            p.on("side");
            p.both(|_, d| write(d, "f", "dirty\n"));
            assert_eq!(p.pull(&[mode], &format!("{f} {mode} dirty")), 128);
            assert_eq!(
                p.pull(&[mode, "--autostash"], &format!("{f} {mode} autostash")),
                0
            );
        }
    });
}

#[test]
fn skipping_a_pick_before_a_merge_that_then_applies_cleanly_leaves_no_merge_message() {
    both_formats(|f| {
        let p = merge_history(f);
        p.on("main");
        p.commit("f", &with_line(2, "R"), "mr");
        p.on("conf");
        assert_eq!(p.same(&["-r", "main"], &format!("{f} start")), 1);
        for _ in 0..4 {
            if p.same(&["--skip"], &format!("{f} skip")) == 0 {
                break;
            }
        }
    });
}
