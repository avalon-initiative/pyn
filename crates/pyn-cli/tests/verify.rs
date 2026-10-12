//! `pyn verify`: exits 0 and prints nothing when history is intact, else lists every break and exits 1.

mod common;

use common::start;
use pyn_core::{RepoId, RepoPath, RevisionId};

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn an_intact_repository_prints_nothing_and_exits_zero() {
    let env = start();
    env.seed("alice", "Source/a.ts", "one", None);
    env.seed("alice", "Source/a.ts", "two", Some(1));
    env.seed("alice", "Content/m.umap", "map", None);
    let home = env.dir("home");

    assert_eq!(env.ok(&home, "alice", &["verify"]), "");
    assert_eq!(env.ok(&home, "alice", &["verify", "Source/a.ts"]), "");
    let out = env.run(&home, "alice", &["verify", "--verbose"]);
    assert!(out.status.success());
    let err = text(&out.stderr);
    assert!(err.contains("checked 3 revisions of 2 paths"), "{err}");
    assert!(err.contains(": ok"), "{err}");
}

#[test]
fn a_rewritten_revision_is_reported_with_exit_one() {
    let env = start();
    for (i, t) in ["one", "two", "three"].into_iter().enumerate() {
        env.seed("alice", "Source/a.ts", t, (i > 0).then_some(i as u64));
    }
    env.meta.tamper(
        &RepoId::new("t"),
        &RepoPath::new("Source/a.ts").unwrap(),
        RevisionId(2),
        |r| r.message = "rewritten".into(),
    );
    let home = env.dir("home");

    let out = env.run(&home, "alice", &["verify"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    let rows = common::cell_rows(&text(&out.stdout));
    assert_eq!(rows[0], ["PROBLEM", "WHAT", "PATH"]);
    assert_eq!(rows[1], ["hash_mismatch", "r2", "Source/a.ts"]);
    assert_eq!(rows.len(), 2, "{rows:?}");

    let err = env.fails(&home, "alice", &["log", "Source/a.ts"]);
    assert!(
        err.contains("history_corrupt") || err.contains("hash"),
        "{err}"
    );
}

#[test]
fn a_tampered_audit_log_is_reported() {
    let env = start();
    env.seed("alice", "Source/a.ts", "one", None);
    env.seed("alice", "Source/a.ts", "two", Some(1));
    env.audit
        .tamper(1, |e| e.actor = pyn_core::UserId::new("mallory"));
    let home = env.dir("home");

    let out = env.run(&home, "alice", &["verify"]);
    assert_eq!(out.status.code(), Some(1));
    let rows = common::cell_rows(&text(&out.stdout));
    assert_eq!(rows[1], ["hash_mismatch", "event 1", "(audit log)"]);
}
