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
        Command::new(env!("CARGO_BIN_EXE_pyn"))
            .args(args)
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join("xdg"))
            .env("TZ", "UTC")
            .envs(IDENTITY)
            .env("PYN_CONFIG_DIR", self.home.join("pyn-config"))
            .env_remove("PYN_SERVER")
            .env_remove("PYN_REPO")
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
        .map(|l| {
            let mut w = l.splitn(3, ' ');
            let (cmd, id, rest) = (w.next(), w.next().unwrap_or(""), w.next().unwrap_or(""));
            format!("{} {} {rest}", cmd.unwrap_or(""), &id[..id.len().min(7)])
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// What a rebase may change: refs, pseudo-refs, the state folder, the index, the work tree and the stash.
fn state(env: &Env, dir: &Path) -> Vec<String> {
    let mut files = BTreeMap::new();
    files_under(dir, dir, &mut files);
    let mut out = vec![
        read(dir, "HEAD"),
        env.ok(dir, &["for-each-ref"]),
        read(dir, "ORIG_HEAD"),
        read(dir, "REBASE_HEAD"),
        read(dir, "MERGE_HEAD"),
        read(dir, "MERGE_MSG"),
        format!("{}", dir.join(".git/rebase-merge").is_dir()),
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
        out.push(read(dir, &format!("rebase-merge/{name}")));
    }
    out.push(sheet(dir, "git-rebase-todo"));
    out.push(sheet(dir, "done"));
    out.push(env.ok(dir, &["ls-files", "-s"]));
    out.push(
        env.ok(dir, &["stash", "list", "--format=%H %s"])
            .replace(' ', "_"),
    );
    out.push(format!("{files:?}"));
    out
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
        let mut full = vec!["rebase"];
        full.extend(args);
        let g = self.env.git(&self.git, &full);
        let p = self.env.pyn(&self.pyn, &full);
        let what = format!("{label}: rebase {args:?}");
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
                let line = a.lines().zip(b.lines()).find(|(x, y)| x != y);
                panic!(
                    "{what}: state part {i} differs (pyn, then git): first differing lines {line:?}; pyn said {}",
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
    env.ok(dir, &["add", "-A"]);
    let tick = TICK.fetch_add(10, std::sync::atomic::Ordering::SeqCst);
    let out = Command::new("git")
        .args(["commit", "-q", "--allow-empty", "-m", message])
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
fn interactive_and_the_other_unbuilt_options_are_usage_errors() {
    let p = history("sha1");
    p.on("clean");
    for flag in ["-i", "--root", "--rebase-merges", "--autosquash", "--apply"] {
        let out = p.env.pyn(&p.pyn, &["rebase", flag, "main"]);
        assert_eq!(out.status.code(), Some(129), "{flag}");
    }
}
