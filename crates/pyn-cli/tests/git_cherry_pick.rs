//! `pyn cherry-pick` on a git repository, compared with the real `git cherry-pick` on a copy of the same repository:
//! the exit status, the commits made (same ids), the refs, the index, the work tree, the state files in `.git` and the
//! reflogs (never the messages). Needs `git` on the PATH (tests only).

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

fn logs(dir: &Path) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        let Ok(rd) = fs::read_dir(dir) else { return };
        let mut paths: Vec<PathBuf> = rd.map(|e| e.unwrap().path()).collect();
        paths.sort();
        for p in paths {
            if p.is_dir() {
                walk(root, &p, out);
            } else {
                let name = p.strip_prefix(root).unwrap().display().to_string();
                out.push(format!("{name}\n{}", fs::read_to_string(&p).unwrap()));
            }
        }
    }
    let mut out = Vec::new();
    let root = dir.join(".git/logs");
    walk(&root, &root, &mut out);
    out
}

/// The todo list with every id cut to seven digits (git abbreviates them, pyn writes them whole).
fn todo(dir: &Path) -> String {
    read(dir, "sequencer/todo")
        .lines()
        .map(|l| {
            let mut w = l.splitn(3, ' ');
            let (cmd, id, rest) = (w.next(), w.next().unwrap_or(""), w.next().unwrap_or(""));
            format!("{} {} {rest}", cmd.unwrap_or(""), &id[..id.len().min(7)])
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The options git and pyn agree on (git adds an empty `gpg-sign`).
fn opts(dir: &Path) -> String {
    read(dir, "sequencer/opts")
        .lines()
        .filter(|l| !l.contains("gpg-sign"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// What a cherry-pick may change: refs, pseudo-refs, state files, the index, the work tree and the reflogs.
fn state(env: &Env, dir: &Path) -> Vec<String> {
    let mut files = BTreeMap::new();
    files_under(dir, dir, &mut files);
    let mut out = vec![
        read(dir, "HEAD"),
        env.ok(dir, &["for-each-ref"]),
        read(dir, "ORIG_HEAD"),
        read(dir, "CHERRY_PICK_HEAD"),
        read(dir, "MERGE_HEAD"),
        read(dir, "MERGE_MSG"),
        read(dir, "sequencer/head"),
        read(dir, "sequencer/abort-safety"),
        todo(dir),
        opts(dir),
        env.ok(dir, &["ls-files", "-s"]),
        format!("{files:?}"),
    ];
    out.extend(logs(dir));
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

    /// Runs `cherry-pick <args>` in both; the exit status and every piece of state must agree. Returns the
    /// status.
    fn same(&self, args: &[&str], label: &str) -> i32 {
        let mut full = vec!["cherry-pick"];
        full.extend(args);
        let g = self.env.git(&self.git, &full);
        let p = self.env.pyn(&self.pyn, &full);
        let what = format!("{label}: cherry-pick {args:?}");
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
        assert_eq!(ps.len(), gs.len(), "{what}: number of reflogs");
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

fn commit_all(env: &Env, dir: &Path, message: &str) {
    env.ok(dir, &["add", "-A"]);
    env.ok(dir, &["commit", "-q", "--allow-empty", "-m", message]);
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

/// `side` (tags `s1` to `s4`, then the merge `mc` of `topic` `t1`) and `main` (`m1` to `m3`) from one `base`.
/// `s1` and `s3` apply cleanly to `main`, `s2` conflicts in `g`, `s4` in the binary `bin`.
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
    env.ok(&git, &["checkout", "-q", "-b", "topic"]);
    write(&git, "k", "topic\n");
    commit_all(&env, &git, "t1");
    env.ok(&git, &["tag", "t1"]);
    env.ok(&git, &["checkout", "-q", "-b", "side", "base"]);
    write(&git, "f", &with_line(2, "S1"));
    commit_all(
        &env,
        &git,
        "s1 first line\n\nbody of s1\n\nReviewed-by: Q <q@example.com>",
    );
    env.ok(&git, &["tag", "s1"]);
    write(&git, "h", "h\n");
    write(&git, "g", "gS\n");
    commit_all(&env, &git, "s2 adds h and changes g");
    env.ok(&git, &["tag", "s2"]);
    write(&git, "f", &with_line(3, "S3"));
    commit_all(&env, &git, "s3");
    env.ok(&git, &["tag", "s3"]);
    write_bytes(&git, "bin", &[0, 9, 9]);
    set_exec(&git, "x", false);
    commit_all(&env, &git, "s4 binary and mode");
    env.ok(&git, &["tag", "s4"]);
    env.ok(&git, &["merge", "-q", "--no-ff", "-m", "mc", "topic"]);
    env.ok(&git, &["tag", "mc"]);
    env.ok(&git, &["checkout", "-q", "main"]);
    env.ok(&git, &["reset", "-q", "--hard", "base"]);
    write(&git, "f", &with_line(5, "M5"));
    commit_all(&env, &git, "m1");
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
fn a_clean_pick_makes_a_commit_with_the_same_id() {
    both_formats(|f| {
        let p = history(f);
        assert_eq!(p.same(&["s1"], &format!("{f} clean")), 0);
        assert_eq!(p.same(&["s3"], &format!("{f} second")), 0);
        let (g, y) = (
            p.env.ok(&p.git, &["rev-parse", "HEAD"]),
            p.env.ok(&p.pyn, &["rev-parse", "HEAD"]),
        );
        assert_eq!(g, y);
    });
}

#[test]
fn record_origin_and_signoff_extend_the_message() {
    both_formats(|f| {
        for args in [
            &["-x", "s1"][..],
            &["-x", "s3"],
            &["-s", "s1"],
            &["-x", "-s", "s1"],
            &["-xs", "s2~0"],
        ] {
            let p = history(f);
            p.same(args, &format!("{f} {args:?}"));
        }
    });
}

#[test]
fn no_commit_leaves_the_changes_staged() {
    both_formats(|f| {
        let p = history(f);
        p.same(&["-n", "s1"], &format!("{f} -n"));
        p.same(&["-n", "s3"], &format!("{f} -n again"));
        let p = history(f);
        p.same(&["-n", "s2"], &format!("{f} -n conflict"));
        let p = history(f);
        p.same(&["-n", "s1", "s3"], &format!("{f} -n two"));
    });
}

#[test]
fn staged_changes_block_a_commit_but_not_no_commit() {
    both_formats(|f| {
        let p = history(f);
        p.both(|env, d| {
            write(d, "new", "staged\n");
            env.ok(d, &["add", "new"]);
        });
        assert_ne!(p.same(&["s1"], &format!("{f} staged")), 0);
        assert_eq!(p.same(&["-n", "s1"], &format!("{f} staged -n")), 0);
    });
}

#[test]
fn local_edits_block_only_the_files_the_pick_touches() {
    both_formats(|f| {
        let p = history(f);
        p.both(|_, d| write(d, "x", "edited\n"));
        assert_eq!(p.same(&["s1"], &format!("{f} unrelated edit")), 0);
        p.both(|_, d| write(d, "f", "edited\n"));
        assert_ne!(p.same(&["s3"], &format!("{f} touched edit")), 0);
        let p = history(f);
        p.both(|_, d| write(d, "h", "untracked\n"));
        assert_ne!(p.same(&["s2"], &format!("{f} untracked in the way")), 0);
    });
}

#[test]
fn a_conflict_stops_and_continue_commits_the_resolution() {
    both_formats(|f| {
        let p = history(f);
        assert_eq!(p.same(&["s2"], &format!("{f} conflict")), 1);
        assert_ne!(p.same(&["--continue"], &format!("{f} unresolved")), 0);
        assert_ne!(p.same(&["s1"], &format!("{f} while stopped")), 0);
        p.resolve("g", "gM+gS\n");
        assert_eq!(p.same(&["--continue"], &format!("{f} resolved")), 0);
        assert_ne!(p.same(&["--continue"], &format!("{f} nothing left")), 0);
    });
}

#[test]
fn a_range_stops_at_the_conflict_and_goes_on_after_it() {
    both_formats(|f| {
        let p = history(f);
        assert_eq!(p.same(&["base..s3"], &format!("{f} range")), 1);
        p.resolve("g", "gM+gS\n");
        assert_eq!(p.same(&["--continue"], &format!("{f} continue")), 0);
        let p = history(f);
        assert_eq!(p.same(&["s1", "s2", "s3"], &format!("{f} list")), 1);
        p.resolve("g", "resolved\n");
        assert_eq!(p.same(&["--continue"], &format!("{f} continue list")), 0);
    });
}

#[test]
fn skip_drops_the_stopped_commit_and_goes_on() {
    both_formats(|f| {
        let p = history(f);
        assert_eq!(p.same(&["base..s3"], &format!("{f} range")), 1);
        assert_eq!(p.same(&["--skip"], &format!("{f} skip")), 0);
        let p = history(f);
        assert_eq!(p.same(&["s2"], &format!("{f} single")), 1);
        assert_eq!(p.same(&["--skip"], &format!("{f} skip single")), 0);
        assert_ne!(p.same(&["--skip"], &format!("{f} skip none")), 0);
    });
}

#[test]
fn abort_puts_everything_back() {
    both_formats(|f| {
        let p = history(f);
        assert_eq!(p.same(&["s1", "s2"], &format!("{f} stopped")), 1);
        assert_eq!(p.same(&["--abort"], &format!("{f} abort")), 0);
        assert_ne!(p.same(&["--abort"], &format!("{f} abort none")), 0);
        let p = history(f);
        assert_eq!(p.same(&["s2"], &format!("{f} single")), 1);
        assert_eq!(p.same(&["--abort"], &format!("{f} abort single")), 0);
    });
}

#[test]
fn abort_keeps_a_head_that_moved() {
    both_formats(|f| {
        let p = history(f);
        assert_eq!(p.same(&["s1", "s2", "s3"], &format!("{f} stopped")), 1);
        p.resolve("g", "resolved\n");
        p.both(|env, d| {
            env.ok(d, &["commit", "-q", "-m", "by hand"]);
        });
        assert_eq!(p.same(&["--abort"], &format!("{f} moved")), 0);
    });
}

#[test]
fn quit_forgets_the_operation_and_keeps_the_files() {
    both_formats(|f| {
        let p = history(f);
        assert_eq!(p.same(&["--quit"], &format!("{f} quit none")), 0);
        assert_eq!(p.same(&["s1", "s2"], &format!("{f} stopped")), 1);
        assert_eq!(p.same(&["--quit"], &format!("{f} quit")), 0);
    });
}

#[test]
fn a_commit_that_changes_nothing_here_stops_as_empty() {
    both_formats(|f| {
        let p = history(f);
        assert_eq!(p.same(&["s1"], &format!("{f} first")), 0);
        assert_ne!(p.same(&["s1"], &format!("{f} again")), 0);
        assert_eq!(p.same(&["--skip"], &format!("{f} skip empty")), 0);
    });
}

#[test]
fn merge_commits_need_a_parent_number() {
    both_formats(|f| {
        for args in [
            &["mc"][..],
            &["-m", "1", "mc"],
            &["-m", "2", "mc"],
            &["-m", "3", "mc"],
            &["-m", "1", "s1"],
            &["-m", "0", "mc"],
        ] {
            let p = history(f);
            p.same(args, &format!("{f} {args:?}"));
        }
    });
}

#[test]
fn conflicts_of_every_kind() {
    both_formats(|f| {
        let p = history(f);
        assert_eq!(p.same(&["s4"], &format!("{f} binary and mode")), 1);
        let p = history(f);
        p.both(|env, d| {
            env.ok(d, &["rm", "-q", "f"]);
            env.ok(d, &["commit", "-q", "-m", "drop f"]);
        });
        assert_eq!(p.same(&["s1"], &format!("{f} modify/delete")), 1);
        let p = history(f);
        p.both(|env, d| {
            write(d, "h", "other h\n");
            commit_all(env, d, "own h");
        });
        assert_eq!(p.same(&["s2"], &format!("{f} add/add")), 1);
    });
}

#[test]
fn a_root_commit_and_an_unborn_branch() {
    both_formats(|f| {
        let p = history(f);
        assert_eq!(p.same(&["base~0"], &format!("{f} root already there")), 1);
        let (tmp, env, git) = Pair::root();
        init(&env, &git, f);
        env.ok(&git, &["checkout", "-q", "-b", "src"]);
        write(&git, "a", "a\n");
        commit_all(&env, &git, "root");
        write(&git, "b", "b\n");
        commit_all(&env, &git, "second");
        env.ok(&git, &["checkout", "-q", "--orphan", "fresh"]);
        env.ok(&git, &["rm", "-rfq", "."]);
        let p = Pair::copied(tmp, env, git);
        assert_eq!(p.same(&["src~1"], &format!("{f} onto unborn")), 0);
    });
}

#[test]
fn a_detached_head() {
    both_formats(|f| {
        let p = history(f);
        p.both(|env, d| {
            env.ok(d, &["checkout", "-q", "--detach", "main~2"]);
        });
        assert_eq!(p.same(&["s1"], &format!("{f} detached")), 0);
    });
}

#[test]
fn refusals_leave_nothing_behind() {
    both_formats(|f| {
        for args in [
            &["nosuch"][..],
            &["s1..nosuch"],
            &["s1...s3"],
            &["--continue"],
            &["--abort"],
            &["--skip"],
        ] {
            let p = history(f);
            assert_ne!(p.same(args, &format!("{f} {args:?}")), 0);
        }
        let p = history(f);
        p.same(&["s2"], &format!("{f} stopped"));
        assert_ne!(p.same(&["s1"], &format!("{f} second")), 0);
        let p = history(f);
        p.both(|env, d| {
            let _ = env.git(d, &["merge", "-q", "--no-commit", "--no-ff", "topic"]);
        });
        assert_ne!(p.same(&["s1"], &format!("{f} merge in progress")), 0);
    });
}

#[test]
fn a_range_of_the_commits_after_a_commit() {
    both_formats(|f| {
        for args in [&["s1..s3"][..], &["base..s1"], &["s3..s3"]] {
            let p = history(f);
            p.same(args, &format!("{f} {args:?}"));
        }
    });
}

#[test]
fn git_continues_what_pyn_stopped() {
    both_formats(|f| {
        let p = history(f);
        let stopped = p.env.pyn(&p.pyn, &["cherry-pick", "base..s3"]);
        assert_eq!(stopped.status.code(), Some(1));
        write(&p.pyn, "g", "resolved\n");
        p.env.ok(&p.pyn, &["add", "g"]);
        p.env.ok(&p.pyn, &["cherry-pick", "--continue"]);
        assert!(!p.pyn.join(".git/sequencer").exists());
        assert_eq!(
            p.env.ok(&p.pyn, &["log", "--format=%s", "-4"]),
            "s3\ns2 adds h and changes g\ns1 first line\nm3\n"
        );
    });
}

#[test]
fn pyn_continues_what_git_stopped() {
    both_formats(|f| {
        let p = history(f);
        let g = p.env.git(&p.git, &["cherry-pick", "base..s3"]);
        assert_eq!(g.status.code(), Some(1));
        write(&p.git, "g", "resolved\n");
        p.env.ok(&p.git, &["add", "g"]);
        let out = p.env.pyn(&p.git, &["cherry-pick", "--continue"]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!p.git.join(".git/sequencer").exists());
        assert_eq!(
            p.env.ok(&p.git, &["log", "--format=%s", "-4"]),
            "s3\ns2 adds h and changes g\ns1 first line\nm3\n"
        );
    });
}

#[test]
fn no_commit_with_several_commits_keeps_the_sequence() {
    both_formats(|f| {
        let p = history(f);
        assert_eq!(
            p.same(&["-n", "s1", "s2", "s3"], &format!("{f} -n list")),
            1
        );
        p.resolve("g", "resolved\n");
        p.same(&["--continue"], &format!("{f} -n continue"));
        let p = history(f);
        assert_eq!(
            p.same(&["-n", "s1", "s3"], &format!("{f} -n clean list")),
            0
        );
        let p = history(f);
        assert_eq!(p.same(&["-n", "s1", "s2"], &format!("{f} -n stop")), 1);
        assert_eq!(p.same(&["--abort"], &format!("{f} -n abort")), 0);
    });
}
