//! `pyn lfs` on a git repository: track, untrack, ls-files, fetch, pull, push and prune, against a local Batch API
//! server. Real git only builds the remote repository.

#[path = "../../pyn-gittransport/tests/common/gitsrv.rs"]
mod gitsrv;
#[path = "../../pyn-gittransport/tests/common/lfssrv.rs"]
mod lfssrv;

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use gitsrv::*;
use lfssrv::{LfsServer, sha256_hex};
use tempfile::{TempDir, tempdir};

const ATTRS: &[u8] = b"*.bin filter=lfs diff=lfs merge=lfs -text\n";

struct Repo {
    _tmp: TempDir,
    dir: PathBuf,
    bare: PathBuf,
    home: PathBuf,
    srv: LfsServer,
}

impl Repo {
    fn new() -> Self {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let dir = root.join("w");
        init(&dir, "sha1", false);
        run(&dir, &["config", "user.name", "t"]);
        run(&dir, &["config", "user.email", "t@example.com"]);
        let bare = root.join("served.git");
        init(&bare, "sha1", true);
        let srv = LfsServer::start();
        run(&dir, &["config", "lfs.url", &srv.url("/lfs")]);
        fs::write(dir.join(".gitattributes"), ATTRS).unwrap();
        Repo {
            home: root.join("home"),
            _tmp: tmp,
            dir,
            bare,
            srv,
        }
    }

