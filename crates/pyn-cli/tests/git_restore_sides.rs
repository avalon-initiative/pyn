//! `pyn restore --ours`, `--theirs` and `--merge` on a git repository, compared with the real `git restore` on a copy
//! of the same conflicted work tree: exit status, messages, work tree and index. Needs `git` on the PATH (tests only).

mod common;

use std::collections::BTreeMap;
use std::fs;
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

struct Env {
    home: PathBuf,
}

impl Env {
    fn git(&self, cwd: &Path, args: &[&str]) -> Output {
        Command::new("git")
            .args(["-c", "gc.auto=0", "-c", "merge.conflictStyle=diff3"])
            .args(args)
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join("xdg"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .envs(IDENTITY)
            .output()
            .expect("git is installed")
    }

    fn ok(&self, cwd: &Path, args: &[&str]) {
        let out = self.git(cwd, args);
        assert!(
            out.status.success() || args[0] == "merge",
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
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

fn write(dir: &Path, name: &str, bytes: &[u8]) {
    let file = dir.join(name);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    let _ = fs::remove_file(&file);
    fs::write(file, bytes).unwrap();
}

fn link(dir: &Path, name: &str, target: &str) {
    let file = dir.join(name);
    let _ = fs::remove_file(&file);
    std::os::unix::fs::symlink(target, file).unwrap();
}

/// Every path in the work tree outside `.git` with its content (or link target) and whether it is executable.
fn tree(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
    use std::os::unix::fs::PermissionsExt;
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
            out.insert(rel.clone(), "dir".into());
            tree(root, &path, out);
        } else {
            let exec = meta.permissions().mode() & 0o111 != 0;
            out.insert(rel, format!("{exec} {:?}", fs::read(&path).unwrap()));
        }
    }
}

struct Pair {
    _tmp: tempfile::TempDir,
    env: Env,
    git: PathBuf,
    pyn: PathBuf,
}

/// A merge of `side` into `main` that conflicts in every way a path can: content, modify/delete both ways, add/add,
/// a link, a binary file and a file whose mode differs; `clean.txt` merges cleanly.
fn conflicted(format: &str) -> Pair {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let env = Env {
        home: root.join("home"),
    };
    fs::create_dir_all(&env.home).unwrap();
    let git = root.join("git");
    fs::create_dir_all(&git).unwrap();
    env.ok(
        &git,
        &[
            "init",
            "-q",
            "-b",
            "main",
            &format!("--object-format={format}"),
        ],
    );
    write(&git, "c.txt", b"a\nb\nc\n");
    write(&git, "md.txt", b"x\n");
    write(&git, "dm.txt", b"y\n");
    write(&git, "clean.txt", b"z\n");
    write(&git, "d/e.txt", b"1\n2\n3\n");
    write(&git, "bin", b"a\0b\n");
    write(&git, "ex.sh", b"#!/bin/sh\n1\n");
    link(&git, "l", "tgt");
    env.ok(&git, &["add", "-A"]);
    env.ok(&git, &["commit", "-q", "-m", "base"]);
    env.ok(&git, &["checkout", "-q", "-b", "side"]);
    write(&git, "c.txt", b"a\nB-theirs\nc\n");
    fs::remove_file(git.join("md.txt")).unwrap();
    write(&git, "dm.txt", b"y2\n");
    write(&git, "aa.txt", b"new-t\n");
    write(&git, "d/e.txt", b"1\nT\n3\n");
    write(&git, "bin", b"a\0c\n");
    write(&git, "ex.sh", b"#!/bin/sh\nT\n");
    link(&git, "l", "one");
    env.ok(&git, &["update-index", "--chmod=+x", "ex.sh"]);
    env.ok(&git, &["add", "-A"]);
    env.ok(&git, &["commit", "-q", "-m", "side"]);
    env.ok(&git, &["checkout", "-q", "main"]);
    write(&git, "c.txt", b"a\nB-ours\nc\n");
    write(&git, "md.txt", b"x2\n");
    fs::remove_file(git.join("dm.txt")).unwrap();
    write(&git, "aa.txt", b"new-o\n");
    write(&git, "d/e.txt", b"1\nO\n3\n");
    write(&git, "bin", b"a\0d\n");
    write(&git, "ex.sh", b"#!/bin/sh\nO\n");
    link(&git, "l", "two");
    write(&git, "clean.txt", b"z\nmain\n");
    env.ok(&git, &["add", "-A"]);
    env.ok(&git, &["commit", "-q", "-m", "main"]);
    env.ok(&git, &["merge", "side"]);
    let pyn = root.join("pyn");
    copy_tree(&git, &pyn);
    Pair {
        _tmp: tmp,
        env,
        git,
        pyn,
    }
}

impl Pair {
    /// The index of one side as `ls-files -s`, read by git on both.
    fn index(&self, dir: &Path) -> String {
        String::from_utf8_lossy(&self.env.git(dir, &["ls-files", "-s"]).stdout).into_owned()
    }

    /// Runs `restore <args>` in both from `sub`; status, messages, work tree and index must agree.
    fn same(&self, sub: &str, args: &[&str], label: &str) {
        self.same_command("restore", sub, args, label);
    }

    fn same_command(&self, command: &str, sub: &str, args: &[&str], label: &str) {
        let mut full = vec![command];
        full.extend(args);
        let g = self.env.git(&self.git.join(sub), &full);
        let p = self.env.pyn(&self.pyn.join(sub), &full);
        let what = format!("{label}: {args:?} in '{sub}'");
        // checkout's progress line is not what is compared here
        let text = |o: &Output| {
            String::from_utf8_lossy(&o.stderr)
                .lines()
                .filter(|l| !l.starts_with("Updated "))
                .map(|l| format!("{l}\n"))
                .collect::<String>()
        };
        let (ge, pe) = (text(&g), text(&p));
        assert_eq!(
            p.status.code(),
            g.status.code(),
            "{what}: exit; pyn: {pe}; git: {ge}"
        );
        assert_eq!(pe, ge, "{what}: stderr (pyn, then git)");
        assert_eq!(p.stdout, g.stdout, "{what}: stdout");
        let (mut gt, mut pt) = (BTreeMap::new(), BTreeMap::new());
        tree(&self.git, &self.git, &mut gt);
        tree(&self.pyn, &self.pyn, &mut pt);
        assert_eq!(pt, gt, "{what}: work tree (pyn, then git)");
        assert_eq!(
            self.index(&self.pyn),
            self.index(&self.git),
            "{what}: index (pyn, then git)"
        );
    }

    fn both(&self, args: &[&str]) {
        self.env.ok(&self.git, args);
        self.env.ok(&self.pyn, args);
    }
}

const FORMATS: [&str; 2] = ["sha1", "sha256"];

#[test]
fn sides_and_merges_match_git() {
    for format in FORMATS {
        for args in [
            &["--ours", "c.txt"][..],
            &["--theirs", "c.txt"],
            &["--ours", "--theirs", "c.txt"],
            &["--theirs", "--ours", "c.txt"],
            &["--ours", "."],
            &["--theirs", "."],
            &["--ours", "d"],
            &["--theirs", "d/e.txt", "aa.txt"],
            &["--ours", "md.txt", "dm.txt"],
            &["--theirs", "md.txt", "dm.txt"],
            &["--ours", "l", "bin", "ex.sh"],
            &["--theirs", "l", "bin", "ex.sh"],
            &["--ours", "clean.txt"],
            &["--theirs", "--worktree", "c.txt"],
            &["--merge", "c.txt"],
            &["--merge", "aa.txt"],
            &["--merge", "d"],
            &["--merge", "l", "bin", "ex.sh"],
            &["--merge", "clean.txt"],
            &["--merge", "md.txt"],
            &["--merge", "dm.txt", "aa.txt"],
            &["--merge", "."],
            &["--merge", "--worktree", "c.txt"],
            &["--ours", "nope", "c.txt"],
            &["--merge", "nope"],
            &["c.txt"],
        ] {
            conflicted(format).same("", args, format);
        }
    }
}

#[test]
fn local_edits_and_resolved_paths_are_overwritten_as_git_does() {
    for format in FORMATS {
        let pair = conflicted(format);
        write(&pair.git, "clean.txt", b"dirty\n");
        write(&pair.pyn, "clean.txt", b"dirty\n");
        pair.same("", &["--ours", "clean.txt"], format);
        let pair = conflicted(format);
        write(&pair.git, "clean.txt", b"dirty\n");
        write(&pair.pyn, "clean.txt", b"dirty\n");
        pair.same("", &["--merge", "clean.txt"], format);
        let pair = conflicted(format);
        write(&pair.git, "c.txt", b"hand\n");
        write(&pair.pyn, "c.txt", b"hand\n");
        pair.both(&["add", "c.txt"]);
        pair.same("", &["--ours", "c.txt"], format);
    }
}

#[test]
fn flag_combinations_match_git() {
    for args in [
        &["--staged", "--ours", "c.txt"][..],
        &["--staged", "--worktree", "--theirs", "c.txt"],
        &["--staged", "--merge", "c.txt"],
        &["--staged", "--ours", "--merge", "c.txt"],
        &["--source=HEAD", "--ours", "c.txt"],
        &["--source=HEAD", "--merge", "c.txt"],
        &["-s", "HEAD", "--ours", "--merge", "c.txt"],
        &["--staged", "-s", "HEAD", "--theirs", "c.txt"],
        &["--ours", "--merge", "c.txt"],
        &["--theirs", "--merge", "c.txt"],
    ] {
        conflicted("sha1").same("", args, "flags");
    }
}

#[test]
fn paths_are_relative_to_the_folder_you_stand_in() {
    for args in [
        &["--ours", "e.txt"][..],
        &["--theirs", "../c.txt"],
        &["--merge", "e.txt", "../c.txt"],
        &["--merge", "."],
        &["--ours", ".."],
    ] {
        conflicted("sha1").same("d", args, "subfolder");
    }
}

#[test]
fn merge_of_a_resolved_path_restores_the_index() {
    let pair = conflicted("sha1");
    write(&pair.pyn, "c.txt", b"hand\n");
    pair.env.ok(&pair.pyn, &["add", "c.txt"]);
    write(&pair.pyn, "c.txt", b"edited\n");
    let out = pair.env.pyn(&pair.pyn, &["restore", "--merge", "c.txt"]);
    assert!(out.status.success());
    assert_eq!(fs::read(pair.pyn.join("c.txt")).unwrap(), b"hand\n");
}

#[test]
fn checkout_names_the_side_an_unmerged_path_lacks() {
    for args in [
        &["--theirs", "--", "md.txt", "c.txt"][..],
        &["--ours", "--", "dm.txt", "aa.txt"],
        &["--ours", "--", "md.txt"],
        &["--theirs", "--", "dm.txt"],
        &["--ours", "--", "clean.txt"],
        &["--theirs", "--", "."],
    ] {
        conflicted("sha1").same_command("checkout", "", args, "checkout");
    }
}
