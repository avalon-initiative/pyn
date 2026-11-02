//! The git home's Git LFS client against a local Batch API server: download on smudge, upload before push,
//! endpoint settings and credentials. Real git only builds the remote repository.

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

    fn pyn_in(&self, cwd: &std::path::Path, args: &[&str]) -> (i32, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_pyn"))
            .current_dir(cwd)
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
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn pyn(&self, args: &[&str]) -> (i32, String) {
        self.pyn_in(&self.dir, args)
    }

    fn ok(&self, args: &[&str]) -> String {
        let (code, err) = self.pyn(args);
        assert_eq!(code, 0, "{args:?}: {err}");
        err
    }

    fn commit_all(&self, message: &str) {
        self.ok(&["add", "-A"]);
        self.ok(&["commit", "-q", "-m", message]);
    }

    fn add_remote(&self) {
        let bare = self.bare.to_str().unwrap();
        run(&self.dir, &["remote", "add", "origin", bare]);
    }

    /// Removes the local store and the work tree copy of `name`, as a fresh clone would lack them.
    fn forget(&self, name: &str) {
        let _ = fs::remove_dir_all(self.dir.join(".git/lfs"));
        fs::remove_file(self.dir.join(name)).unwrap();
    }
}

fn warnings(stderr: &str) -> usize {
    stderr.lines().filter(|l| l.contains("Git LFS")).count()
}

fn pointer_of(content: &[u8]) -> Vec<u8> {
    format!(
        "version https://git-lfs.github.com/spec/v1\noid sha256:{}\nsize {}\n",
        sha256_hex(content),
        content.len()
    )
    .into_bytes()
}

#[test]
fn push_uploads_new_objects_before_the_refs_move() {
    let r = Repo::new();
    r.add_remote();
    let content = vec![42u8; 100_000];
    r.write("big.bin", &content);
    r.write("plain.txt", b"text\n");
    r.commit_all("one");
    r.ok(&["push", "-q", "origin", "main"]);
    assert_eq!(r.srv.has(&sha256_hex(&content)).unwrap(), content);
    assert_eq!(r.srv.count("PUT"), 1);
    assert_eq!(r.srv.count("POST /lfs/objects/verify"), 1);
    assert_eq!(run(&r.bare, &["rev-parse", "main"]).trim().len(), 40);

    r.write("plain.txt", b"changed\n");
    r.ok(&["commit", "-q", "-a", "-m", "two"]);
    r.ok(&["push", "-q", "origin", "main"]);
    assert_eq!(r.srv.count("PUT"), 1, "nothing new to upload");
    assert_eq!(
        r.srv.count("POST /lfs/objects/batch"),
        1,
        "no batch without a new pointer"
    );
}

#[test]
fn push_sends_only_pointers_of_commits_the_remote_lacks() {
    let r = Repo::new();
    r.add_remote();
    r.write("a.bin", b"first object");
    r.commit_all("one");
    r.ok(&["push", "-q", "origin", "main"]);
    r.write("b.bin", b"second object");
    r.commit_all("two");
    r.ok(&["push", "-q", "origin", "main"]);
    assert_eq!(r.srv.count("PUT"), 2);
    assert_eq!(r.srv.count("POST /lfs/objects/batch"), 2);
}

#[test]
fn a_refused_upload_stops_the_push() {
    let r = Repo::new();
    r.add_remote();
    r.srv.state.lock().unwrap().refuse_uploads = true;
    r.write("big.bin", b"payload");
    r.commit_all("one");
    let (code, err) = r.pyn(&["push", "origin", "main"]);
    assert_ne!(code, 0, "{err}");
    assert!(err.contains("403"), "{err}");
    let refs = run(&r.bare, &["for-each-ref"]);
    assert_eq!(refs, "", "the remote must not get the commit");
}

#[test]
fn no_verify_skips_the_upload() {
    let r = Repo::new();
    r.add_remote();
    r.write("big.bin", b"payload");
    r.commit_all("one");
    r.ok(&["push", "-q", "--no-verify", "origin", "main"]);
    assert!(r.srv.log().is_empty());
}

#[test]
fn smudge_downloads_a_missing_object() {
    let r = Repo::new();
    let content = b"large file content\n".repeat(1000);
    r.srv.put(&content);
    r.write("big.bin", &pointer_of(&content));
    r.commit_all("one");
    fs::remove_file(r.dir.join("big.bin")).unwrap();
    let err = r.ok(&["restore", "big.bin"]);
    assert_eq!(warnings(&err), 0, "{err}");
    assert_eq!(r.read("big.bin"), content);
    assert_eq!(r.srv.count("GET"), 1);
    fs::remove_file(r.dir.join("big.bin")).unwrap();
    r.ok(&["restore", "big.bin"]);
    assert_eq!(r.srv.count("GET"), 1, "the second restore uses the store");
}