    fn write(&self, name: &str, bytes: &[u8]) {
        let path = self.dir.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    fn read(&self, name: &str) -> Vec<u8> {
        fs::read(self.dir.join(name)).unwrap()
    }

    fn pyn(&self, args: &[&str]) -> (i32, String, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_pyn"))
            .current_dir(&self.dir)
            .args(args)
            .env_clear()
            .env("PATH", path_env())
            .env("HOME", &self.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_EDITOR", "true")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .output()
            .unwrap();
        (
            out.status.code().unwrap(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn ok(&self, args: &[&str]) -> String {
        let (code, out, err) = self.pyn(args);
        assert_eq!(code, 0, "{args:?}: {err}");
        out
    }

    fn commit_all(&self, message: &str) {
        self.ok(&["add", "-A"]);
        self.ok(&["commit", "-q", "-m", message]);
    }

    fn add_remote(&self) {
        let bare = self.bare.to_str().unwrap();
        run(&self.dir, &["config", "remote.origin.url", bare]);
        run(
            &self.dir,
            &[
                "config",
                "remote.origin.fetch",
                "+refs/heads/*:refs/remotes/origin/*",
            ],
        );
    }

    fn object(&self, content: &[u8]) -> PathBuf {
        let oid = sha256_hex(content);
        self.dir
            .join(".git/lfs/objects")
            .join(&oid[..2])
            .join(&oid[2..4])
            .join(oid)
    }

    fn forget_store(&self) {
        let _ = fs::remove_dir_all(self.dir.join(".git/lfs"));
    }
}

#[test]
fn track_lists_adds_and_removes_patterns() {
    let r = Repo::new();
    fs::remove_file(r.dir.join(".gitattributes")).unwrap();
    assert!(
        r.ok(&["lfs", "track"])
            .starts_with("Listing tracked patterns\n")
    );
    assert_eq!(
        r.ok(&["lfs", "track", "*.psd", "my file.bin"]),
        "Tracking \"*.psd\"\nTracking \"my file.bin\"\n"
    );
    assert_eq!(
        fs::read_to_string(r.dir.join(".gitattributes")).unwrap(),
        "*.psd filter=lfs diff=lfs merge=lfs -text\nmy[[:space:]]file.bin filter=lfs diff=lfs merge=lfs -text\n"
    );
    assert_eq!(
        r.ok(&["lfs", "track", "*.psd"]),
        "\"*.psd\" already supported\n"
    );
    assert_eq!(
        r.ok(&["lfs", "track"]),
        "Listing tracked patterns\n    *.psd (.gitattributes)\n    my file.bin (.gitattributes)\n"
    );
    r.write("a.psd", b"layers");
    r.ok(&["add", "a.psd"]);
    let staged = git(&r.dir)
        .args(["cat-file", "blob", ":a.psd"])
        .output()
        .unwrap()
        .stdout;
    assert_eq!(staged, pointer_of(b"layers"));

    assert_eq!(r.ok(&["lfs", "untrack", "*.psd"]), "Untracking \"*.psd\"\n");
    assert_eq!(r.ok(&["lfs", "untrack", "*.nothing"]), "");
    assert_eq!(
        fs::read_to_string(r.dir.join(".gitattributes")).unwrap(),
        "my[[:space:]]file.bin filter=lfs diff=lfs merge=lfs -text\n"
    );
}

#[test]
fn ls_files_shows_pointers_and_whether_the_object_is_stored() {
    let r = Repo::new();
    r.write("a.bin", b"stored");
    r.write("b.bin", b"gone");
    r.write("plain.txt", b"text\n");
    r.commit_all("one");
    fs::remove_file(r.object(b"gone")).unwrap();
    let a = sha256_hex(b"stored");
    let b = sha256_hex(b"gone");
    assert_eq!(
        r.ok(&["lfs", "ls-files"]),
        format!("{} * a.bin\n{} - b.bin\n", &a[..10], &b[..10])
    );
    assert_eq!(
        r.ok(&["lfs", "ls-files", "--long"]),
        format!("{a} * a.bin\n{b} - b.bin\n")
    );
    assert_eq!(r.ok(&["lfs", "ls-files", "-n"]), "a.bin\nb.bin\n");
}

#[test]
fn push_fetch_and_pull_move_objects() {
    let r = Repo::new();
    r.add_remote();
    let content = vec![9u8; 50_000];
    r.write("big.bin", &content);
    r.commit_all("one");
    run(&r.dir, &["push", "-q", "--no-verify", "origin", "main"]);
    assert!(r.srv.log().is_empty());

    assert_eq!(
        r.ok(&["lfs", "push"]),
        "uploaded 1 Git LFS object(s) to origin\n"
    );
    assert_eq!(r.srv.has(&sha256_hex(&content)).unwrap(), content);
    assert_eq!(r.srv.count("PUT"), 1);

    r.forget_store();
    assert_eq!(
        r.ok(&["lfs", "fetch"]),
        "fetched 1 Git LFS object(s) from origin\n"
    );
    assert_eq!(fs::read(r.object(&content)).unwrap(), content);
    assert_eq!(
        r.ok(&["lfs", "fetch", "origin", "HEAD"]),
        "fetched 0 Git LFS object(s) from origin\n"
    );

    r.forget_store();
    fs::write(r.dir.join("big.bin"), pointer_of(&content)).unwrap();
    assert_eq!(
        r.ok(&["lfs", "pull"]),
        "replaced 1 pointer file(s) with their objects\n"
    );
    assert_eq!(r.read("big.bin"), content);
    assert_eq!(r.ok(&["status", "--porcelain"]), "");
}

#[test]
fn pull_leaves_modified_files_alone() {
    let r = Repo::new();
    r.add_remote();
    r.write("big.bin", b"original");
    r.commit_all("one");
    r.ok(&["lfs", "push"]);
    r.forget_store();
    r.write("big.bin", b"my edit");
    r.ok(&["lfs", "pull"]);
    assert_eq!(r.read("big.bin"), b"my edit");
}

#[test]
fn fetch_without_a_remote_is_an_error() {
    let r = Repo::new();
    r.write("big.bin", b"x");
    r.commit_all("one");
    let (code, _, err) = r.pyn(&["lfs", "fetch"]);
    assert_ne!(code, 0);
    assert!(err.contains("no remote"), "{err}");
    let (code, _, err) = r.pyn(&["lfs", "push", "nowhere"]);
    assert_ne!(code, 0);
    assert!(err.contains("not a configured remote"), "{err}");
}

#[test]
fn prune_keeps_unpushed_and_head_objects_and_deletes_the_rest() {
    let r = Repo::new();
    r.add_remote();
    r.write("big.bin", b"version one");
    r.commit_all("one");
    r.write("big.bin", b"version two");
    r.commit_all("two");
    let (v1, v2) = (r.object(b"version one"), r.object(b"version two"));

    let out = r.ok(&["lfs", "prune", "--days", "0"]);
    assert_eq!(
        out, "pruned 0 Git LFS object(s) (0 bytes), kept 2\n",
        "version two is unpushed, and version one is its parent's"
    );
    assert!(v1.exists() && v2.exists());

    r.ok(&["push", "-q", "origin", "main"]);
    let out = r.ok(&["lfs", "prune", "--days", "0", "--dry-run", "-v"]);
    assert_eq!(
        out,
        format!(
            "{} 11\nwould prune 1 Git LFS object(s) (11 bytes), kept 1\n",
            sha256_hex(b"version one")
        )
    );
    assert!(v1.exists());
    r.ok(&["lfs", "prune", "--days", "0"]);
    assert!(!v1.exists() && v2.exists());

    let out = r.ok(&["lfs", "prune"]);
    assert_eq!(out, "pruned 0 Git LFS object(s) (0 bytes), kept 1\n");
}

fn pointer_of(content: &[u8]) -> Vec<u8> {
    format!(
        "version https://git-lfs.github.com/spec/v1\noid sha256:{}\nsize {}\n",
        sha256_hex(content),
        content.len()
    )
    .into_bytes()
}
