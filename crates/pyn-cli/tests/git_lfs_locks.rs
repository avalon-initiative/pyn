//! The git home's Git LFS locking against a local Locks API server: `lockable` files read-only until locked, `lock`,
//! `unlock` and `locks`, and the pre-push check. Real git only builds the remote repository.

#[path = "../../pyn-gittransport/tests/common/gitsrv.rs"]
mod gitsrv;
#[path = "../../pyn-gittransport/tests/common/lockssrv.rs"]
mod lockssrv;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use gitsrv::*;
use lockssrv::LocksServer;
use tempfile::{TempDir, tempdir};

const ATTRS: &[u8] = b"*.psd lockable\n";

struct Repo {
    _tmp: TempDir,
    dir: PathBuf,
    bare: PathBuf,
    home: PathBuf,
    srv: LocksServer,
}

struct Out {
    code: i32,
    out: String,
    err: String,
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
        let srv = LocksServer::start();
        srv.add_user("alice", "a");
        srv.add_user("bob", "b");
        run(
            &dir,
            &["config", "lfs.url", &srv.url_as("alice", "a", "/lfs")],
        );
        run(&dir, &["remote", "add", "origin", bare.to_str().unwrap()]);
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
        if path.exists() {
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        }
        fs::write(path, bytes).unwrap();
    }

    fn writable(&self, name: &str) -> bool {
        fs::metadata(self.dir.join(name))
            .unwrap()
            .permissions()
            .mode()
            & 0o200
            != 0
    }

    fn pyn_in(&self, cwd: &Path, args: &[&str]) -> Out {
        let out = Command::new(env!("CARGO_BIN_EXE_pyn"))
            .current_dir(cwd)
            .args(args)
            .env_clear()
            .env("PATH", path_env())
            .env("HOME", &self.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_EDITOR", "true")
            .env("TZ", "UTC")
            .env("GIT_AUTHOR_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_DATE", "1700000000 +0000")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .output()
            .unwrap();
        Out {
            code: out.status.code().unwrap(),
            out: String::from_utf8_lossy(&out.stdout).into_owned(),
            err: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }

    fn pyn(&self, args: &[&str]) -> Out {
        self.pyn_in(&self.dir, args)
    }

    fn ok(&self, args: &[&str]) -> Out {
        let o = self.pyn(args);
        assert_eq!(o.code, 0, "{args:?}: {}", o.err);
        o
    }

    fn commit_all(&self, message: &str) {
        self.ok(&["add", "-A"]);
        self.ok(&["commit", "-q", "-m", message]);
    }

    /// A repository with `art/hero.psd` and `notes.txt` committed on `main`.
    fn seeded() -> Self {
        let r = Self::new();
        r.write("art/hero.psd", b"v1");
        r.write("notes.txt", b"notes\n");
        r.commit_all("one");
        r
    }
}

#[test]
fn lock_unlocks_a_lockable_file_and_unlock_locks_it_again() {
    let r = Repo::seeded();
    r.ok(&["switch", "-q", "-c", "work"]);
    assert!(!r.writable("art/hero.psd"), "read-only after a checkout");
    assert!(r.writable("notes.txt"));

    let o = r.ok(&["lock", "art/hero.psd"]);
    assert_eq!(o.out, "locked art/hero.psd\n");
    assert!(r.writable("art/hero.psd"));
    assert_eq!(r.srv.held(), [("alice".into(), "art/hero.psd".into())]);
    let bodies = r.srv.bodies();
    let create = bodies.iter().find(|b| b.get("path").is_some()).unwrap();
    assert_eq!(create["path"], "art/hero.psd");
    assert_eq!(create["ref"]["name"], "refs/heads/work");

    r.ok(&["switch", "-q", "main"]);
    assert!(r.writable("art/hero.psd"), "our lock keeps it writable");

    let o = r.ok(&["unlock", "art/hero.psd"]);
    assert_eq!(o.out, "unlocked art/hero.psd\n");
    assert!(!r.writable("art/hero.psd"));
    assert!(r.srv.held().is_empty());
}

#[test]
fn paths_are_taken_from_the_working_directory() {
    let r = Repo::seeded();
    let sub = r.dir.join("art");
    assert_eq!(
        r.pyn_in(&sub, &["lock", "hero.psd"]).out,
        "locked art/hero.psd\n"
    );
    assert_eq!(
        r.pyn_in(&sub, &["unlock", "./hero.psd"]).out,
        "unlocked art/hero.psd\n"
    );
}

#[test]
fn locks_lists_everyones_and_mine() {
    let r = Repo::seeded();
    r.srv.lock_for("bob", "other.psd");
    r.ok(&["lock", "art/hero.psd"]);
    let all = r.ok(&["locks"]).out;
    assert!(all.starts_with("OWNER"), "{all}");
    assert!(all.contains("alice") && all.contains("bob"), "{all}");
    assert!(all.lines().any(|l| l.ends_with("art/hero.psd")));
    let mine = r.ok(&["locks", "--mine"]).out;
    assert!(mine.starts_with("LOCKED"), "{mine}");
    assert!(mine.contains("Jan 02 2026 03:04"), "{mine}");
    assert!(
        mine.contains("art/hero.psd") && !mine.contains("other.psd"),
        "{mine}"
    );
    assert!(!mine.contains("bob"));
}

#[test]
fn a_locked_path_is_refused_with_its_holder() {
    let r = Repo::seeded();
    r.srv.lock_for("bob", "art/hero.psd");
    let o = r.pyn(&["lock", "art/hero.psd"]);
    assert_ne!(o.code, 0);
    assert!(o.err.contains("already locked by bob"), "{}", o.err);
}

#[test]
fn unlocking_anothers_lock_needs_force() {
    let r = Repo::seeded();
    r.srv.lock_for("bob", "art/hero.psd");
    let o = r.pyn(&["unlock", "art/hero.psd"]);
    assert_ne!(o.code, 0);
    assert!(o.err.contains("403"), "{}", o.err);
    assert_eq!(r.srv.held().len(), 1);

    let o = r.ok(&[
        "unlock",
        "--force",
        "--reason",
        "left the team",
        "art/hero.psd",
    ]);
    assert_eq!(o.out, "removed bob's lock on art/hero.psd\n");
    assert!(r.srv.held().is_empty());
    assert_eq!(r.srv.bodies().last().unwrap()["force"], true);
}

#[test]
fn unlocking_an_unlocked_path_says_so() {
    let r = Repo::seeded();
    let o = r.pyn(&["unlock", "art/hero.psd"]);
    assert_ne!(o.code, 0);
    assert!(o.err.contains("'art/hero.psd' is not locked"), "{}", o.err);
}

#[test]
fn base_is_a_pyn_server_option() {
    let r = Repo::seeded();
    let o = r.pyn(&["lock", "--base", "3", "art/hero.psd"]);
    assert_ne!(o.code, 0);
    assert!(o.err.contains("--base"), "{}", o.err);
    assert_eq!(r.srv.count("POST"), 0);
}

#[test]
fn push_is_refused_when_someone_else_locked_a_changed_file() {
    let r = Repo::seeded();
    r.ok(&["push", "-q", "origin", "main"]);
    r.srv.lock_for("bob", "art/hero.psd");
    r.write("art/hero.psd", b"v2");
    r.commit_all("two");
    let before = run(&r.bare, &["rev-parse", "main"]);

    let o = r.pyn(&["push", "-q", "origin", "main"]);
    assert_ne!(o.code, 0);
    assert!(o.err.contains("Unable to push locked files"), "{}", o.err);
    assert!(o.err.contains("art/hero.psd - bob"), "{}", o.err);
    assert_eq!(
        run(&r.bare, &["rev-parse", "main"]),
        before,
        "nothing was sent"
    );
    let verify = r
        .srv
        .bodies()
        .into_iter()
        .find(|b| b["ref"].is_object())
        .unwrap();
    assert_eq!(verify["ref"]["name"], "refs/heads/main");

    r.ok(&["push", "-q", "--no-verify", "origin", "main"]);
    assert_ne!(run(&r.bare, &["rev-parse", "main"]), before);
}

#[test]
fn push_passes_for_our_own_lock_and_for_other_files() {
    let r = Repo::seeded();
    r.ok(&["push", "-q", "origin", "main"]);
    r.srv.lock_for("bob", "elsewhere.psd");
    r.ok(&["lock", "art/hero.psd"]);
    r.write("art/hero.psd", b"v2");
    r.write("notes.txt", b"changed\n");
    r.commit_all("two");
    let o = r.pyn(&["push", "-q", "origin", "main"]);
    assert_eq!(o.code, 0, "{}", o.err);
    assert!(r.srv.count("POST /lfs/locks/verify") >= 1);
}

#[test]
fn a_force_unlocked_file_can_be_pushed() {
    let r = Repo::seeded();
    r.ok(&["push", "-q", "origin", "main"]);
    r.srv.lock_for("bob", "art/hero.psd");
    r.write("art/hero.psd", b"v2");
    r.commit_all("two");
    assert_ne!(r.pyn(&["push", "-q", "origin", "main"]).code, 0);
    r.ok(&["unlock", "--force", "--reason", "x", "art/hero.psd"]);
    r.ok(&["push", "-q", "origin", "main"]);
}

#[test]
fn nothing_is_checked_without_lockable_files() {
    let r = Repo::new();
    fs::write(r.dir.join(".gitattributes"), b"").unwrap();
    r.write("a.txt", b"a\n");
    r.commit_all("one");
    r.ok(&["push", "-q", "origin", "main"]);
    assert_eq!(r.srv.log().len(), 0);
}

#[test]
fn locksverify_false_skips_the_check() {
    let r = Repo::seeded();
    run(&r.dir, &["config", "lfs.locksverify", "false"]);
    r.srv.lock_for("bob", "art/hero.psd");
    r.write("art/hero.psd", b"v2");
    r.commit_all("two");
    r.ok(&["push", "-q", "origin", "main"]);
    assert_eq!(r.srv.count("POST /lfs/locks/verify"), 0);
}

#[test]
fn a_server_without_verify_does_not_block_pushes() {
    let r = Repo::seeded();
    r.srv.state.lock().unwrap().verify_status = Some(501);
    r.srv.lock_for("bob", "art/hero.psd");
    r.write("art/hero.psd", b"v2");
    r.commit_all("two");
    r.ok(&["push", "-q", "origin", "main"]);
}
