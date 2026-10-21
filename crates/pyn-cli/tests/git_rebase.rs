//! `pyn rebase` on a git repository, compared with the real `git rebase` on a copy of the same repository: the exit
//! status, the commits made (same ids), the refs, the index, the work tree, the stash and the `rebase-merge` state
//! (never the messages or the reflogs). Needs `git` on the PATH (tests only).

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

/// The todo list with every id cut to seven digits (git abbreviates them, pyn writes them whole).
fn sheet(dir: &Path, name: &str) -> String {
    read(dir, &format!("rebase-merge/{name}"))
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(|l| {
            let mut w = l.splitn(3, ' ');
            let (cmd, id, rest) = (w.next(), w.next().unwrap_or(""), w.next().unwrap_or(""));
            // git 2.55 writes `pick <id> # <subject>`.
            let rest = rest.strip_prefix("# ").unwrap_or(rest);
            format!("{} {} {rest}", cmd.unwrap_or(""), &id[..id.len().min(7)])
        })
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
    out
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
        _ => "work tree".into(),
    }
}

struct Pair {
    _tmp: tempfile::TempDir,
    env: Env,
    git: PathBuf,
    pyn: PathBuf,
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
            if a != b {
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
    for flag in ["--root", "--rebase-merges", "--apply", "--keep-base"] {
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
