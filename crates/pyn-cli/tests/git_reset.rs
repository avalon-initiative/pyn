//! `pyn reset` on a git repository, compared with the real `git reset` on a copy of the same repository: the exit
//! status and the refs, `ORIG_HEAD`, merge state, index, work tree and reflogs left behind (never the messages).
//! Needs `git` on the PATH (tests only).

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
    ("GIT_COMMITTER_NAME", "T"),
    ("GIT_COMMITTER_EMAIL", "t@example.com"),
    ("GIT_AUTHOR_DATE", "1700000000 +0000"),
    ("GIT_COMMITTER_DATE", "1700000000 +0000"),
];

const MODES: [&str; 5] = ["--soft", "--mixed", "--hard", "--keep", "--merge"];

struct Env {
    home: PathBuf,
}

impl Env {
    fn git(&self, cwd: &Path, args: &[&str]) -> Output {
        Command::new("git")
            .args(["-c", "gc.auto=0", "-c", "maintenance.auto=false"])
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

fn set_exec(dir: &Path, name: &str) {
    let file = dir.join(name);
    let mut perms = fs::metadata(&file).unwrap().permissions();
    perms.set_mode(0o755);
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
        if meta.file_type().is_symlink() {
            out.insert(
                rel,
                format!("-> {}", fs::read_link(&path).unwrap().display()),
            );
        } else if meta.is_dir() {
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
    let mut out = Vec::new();
    let root = dir.join(".git/logs");
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
    walk(&root, &root, &mut out);
    out
}

/// What a reset may change: refs, pseudo-refs, merge state, the index, the work tree and the reflogs.
fn state(env: &Env, dir: &Path) -> Vec<String> {
    let mut files = BTreeMap::new();
    files_under(dir, dir, &mut files);
    let mut out = vec![
        read(dir, "HEAD"),
        env.git(dir, &["for-each-ref"])
            .stdout
            .iter()
            .map(|b| *b as char)
            .collect(),
        read(dir, "ORIG_HEAD"),
        read(dir, "MERGE_HEAD"),
        read(dir, "MERGE_MSG"),
        read(dir, "MERGE_MODE"),
        read(dir, "SQUASH_MSG"),
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

    /// Copies the finished `git` repository to the directory `pyn` works in.
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

    /// Runs `reset <args>` in both from `sub` (a folder under the root); the exit status and every piece of
    /// state must agree.
    fn same_in(&self, sub: &str, args: &[&str], label: &str) {
        let mut full = vec!["reset"];
        full.extend(args);
        let g = self.env.git(&self.git.join(sub), &full);
        let p = self.env.pyn(&self.pyn.join(sub), &full);
        let what = format!("{label}: reset {args:?}");
        assert_eq!(
            p.status.code(),
            g.status.code(),
            "{what}: exit status; pyn said {}; git said {}",
            String::from_utf8_lossy(&p.stderr),
            String::from_utf8_lossy(&g.stderr)
        );
        let (gs, ps) = (state(&self.env, &self.git), state(&self.env, &self.pyn));
        for (i, (a, b)) in ps.iter().zip(&gs).enumerate() {
            assert_eq!(a, b, "{what}: state part {i} differs (pyn, then git)");
        }
        assert_eq!(ps.len(), gs.len(), "{what}: number of reflogs");
        let fsck = self.env.git(&self.pyn, &["fsck", "--strict"]);
        assert!(
            fsck.status.success(),
            "{what}: fsck: {}",
            String::from_utf8_lossy(&fsck.stderr)
        );
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

/// A repository with three commits touching `a`, `dir/b`, an executable and a link.
fn history(format: &str) -> Pair {
    let (tmp, env, git) = Pair::root();
    init(&env, &git, format);
    write(&git, "a", "one\n");
    write(&git, "dir/b", "b1\n");
    write(&git, "run.sh", "#!/bin/sh\n");
    set_exec(&git, "run.sh");
    commit_all(&env, &git, "first");
    write(&git, "a", "two\n");
    write(&git, "dir/c", "c2\n");
    std::os::unix::fs::symlink("a", git.join("link")).unwrap();
    commit_all(&env, &git, "second");
    write(&git, "a", "three\n");
    write(&git, "dir/b", "b3\n");
    fs::remove_file(git.join("run.sh")).unwrap();
    write(&git, "run.sh", "plain\n");
    commit_all(&env, &git, "third");
    env.ok(&git, &["branch", "side", "HEAD~1"]);
    Pair::copied(tmp, env, git)
}

/// `history` with changes in the index and work tree on top of it.
fn dirty_history(format: &str, extra: impl Fn(&Env, &Path)) -> Pair {
    let p = history(format);
    let (env, g) = (&p.env, &p.git);
    write(g, "a", "staged\n");
    env.ok(g, &["add", "a"]);
    write(g, "dir/b", "unstaged\n");
    write(g, "untracked", "u\n");
    write(g, "new", "n\n");
    env.ok(g, &["add", "new"]);
    extra(env, g);
    let (tmp, env, git) = (p._tmp, p.env, p.git);
    Pair::copied(tmp, env, git)
}

#[test]
fn every_mode_from_a_clean_and_a_dirty_tree() {
    for format in ["sha1", "sha256"] {
        for target in ["HEAD", "HEAD~1", "HEAD~2", "side", "main"] {
            for mode in MODES {
                let clean = history(format);
                clean.same_in("", &[mode, target], &format!("{format} clean"));
                let dirty = dirty_history(format, |_, _| {});
                dirty.same_in("", &[mode, target], &format!("{format} dirty"));
            }
        }
    }
}

#[test]
fn default_mode_and_flag_forms() {
    for format in ["sha1", "sha256"] {
        for args in [
            &[][..],
            &["HEAD~1"],
            &["-q"],
            &["-q", "HEAD~1"],
            &["--quiet", "--hard", "HEAD~1"],
            &["--soft", "--mixed", "HEAD~1"],
            &["--mixed", "--hard", "HEAD~1"],
            &["--hard", "--soft", "HEAD~1"],
            &["HEAD~1", "--hard"],
            &["--hard", "HEAD~9"],
            &["nonexistent"],
            &["--hard", "side^"],
            &["-q", "--soft", "HEAD^"],
        ] {
            history(format).same_in("", args, &format!("{format} clean"));
            dirty_history(format, |_, _| {}).same_in("", args, &format!("{format} dirty"));
        }
    }
}

#[test]
fn the_reflog_and_orig_head_after_two_resets() {
    let p = history("sha1");
    for args in [
        &["--hard", "HEAD~1"][..],
        &["--soft", "ORIG_HEAD"],
        &["HEAD"],
        &["--mixed", "HEAD~2"],
    ] {
        p.same_in("", args, "chain");
    }
}

#[test]
fn paths_reset_only_the_index() {
    for format in ["sha1", "sha256"] {
        for args in [
            &["--", "a"][..],
            &["a"],
            &["-q", "a"],
            &["HEAD~1", "--", "a"],
            &["HEAD~2", "--", "dir"],
            &["HEAD~2", "--", "."],
            &["--", "."],
            &["--", "new", "untracked", "missing"],
            &["HEAD~1", "run.sh", "link"],
            &["HEAD~2", "--", "link", "run.sh"],
            &["side", "--", "dir/c"],
            &["--", "nonexistent"],
            &["HEAD", "nonexistent"],
            &["a", "dir"],
            &["--mixed", "--", "a"],
            &["--hard", "--", "a"],
            &["--soft", "--", "a"],
            &["--keep", "--", "a"],
            &["--merge", "--", "a"],
            &["--hard", "HEAD", "a"],
            &["nonexistent", "--", "a"],
            &["HEAD", "HEAD~1", "--", "a"],
            &["HEAD", "HEAD~1", "a"],
            &["HEAD", "--"],
            &["--"],
        ] {
            dirty_history(format, |_, _| {}).same_in("", args, &format!("{format} paths"));
        }
    }
}

#[test]
fn paths_from_a_subfolder() {
    let make = || dirty_history("sha1", |_, _| {});
    for args in [
        &["--", "b"][..],
        &["b"],
        &["HEAD~2", "--", "c", "b"],
        &["--", "../a"],
        &["--", "."],
        &["HEAD~1", "--", ".."],
        &["--", "../new"],
    ] {
        make().same_in("dir", args, "subfolder");
    }
}

#[test]
fn a_path_named_like_a_revision_is_ambiguous() {
    let p = dirty_history("sha1", |env, g| {
        write(g, "side", "file\n");
        env.ok(g, &["add", "side"]);
    });
    p.same_in("", &["side"], "ambiguous");
    p.same_in("", &["side", "--"], "ambiguous with separator");
    p.same_in("", &["--", "side"], "path with separator");
}

#[test]
fn an_unborn_branch() {
    for format in ["sha1", "sha256"] {
        for args in [
            &[][..],
            &["--soft"],
            &["--mixed"],
            &["--hard"],
            &["--keep"],
            &["--merge"],
            &["-q"],
            &["--", "a"],
            &["a"],
            &["--hard", "--", "a"],
            &["HEAD"],
            &["--hard", "HEAD"],
            &["main"],
            &["--", "missing"],
        ] {
            let (tmp, env, git) = Pair::root();
            init(&env, &git, format);
            write(&git, "a", "a\n");
            write(&git, "d/b", "b\n");
            write(&git, "loose", "l\n");
            env.ok(&git, &["add", "a", "d"]);
            Pair::copied(tmp, env, git).same_in("", args, &format!("{format} unborn"));
        }
    }
}

#[test]
fn a_detached_head() {
    for format in ["sha1", "sha256"] {
        for args in [
            &["HEAD~1"][..],
            &["--hard", "HEAD~1"],
            &["--soft", "side"],
            &["--keep", "HEAD~2"],
            &["--merge", "HEAD~2"],
            &["--hard", "main"],
            &["--", "a"],
            &["--hard"],
        ] {
            let p = dirty_history(format, |env, g| {
                env.ok(g, &["checkout", "-q", "--detach", "HEAD"]);
            });
            p.same_in("", args, &format!("{format} detached"));
        }
    }
}

/// `main` and `side` change `f` differently, so merging `side` into `main` stops with `f` unmerged.
fn conflicted(format: &str) -> Pair {
    let (tmp, env, git) = Pair::root();
    init(&env, &git, format);
    write(&git, "f", "a\nb\nc\n");
    write(&git, "g", "g0\n");
    write(&git, "h", "h0\n");
    commit_all(&env, &git, "base");
    env.ok(&git, &["checkout", "-q", "-b", "side"]);
    write(&git, "f", "a\nS\nc\n");
    write(&git, "g", "gS\n");
    write(&git, "n", "side only\n");
    commit_all(&env, &git, "side");
    env.ok(&git, &["checkout", "-q", "main"]);
    write(&git, "f", "a\nM\nc\n");
    commit_all(&env, &git, "main");
    let merged = env.git(&git, &["merge", "side"]);
    assert!(!merged.status.success());
    write(&git, "h", "h unstaged\n");
    Pair::copied(tmp, env, git)
}

#[test]
fn a_merge_in_progress() {
    for format in ["sha1", "sha256"] {
        for args in [
            &["--soft"][..],
            &["--mixed"],
            &["--hard"],
            &["--keep"],
            &["--merge"],
            &["--merge", "HEAD~1"],
            &["--hard", "side"],
            &["--keep", "HEAD"],
            &["--mixed", "side"],
            &["--", "f"],
            &["f"],
            &["HEAD~1", "--", "g"],
            &["-q"],
        ] {
            conflicted(format).same_in("", args, &format!("{format} merge"));
        }
    }
}

#[test]
fn a_merge_that_staged_files_cleanly_then_stopped() {
    for mode in ["--merge", "--keep", "--hard", "--mixed"] {
        let p = conflicted("sha1");
        write(&p.git, "n", "changed after the merge\n");
        let (tmp, env, git) = (p._tmp, p.env, p.git);
        Pair::copied(tmp, env, git).same_in("", &[mode, "HEAD"], "merge with n changed");
    }
}

#[test]
fn local_changes_that_keep_and_merge_must_not_lose() {
    for format in ["sha1", "sha256"] {
        for mode in ["--keep", "--merge", "--hard"] {
            for (path_change, target) in [
                ("unstaged-same-path", "HEAD~1"),
                ("unstaged-other-path", "HEAD~1"),
                ("staged-same-path", "HEAD~1"),
                ("staged-other-path", "HEAD~1"),
                ("untracked-in-the-way", "HEAD~2"),
                ("deleted-in-tree", "HEAD~1"),
                ("untracked-dir-in-the-way", "HEAD~2"),
                ("exec-bit", "HEAD~1"),
                ("link-replaced", "HEAD~1"),
            ] {
                let p = history(format);
                let g = &p.git;
                match path_change {
                    "unstaged-same-path" => write(g, "a", "mine\n"),
                    "unstaged-other-path" => write(g, "dir/b", "mine\n"),
                    "staged-same-path" => {
                        write(g, "a", "mine\n");
                        p.env.ok(g, &["add", "a"]);
                    }
                    "staged-other-path" => {
                        write(g, "dir/b", "mine\n");
                        p.env.ok(g, &["add", "dir/b"]);
                    }
                    "untracked-in-the-way" => write(g, "dir/c", "mine\n"),
                    "untracked-dir-in-the-way" => {}
                    "deleted-in-tree" => fs::remove_file(g.join("a")).unwrap(),
                    "exec-bit" => set_exec(g, "run.sh"),
                    "link-replaced" => {
                        fs::remove_file(g.join("link")).unwrap();
                        write(g, "link", "now a file\n");
                    }
                    _ => unreachable!(),
                }
                if path_change == "untracked-dir-in-the-way" {
                    p.env.ok(g, &["reset", "-q", "--hard", "HEAD~1"]);
                    p.env.ok(g, &["rm", "-q", "link"]);
                    p.env.ok(g, &["commit", "-q", "-m", "drop the link"]);
                    write(g, "link/x", "mine\n");
                }
                let (tmp, env, git) = (p._tmp, p.env, p.git);
                Pair::copied(tmp, env, git).same_in(
                    "",
                    &[mode, target],
                    &format!("{format} {path_change}"),
                );
            }
        }
    }
}

/// Every state of one file in `HEAD`, the target, the index and the work tree (`-` is absent).
fn matrix(format: &str, values: &[&str], modes: &[&str]) {
    for head in values {
        for target in values {
            let (tmp, env, git) = Pair::root();
            init(&env, &git, format);
            write(&git, "base", "base\n");
            commit_all(&env, &git, "base");
            if *head != "-" {
                write(&git, "f", &format!("{head}\n"));
            }
            commit_all(&env, &git, "head");
            env.ok(&git, &["checkout", "-q", "-b", "target"]);
            if *target == "-" {
                if *head != "-" {
                    fs::remove_file(git.join("f")).unwrap();
                }
            } else {
                write(&git, "f", &format!("{target}\n"));
            }
            commit_all(&env, &git, "target");
            env.ok(&git, &["checkout", "-q", "main"]);
            let base = Pair::copied(tmp, env, git);
            for index in values {
                for work in values {
                    let case = format!("{format} H={head} T={target} I={index} W={work}");
                    for mode in modes {
                        let tmp = tempfile::tempdir().unwrap();
                        let root = tmp.path().canonicalize().unwrap();
                        let env = Env {
                            home: root.join("home"),
                        };
                        fs::create_dir_all(&env.home).unwrap();
                        copy_tree(&base.git, &root.join("git"));
                        let git = root.join("git");
                        let _ = env.git(&git, &["rm", "-q", "--cached", "f"]);
                        if *index != "-" {
                            write(&git, "f", &format!("{index}\n"));
                            env.ok(&git, &["add", "f"]);
                        }
                        if *work == "-" {
                            let _ = fs::remove_file(git.join("f"));
                        } else {
                            write(&git, "f", &format!("{work}\n"));
                        }
                        Pair::copied(tmp, env, git).same_in("", &[mode, "target"], &case);
                    }
                }
            }
        }
    }
}

#[test]
fn file_states_in_head_target_index_and_work_tree_sha1() {
    matrix("sha1", &["-", "A", "B"], &MODES);
}

#[test]
fn file_states_in_head_target_index_and_work_tree_sha256() {
    matrix(
        "sha256",
        &["-", "A"],
        &["--keep", "--merge", "--hard", "--mixed"],
    );
}
