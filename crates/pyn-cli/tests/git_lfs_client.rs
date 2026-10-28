//! pyn's own `filter=lfs` on a git repository: clean writes pointer files and the local object store, smudge reads the
//! store back and keeps the pointer (with a warning) when an object is absent. Real git only reads the repository back;
//! `git lfs` is used as an oracle for the pointer bytes when it is installed, and those checks are skipped otherwise.

#[path = "../../pyn-gittransport/tests/common/gitsrv.rs"]
mod gitsrv;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use gitsrv::*;
use tempfile::{TempDir, tempdir};

const ATTRS: &[u8] = b"*.bin filter=lfs diff=lfs merge=lfs -text\n";
const HELLO_OID: &str = "5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03";

struct Repo {
    _tmp: TempDir,
    dir: PathBuf,
    home: PathBuf,
}

impl Repo {
    fn new(format: &str) -> Self {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let dir = root.join("w");
        init(&dir, format, false);
        run(&dir, &["config", "user.name", "t"]);
        run(&dir, &["config", "user.email", "t@example.com"]);
        fs::write(dir.join(".gitattributes"), ATTRS).unwrap();
        Repo {
            home: root.join("home"),
            _tmp: tmp,
            dir,
        }
    }

    fn write(&self, name: &str, bytes: &[u8]) {
        fs::write(self.dir.join(name), bytes).unwrap();
    }

    fn read(&self, name: &str) -> Vec<u8> {
        fs::read(self.dir.join(name)).unwrap()
    }

