//! Git LFS pointer files on a git home: clone, fetch, switch and pull keep them byte for byte and warn once per
//! invocation when `filter=lfs` applies. Real git only builds the remote; wording is not compared.

#[path = "../../pyn-gittransport/tests/common/gitsrv.rs"]
mod gitsrv;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use gitsrv::*;
use tempfile::{TempDir, tempdir};

const ATTRS: &str = "*.bin filter=lfs diff=lfs merge=lfs -text\n";

fn pointer(seed: char, size: u32) -> Vec<u8> {
    let oid = seed.to_string().repeat(64);
    format!("version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize {size}\n")
        .into_bytes()
}

struct Rig {
    _tmp: TempDir,
    root: PathBuf,
    bare: PathBuf,
    work: PathBuf,
    home: PathBuf,
    shim: PathBuf,
    url: String,
}

impl Rig {
    fn new(format: &str, attrs: &str) -> Self {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let work = root.join("seed");
        init(&work, format, false);
        commit(&work, "a.txt", b"plain\n", "plain");
        commit(&work, ".gitattributes", attrs.as_bytes(), "attributes");
        fs::create_dir_all(work.join("sub")).unwrap();
        commit(&work, "big.bin", &pointer('a', 1000), "pointer");
        commit(
            &work,
            "sub/other.bin",
            &pointer('b', 2000),
            "nested pointer",
        );
        let bare = root.join("served.git");
        init(&bare, format, true);
        run(&work, &["push", "-q", bare.to_str().unwrap(), "main"]);
        run(&bare, &["symbolic-ref", "HEAD", "refs/heads/main"]);
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        Rig {
            shim: ssh_shim(&root, false),
            url: format!("ssh://git@example.invalid{}", bare.display()),
            _tmp: tmp,
            root,
            bare,
            work,
            home,
        }
    }

    fn pyn(&self, cwd: &Path, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_pyn"))
            .current_dir(cwd)
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
            .unwrap()
    }

    fn pyn_ok(&self, cwd: &Path, args: &[&str]) -> String {
        let out = self.pyn(cwd, args);
        let err = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(out.status.success(), "{args:?}: {err}");
        err
    }

    fn clone(&self) -> (PathBuf, String) {
        let err = self.pyn_ok(&self.root, &["clone", &self.url, "clone"]);
        (self.root.join("clone"), err)
    }

    /// Pushes a branch whose tip changes both pointers.
    fn push_change(&self, branch: &str) {
        let w = &self.work;
        run(w, &["switch", "-q", "-c", branch]);
        fs::write(w.join("big.bin"), pointer('c', 1)).unwrap();
        fs::write(w.join("sub/other.bin"), pointer('d', 2)).unwrap();
        run(w, &["commit", "-q", "-a", "-m", "move pointers"]);
        run(w, &["push", "-q", self.bare.to_str().unwrap(), branch]);
        run(w, &["switch", "-q", "main"]);
    }
}

fn warnings(stderr: &str) -> usize {
    stderr.lines().filter(|l| l.contains("Git LFS")).count()
}

#[test]
fn clone_keeps_pointers_and_warns_once() {
    for format in FORMATS {
        let rig = Rig::new(format, ATTRS);
        let (dir, err) = rig.clone();
        assert_eq!(warnings(&err), 1, "{err}");
        assert_eq!(fs::read(dir.join("big.bin")).unwrap(), pointer('a', 1000));
        assert_eq!(
            fs::read(dir.join("sub/other.bin")).unwrap(),
            pointer('b', 2000)
        );
        assert_eq!(run(&dir, &["status", "--porcelain"]), "");
    }
}

#[test]
fn clone_without_lfs_attributes_does_not_warn() {
    let rig = Rig::new("sha1", "*.bin -text\n");
    let (_, err) = rig.clone();
    assert_eq!(warnings(&err), 0, "{err}");
}

#[test]
fn clone_that_checks_nothing_out_does_not_warn() {
    let rig = Rig::new("sha1", ATTRS);
    let err = rig.pyn_ok(&rig.root, &["clone", "--bare", &rig.url, "bare.git"]);
    assert_eq!(warnings(&err), 0, "{err}");
}

#[test]
fn fetch_warns_once_and_stores_pointers_unchanged() {
    for format in FORMATS {
        let rig = Rig::new(format, ATTRS);
        let (dir, _) = rig.clone();
        rig.push_change("moved");
        let err = rig.pyn_ok(&dir, &["fetch"]);
        assert_eq!(warnings(&err), 1, "{err}");
        let blob = run(&dir, &["cat-file", "blob", "origin/moved:big.bin"]);
        assert_eq!(blob.as_bytes(), pointer('c', 1).as_slice());
        let err = rig.pyn_ok(&dir, &["fetch"]);
        assert_eq!(warnings(&err), 0, "nothing new: {err}");
    }
}

#[test]
fn switch_warns_once_and_writes_pointers_unchanged() {
    let rig = Rig::new("sha1", ATTRS);
    let (dir, _) = rig.clone();
    rig.push_change("moved");
    rig.pyn_ok(&dir, &["fetch"]);
    let err = rig.pyn_ok(&dir, &["switch", "-c", "moved", "origin/moved"]);
    assert_eq!(warnings(&err), 1, "{err}");
    assert_eq!(fs::read(dir.join("big.bin")).unwrap(), pointer('c', 1));
    assert_eq!(
        fs::read(dir.join("sub/other.bin")).unwrap(),
        pointer('d', 2)
    );
    let err = rig.pyn_ok(&dir, &["checkout", "main"]);
    assert_eq!(warnings(&err), 1, "{err}");
    assert_eq!(fs::read(dir.join("big.bin")).unwrap(), pointer('a', 1000));
}

#[test]
fn pull_warns_once_for_the_fetch_and_the_merge() {
    let rig = Rig::new("sha1", ATTRS);
    let (dir, _) = rig.clone();
    let w = &rig.work;
    fs::write(w.join("big.bin"), pointer('e', 5)).unwrap();
    run(w, &["commit", "-q", "-a", "-m", "move"]);
    run(w, &["push", "-q", rig.bare.to_str().unwrap(), "main"]);
    let err = rig.pyn_ok(&dir, &["pull"]);
    assert_eq!(warnings(&err), 1, "{err}");
    assert_eq!(fs::read(dir.join("big.bin")).unwrap(), pointer('e', 5));
}
