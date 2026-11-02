//! The Git LFS Locks client against a local Locks API server: create, list, verify, unlock, force unlock, the ref, paging
//! and credentials.

mod common;

use std::path::Path;

use common::lockssrv::LocksServer;
use pyn_git::Config;
use pyn_gittransport::credential::Prompt;
use pyn_gittransport::lfs::Client;
use pyn_gittransport::{Env, Error};
use reqwest::Url;
use tempfile::tempdir;

struct NoPrompt;

impl Prompt for NoPrompt {
    fn ask(&mut self, _prompt: &str, _secret: bool) -> pyn_gittransport::Result<String> {
        Err(Error::Credential("no prompt".into()))
    }
}

fn client(url: &str, home: &Path) -> Client {
    let env = Env::default()
        .with("HOME", home.to_str().unwrap())
        .with("GIT_CONFIG_NOSYSTEM", "1")
        .with("GIT_TERMINAL_PROMPT", "0");
    Client::new(
        Url::parse(url).unwrap(),
        &Config::parse(b"").unwrap(),
        &env,
        Box::new(NoPrompt),
    )
    .unwrap()
}

fn setup() -> (LocksServer, tempfile::TempDir) {
    let srv = LocksServer::start();
    srv.add_user("alice", "a");
    srv.add_user("bob", "b");
    (srv, tempdir().unwrap())
}

#[test]
fn create_then_list_then_unlock() {
    let (srv, home) = setup();
    let mut alice = client(&srv.url_as("alice", "a", "/r/info/lfs"), home.path());
    let lock = alice
        .create_lock("art/hero.psd", Some("refs/heads/main"))
        .unwrap();
    assert_eq!(lock.path, "art/hero.psd");
    assert_eq!(lock.owner.name, "alice");
    assert_eq!(srv.bodies()[0]["ref"]["name"], "refs/heads/main");

    let listed = alice
        .list_locks(None, None, Some("refs/heads/main"))
        .unwrap();
    assert_eq!(listed, vec![lock.clone()]);
    assert!(
        srv.log()
            .iter()
            .any(|l| l.contains("refspec=refs%2Fheads%2Fmain"))
    );
    assert_eq!(alice.list_locks(Some("other"), None, None).unwrap(), vec![]);
    assert_eq!(
        alice.list_locks(None, Some(&lock.id), None).unwrap().len(),
        1
    );

    let gone = alice.unlock(&lock.id, false, None).unwrap();
    assert_eq!(gone.id, lock.id);
    assert!(srv.held().is_empty());
}

#[test]
fn a_taken_path_names_its_holder() {
    let (srv, home) = setup();
    srv.lock_for("bob", "x.bin");
    let mut alice = client(&srv.url_as("alice", "a", "/r/info/lfs"), home.path());
    let e = alice.create_lock("x.bin", None).unwrap_err().to_string();
    assert!(e.contains("already locked by bob"), "{e}");
}

#[test]
fn verify_splits_ours_from_theirs_across_pages() {
    let (srv, home) = setup();
    srv.state.lock().unwrap().page = 2;
    for (user, path) in [
        ("alice", "a"),
        ("bob", "b"),
        ("alice", "c"),
        ("bob", "d"),
        ("bob", "e"),
    ] {
        srv.lock_for(user, path);
    }
    let mut alice = client(&srv.url_as("alice", "a", "/r/info/lfs"), home.path());
    let v = alice
        .verify_locks(Some("refs/heads/main"))
        .unwrap()
        .unwrap();
    let paths = |l: &[pyn_gittransport::lfs_locks::Lock]| {
        l.iter().map(|l| l.path.clone()).collect::<Vec<_>>()
    };
    assert_eq!(paths(&v.ours), ["a", "c"]);
    assert_eq!(paths(&v.theirs), ["b", "d", "e"]);
    let followed = srv
        .bodies()
        .iter()
        .filter(|b| b.get("cursor").is_some())
        .count();
    assert_eq!(followed, 2);
    assert_eq!(alice.list_locks(None, None, None).unwrap().len(), 5);
}

#[test]
fn unlocking_anothers_lock_needs_force() {
    let (srv, home) = setup();
    let id = srv.lock_for("bob", "x.bin");
    let mut alice = client(&srv.url_as("alice", "a", "/r/info/lfs"), home.path());
    let e = alice.unlock(&id, false, None).unwrap_err().to_string();
    assert!(e.contains("403"), "{e}");
    assert!(e.contains("not your lock"), "{e}");
    assert_eq!(alice.unlock(&id, true, None).unwrap().owner.name, "bob");
    assert_eq!(srv.bodies().last().unwrap()["force"], true);
    assert!(
        alice
            .unlock(&id, false, None)
            .unwrap_err()
            .to_string()
            .contains("404")
    );
}

#[test]
fn verify_is_none_where_the_server_lacks_it() {
    let (srv, home) = setup();
    srv.state.lock().unwrap().verify_status = Some(501);
    let mut alice = client(&srv.url_as("alice", "a", "/r/info/lfs"), home.path());
    assert!(alice.verify_locks(None).unwrap().is_none());
}

#[test]
fn bad_credentials_fail_to_authenticate() {
    let (srv, home) = setup();
    let mut mallory = client(&srv.url_as("alice", "wrong", "/r/info/lfs"), home.path());
    assert!(mallory.create_lock("x", None).is_err());
    assert!(srv.held().is_empty());
}