    fn pyn_env(&self, args: &[&str], env: &[(&str, &str)]) -> (i32, String, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_pyn"))
            .current_dir(&self.dir)
            .args(args)
            .env_clear()
            .env("PATH", path_env())
            .env("HOME", &self.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_EDITOR", "true")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .envs(env.iter().copied())
            .output()
            .unwrap();
        (
            out.status.code().unwrap(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn pyn(&self, args: &[&str]) -> (i32, String, String) {
        self.pyn_env(args, &[])
    }

    fn ok(&self, args: &[&str]) -> String {
        let (code, _, err) = self.pyn(args);
        assert_eq!(code, 0, "{args:?}: {err}");
        err
    }

    fn staged(&self, name: &str) -> Vec<u8> {
        git(&self.dir)
            .args(["cat-file", "blob", &format!(":{name}")])
            .output()
            .unwrap()
            .stdout
    }

    fn object(&self, oid: &str) -> PathBuf {
        self.dir
            .join(".git/lfs/objects")
            .join(&oid[..2])
            .join(&oid[2..4])
            .join(oid)
    }

    fn status(&self) -> String {
        self.pyn(&["status", "--porcelain"]).1
    }
}

fn pointer(oid: &str, size: usize) -> Vec<u8> {
    format!("version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize {size}\n")
        .into_bytes()
}

fn lfs_warnings(stderr: &str) -> usize {
    stderr.lines().filter(|l| l.contains("Git LFS")).count()
}

#[test]
fn add_stores_a_pointer_and_the_object() {
    for format in FORMATS {
        let r = Repo::new(format);
        r.write("big.bin", b"hello\n");
        r.ok(&["add", "-A"]);
        assert_eq!(r.staged("big.bin"), pointer(HELLO_OID, 6));
        assert_eq!(fs::read(r.object(HELLO_OID)).unwrap(), b"hello\n");
        assert_eq!(r.read("big.bin"), b"hello\n");
        assert_eq!(r.status(), "A  .gitattributes\nA  big.bin\n");
    }
}

#[test]
fn checkout_restores_content_from_the_store() {
    let r = Repo::new("sha1");
    r.write("big.bin", b"hello\n");
    r.ok(&["add", "-A"]);
    r.ok(&["commit", "-q", "-m", "one"]);
    fs::remove_file(r.dir.join("big.bin")).unwrap();
    assert_eq!(r.status(), " D big.bin\n");
    let err = r.ok(&["restore", "big.bin"]);
    assert_eq!(r.read("big.bin"), b"hello\n");
    assert_eq!(lfs_warnings(&err), 0, "{err}");
    assert_eq!(r.status(), "");

    r.write("big.bin", b"changed\n");
    assert_eq!(r.status(), " M big.bin\n");
    r.ok(&["commit", "-q", "-a", "-m", "two"]);
    r.ok(&["reset", "-q", "--hard", "HEAD~1"]);
    assert_eq!(r.read("big.bin"), b"hello\n");
    r.ok(&["reset", "-q", "--hard", "HEAD@{1}"]);
    assert_eq!(r.read("big.bin"), b"changed\n");
    assert_eq!(r.status(), "");
}

#[test]
fn missing_object_keeps_the_pointer_and_warns_once() {
    let r = Repo::new("sha1");
    r.write("a.bin", b"hello\n");
    r.write("b.bin", b"world\n");
    r.ok(&["add", "-A"]);
    r.ok(&["commit", "-q", "-m", "one"]);
    fs::remove_dir_all(r.dir.join(".git/lfs")).unwrap();
    fs::remove_file(r.dir.join("a.bin")).unwrap();
    fs::remove_file(r.dir.join("b.bin")).unwrap();
    let err = r.ok(&["restore", "."]);
    assert_eq!(lfs_warnings(&err), 1, "{err}");
    assert_eq!(r.read("a.bin"), pointer(HELLO_OID, 6));
    assert_eq!(r.status(), "");
}

#[test]
fn skip_smudge_leaves_pointers() {
    let r = Repo::new("sha1");
    r.write("big.bin", b"hello\n");
    r.ok(&["add", "-A"]);
    r.ok(&["commit", "-q", "-m", "one"]);
    fs::remove_file(r.dir.join("big.bin")).unwrap();
    let (code, _, err) = r.pyn_env(&["restore", "big.bin"], &[("GIT_LFS_SKIP_SMUDGE", "1")]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(r.read("big.bin"), pointer(HELLO_OID, 6));
}

#[test]
fn empty_files_pointers_and_duplicates() {
    let r = Repo::new("sha1");
    r.write("empty.bin", b"");
    r.write("ptr.bin", &pointer(&"a".repeat(64), 10));
    r.write("one.bin", &vec![7u8; 3 << 20]);
    r.write("two.bin", &vec![7u8; 3 << 20]);
    r.ok(&["add", "-A"]);
    assert_eq!(r.staged("empty.bin"), b"");
    assert_eq!(r.staged("ptr.bin"), pointer(&"a".repeat(64), 10));
    assert_eq!(r.staged("one.bin"), r.staged("two.bin"));
    let stored = fs::read_dir(r.dir.join(".git/lfs/objects/"))
        .unwrap()
        .count();
    assert_eq!(stored, 1);
}

#[test]
fn configured_lfs_commands_are_not_used() {
    let r = Repo::new("sha1");
    run(&r.dir, &["config", "filter.lfs.clean", "false"]);
    run(&r.dir, &["config", "filter.lfs.smudge", "false"]);
    run(&r.dir, &["config", "filter.lfs.required", "true"]);
    r.write("big.bin", b"hello\n");
    r.ok(&["add", "-A"]);
    assert_eq!(r.staged("big.bin"), pointer(HELLO_OID, 6));
    r.ok(&["commit", "-q", "-m", "one"]);
    fs::remove_file(r.dir.join("big.bin")).unwrap();
    r.ok(&["restore", "big.bin"]);
    assert_eq!(r.read("big.bin"), b"hello\n");
}

#[test]
fn switching_branches_swaps_content_and_status_stays_clean() {
    let r = Repo::new("sha1");
    r.write("big.bin", b"first\n");
    r.ok(&["add", "-A"]);
    r.ok(&["commit", "-q", "-m", "one"]);
    r.ok(&["switch", "-c", "other"]);
    r.write("big.bin", b"second\n");
    r.ok(&["commit", "-q", "-a", "-m", "two"]);
    r.ok(&["switch", "main"]);
    assert_eq!(r.read("big.bin"), b"first\n");
    r.ok(&["switch", "other"]);
    assert_eq!(r.read("big.bin"), b"second\n");
    assert_eq!(r.status(), "");
}

#[test]
fn pointers_match_git_lfs_when_it_is_installed() {
    if Command::new("git-lfs").arg("version").output().is_err() {
        eprintln!("git-lfs is not installed: skipped");
        return;
    }
    let r = Repo::new("sha1");
    let content: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
    r.write("big.bin", &content);
    r.ok(&["add", "-A"]);
    let out = git(&r.dir)
        .args(["lfs", "pointer", "--file=big.bin"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(r.staged("big.bin"), out.stdout);
    let oid = String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("oid sha256:").map(str::to_owned))
        .unwrap();
    assert!(Path::new(&r.object(&oid)).is_file());
}