#[test]
fn round_trip_between_two_clones() {
    let r = Repo::new();
    r.add_remote();
    let content = vec![7u8; 300_000];
    r.write("big.bin", &content);
    r.commit_all("one");
    r.ok(&["push", "-q", "origin", "main"]);
    let root = r.dir.parent().unwrap();
    let second = root.join("second");
    let (code, err) = r.pyn_in(
        root,
        &[
            "clone",
            "-q",
            r.bare.to_str().unwrap(),
            second.to_str().unwrap(),
        ],
    );
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        fs::read(second.join("big.bin")).unwrap(),
        pointer_of(&content),
        "no lfs.url in the clone: pointer kept"
    );
    run(&second, &["config", "lfs.url", &r.srv.url("/lfs")]);
    fs::remove_file(second.join("big.bin")).unwrap();
    let (code, err) = r.pyn_in(&second, &["restore", "big.bin"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(fs::read(second.join("big.bin")).unwrap(), content);
}

#[test]
fn the_endpoint_is_derived_from_the_remote_url() {
    let r = Repo::new();
    run(&r.dir, &["config", "--unset", "lfs.url"]);
    let content = b"derived".to_vec();
    r.srv.put(&content);
    r.write("big.bin", &pointer_of(&content));
    r.commit_all("one");
    run(
        &r.dir,
        &["remote", "add", "origin", &r.srv.url("/team/game")],
    );
    r.forget("big.bin");
    r.ok(&["restore", "big.bin"]);
    assert_eq!(r.read("big.bin"), content);
    let want = "POST /team/game.git/info/lfs/objects/batch".to_string();
    assert!(r.srv.log().contains(&want), "{:?}", r.srv.log());
}

#[test]
fn a_per_remote_url_beats_lfs_url() {
    let r = Repo::new();
    let content = b"per remote".to_vec();
    r.srv.put(&content);
    r.write("big.bin", &pointer_of(&content));
    r.commit_all("one");
    run(
        &r.dir,
        &["remote", "add", "origin", "http://127.0.0.1:9/never"],
    );
    let special = r.srv.url("/special");
    run(&r.dir, &["config", "remote.origin.lfsurl", &special]);
    r.forget("big.bin");
    r.ok(&["restore", "big.bin"]);
    assert_eq!(r.read("big.bin"), content);
    assert_eq!(r.srv.count("POST /special/objects/batch"), 1);
}

#[test]
fn lfsconfig_in_the_work_tree_supplies_the_url() {
    let r = Repo::new();
    run(&r.dir, &["config", "--unset", "lfs.url"]);
    let content = b"from .lfsconfig".to_vec();
    r.srv.put(&content);
    let cfg = format!("[lfs]\n\turl = {}\n", r.srv.url("/shared"));
    r.write(".lfsconfig", cfg.as_bytes());
    r.write("big.bin", &pointer_of(&content));
    r.commit_all("one");
    r.forget("big.bin");
    r.ok(&["restore", "big.bin"]);
    assert_eq!(r.read("big.bin"), content);

    run(&r.dir, &["config", "lfs.url", &r.srv.url("/mine")]);
    r.forget("big.bin");
    r.ok(&["restore", "big.bin"]);
    assert_eq!(
        r.srv.count("POST /mine/objects/batch"),
        1,
        ".git/config wins over .lfsconfig"
    );
}

#[test]
fn lfsconfig_cannot_set_a_credential_helper() {
    let r = Repo::new();
    let marker = r.dir.parent().unwrap().join("ran");
    let content = b"guarded".to_vec();
    r.srv.put(&content);
    r.srv.require("ann", "s3cret");
    let cfg = format!("[credential]\n\thelper = \"!touch {}\"\n", marker.display());
    r.write(".lfsconfig", cfg.as_bytes());
    r.write("big.bin", &pointer_of(&content));
    r.commit_all("one");
    r.forget("big.bin");
    let (_, err) = r.pyn(&["restore", "big.bin"]);
    assert!(!marker.exists(), "{err}");
    assert_eq!(r.read("big.bin"), pointer_of(&content));
}

#[test]
fn credentials_come_from_the_configured_helper() {
    let r = Repo::new();
    r.srv.require("ann", "s3cret");
    let helper = "!f() { echo username=ann; echo password=s3cret; }; f";
    run(&r.dir, &["config", "credential.helper", helper]);
    let content = b"private".to_vec();
    r.srv.put(&content);
    r.write("big.bin", &pointer_of(&content));
    r.commit_all("one");
    r.forget("big.bin");
    let err = r.ok(&["restore", "big.bin"]);
    assert_eq!(warnings(&err), 0, "{err}");
    assert_eq!(r.read("big.bin"), content);
}

#[test]
fn without_credentials_the_pointer_stays_and_one_warning_is_printed() {
    let r = Repo::new();
    r.srv.require("ann", "s3cret");
    let content = b"private".to_vec();
    r.srv.put(&content);
    r.write("a.bin", &pointer_of(&content));
    r.write("b.bin", &pointer_of(&content));
    r.commit_all("one");
    let _ = fs::remove_dir_all(r.dir.join(".git/lfs"));
    fs::remove_file(r.dir.join("a.bin")).unwrap();
    fs::remove_file(r.dir.join("b.bin")).unwrap();
    let (code, err) = r.pyn(&["restore", "."]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(warnings(&err), 1, "{err}");
    assert_eq!(r.read("a.bin"), pointer_of(&content));
}

#[test]
fn a_corrupt_download_keeps_the_pointer_and_stores_nothing() {
    let r = Repo::new();
    let content = b"will be mangled".repeat(10);
    r.srv.put(&content);
    r.srv.state.lock().unwrap().corrupt_downloads = true;
    r.write("big.bin", &pointer_of(&content));
    r.commit_all("one");
    r.forget("big.bin");
    let (code, err) = r.pyn(&["restore", "big.bin"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(warnings(&err), 1, "{err}");
    assert!(err.contains("does not match"), "{err}");
    assert_eq!(r.read("big.bin"), pointer_of(&content));
    assert!(!r.dir.join(".git/lfs/objects").exists());
}

#[test]
fn fetch_include_and_exclude_limit_what_is_downloaded() {
    let r = Repo::new();
    let (a, b) = (b"in assets".to_vec(), b"in docs".to_vec());
    r.srv.put(&a);
    r.srv.put(&b);
    r.write("assets/a.bin", &pointer_of(&a));
    r.write("docs/b.bin", &pointer_of(&b));
    r.commit_all("one");
    run(&r.dir, &["config", "lfs.fetchinclude", "assets/"]);
    let wipe = || {
        let _ = fs::remove_dir_all(r.dir.join(".git/lfs"));
        let _ = fs::remove_file(r.dir.join("assets/a.bin"));
        let _ = fs::remove_file(r.dir.join("docs/b.bin"));
    };
    wipe();
    r.ok(&["restore", "."]);
    assert_eq!(r.read("assets/a.bin"), a);
    assert_eq!(r.read("docs/b.bin"), pointer_of(&b));

    run(&r.dir, &["config", "--unset", "lfs.fetchinclude"]);
    run(&r.dir, &["config", "lfs.fetchexclude", "*.bin, nothing/"]);
    wipe();
    let before = r.srv.count("POST");
    r.ok(&["restore", "."]);
    assert_eq!(r.srv.count("POST"), before, "everything is excluded");
    assert_eq!(r.read("assets/a.bin"), pointer_of(&a));
}

#[test]
fn lfs_storage_moves_the_object_store() {
    let r = Repo::new();
    run(&r.dir, &["config", "lfs.storage", "elsewhere/lfs"]);
    r.write("big.bin", b"stored elsewhere");
    r.commit_all("one");
    let oid = sha256_hex(b"stored elsewhere");
    let path = r
        .dir
        .join(".git/elsewhere/lfs/objects")
        .join(&oid[..2])
        .join(&oid[2..4])
        .join(&oid);
    assert!(path.is_file());
    assert!(!r.dir.join(".git/lfs/objects").exists());
    fs::remove_file(r.dir.join("big.bin")).unwrap();
    r.ok(&["restore", "big.bin"]);
    assert_eq!(r.read("big.bin"), b"stored elsewhere");
}

#[test]
fn pointers_with_extensions_are_kept_and_never_downloaded() {
    let r = Repo::new();
    let oid = sha256_hex(b"x");
    let ext = format!(
        "version https://git-lfs.github.com/spec/v1\next-0-foo sha256:{}\noid sha256:{oid}\nsize 1\n",
        "a".repeat(64)
    );
    r.srv.put(b"x");
    r.write("big.bin", ext.as_bytes());
    r.commit_all("one");
    fs::remove_file(r.dir.join("big.bin")).unwrap();
    let err = r.ok(&["restore", "big.bin"]);
    assert_eq!(warnings(&err), 1, "{err}");
    assert_eq!(r.read("big.bin"), ext.as_bytes());
    assert!(r.srv.log().is_empty());
}
