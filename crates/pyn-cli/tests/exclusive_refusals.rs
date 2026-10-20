//! Refusals on exclusive paths in a workspace of a pyn server: `commit`, `push`, `restore`, `switch`, `checkout --` and
//! `merge` without the lock, with another user's lock, and after a lease lapses. Exit codes and messages are asserted.

#![cfg(unix)]

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{Duration, Utc};
use common::{Env, REPO, start, start_with_clock};
use pyn_core::ManualClock;

const MAP: &str = "Content/m.umap";

fn clone_with(env: &Env, name: &str, extra: &[&str]) -> PathBuf {
    let ws = env.dir(name);
    let source = env.source(REPO);
    let mut args = vec!["clone"];
    args.extend(extra);
    args.extend([source.as_str(), ws.to_str().unwrap()]);
    env.ok(&env.dir("home"), "alice", &args);
    ws.canonicalize().unwrap()
}

fn write(ws: &Path, path: &str, text: &str) {
    let file = ws.join(path);
    if file.exists() {
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    std::fs::write(file, text).unwrap();
}

fn read(ws: &Path, path: &str) -> String {
    std::fs::read_to_string(ws.join(path)).unwrap()
}

fn read_only(ws: &Path, path: &str) -> bool {
    std::fs::metadata(ws.join(path))
        .unwrap()
        .permissions()
        .mode()
        & 0o222
        == 0
}

/// Runs a command that must fail and returns its exit code and standard error.
fn refused(env: &Env, ws: &Path, user: &str, args: &[&str]) -> (i32, String) {
    let out = env.run(ws, user, args);
    assert!(!out.status.success(), "pyn {args:?} should have failed");
    (
        out.status.code().unwrap(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// A server with the map at r1 and r2, and a workspace with the map edited and no lock held.
fn edited_unlocked() -> (Env, PathBuf) {
    let env = start();
    env.seed("alice", MAP, "map one", None);
    env.seed("alice", MAP, "map two", Some(1));
    env.seed("alice", "Source/a.cpp", "one", None);
    let ws = clone_with(&env, "ws", &["--no-local-store"]);
    write(&ws, MAP, "map edited");
    (env, ws)
}

#[test]
fn commit_with_nothing_staged_is_refused_even_when_an_unlocked_exclusive_file_is_edited() {
    let (env, ws) = edited_unlocked();
    let (code, err) = refused(&env, &ws, "alice", &["commit", "-m", "x"]);
    assert_eq!(code, 1);
    assert_eq!(
        err.trim(),
        "nothing to commit; stage changes with `pyn add`, or use `pyn commit -a`"
    );
}

#[test]
fn commit_of_an_unlocked_exclusive_path_names_the_lock_requirement() {
    let (env, ws) = edited_unlocked();
    let (code, err) = refused(&env, &ws, "alice", &["commit", MAP, "-m", "x"]);
    assert_eq!(code, 128);
    assert_eq!(
        err.trim(),
        "fatal: lock_required (409): Content/m.umap is exclusive: lock it before checking in"
    );
    assert_eq!(env.ok(&ws, "alice", &["get", MAP]), "map two");
}

#[test]
fn commit_dash_a_refuses_an_unlocked_exclusive_file_and_commits_nothing() {
    let (env, ws) = edited_unlocked();
    write(&ws, "Source/a.cpp", "two");
    let (code, err) = refused(&env, &ws, "alice", &["commit", "-a", "-m", "x"]);
    assert_eq!(code, 128);
    assert_eq!(
        err.trim(),
        "fatal: Content/m.umap is exclusive and you do not hold its lock; run `pyn lock Content/m.umap` first"
    );
    assert_eq!(env.ok(&ws, "alice", &["get", "Source/a.cpp"]), "one");
}

#[test]
fn commit_while_another_user_holds_the_lock_names_the_holder() {
    let (env, ws) = edited_unlocked();
    env.ok(&ws, "bob", &["lock", MAP]);
    let (code, err) = refused(&env, &ws, "alice", &["commit", MAP, "-m", "x"]);
    assert_eq!(code, 128);
    assert!(
        err.contains("lock_held (409)") && err.contains("Content/m.umap is locked by bob until"),
        "{err}"
    );
    let (code, err) = refused(&env, &ws, "alice", &["add", MAP]);
    assert_eq!(code, 128);
    assert!(err.contains("locked by bob"), "{err}");
    let (code, err) = refused(&env, &ws, "alice", &["commit", "-a", "-m", "x"]);
    assert_eq!(code, 128);
    assert!(err.contains("locked by bob"), "{err}");
    assert_eq!(env.ok(&ws, "alice", &["get", MAP]), "map two");
}

#[test]
fn a_lease_that_lapses_between_add_and_commit_is_refused_at_commit() {
    let clock = Arc::new(ManualClock::new(Utc::now()));
    let env = start_with_clock(clock.clone());
    env.seed("alice", MAP, "map one", None);
    let ws = clone_with(&env, "ws", &["--no-local-store"]);
    env.ok(&ws, "alice", &["lock", MAP]);
    write(&ws, MAP, "map two");
    env.ok(&ws, "alice", &["add", MAP]);

    clock.advance(Duration::hours(9));
    let (code, err) = refused(&env, &ws, "alice", &["commit", "-m", "x"]);
    assert_eq!(code, 128);
    assert!(
        err.contains("lock_required (409)") && err.contains("lock it before checking in"),
        "{err}"
    );
    assert_eq!(env.ok(&ws, "alice", &["get", MAP]), "map one");

    env.ok(&ws, "alice", &["lock", MAP]);
    env.ok(&ws, "alice", &["commit", "-m", "x"]);
    assert_eq!(env.ok(&ws, "alice", &["get", MAP]), "map two");
}

#[test]
fn an_expired_lease_taken_by_another_user_refuses_the_commit_with_lock_held() {
    let clock = Arc::new(ManualClock::new(Utc::now()));
    let env = start_with_clock(clock.clone());
    env.seed("alice", MAP, "map one", None);
    let ws = clone_with(&env, "ws", &["--no-local-store"]);
    env.ok(&ws, "alice", &["lock", MAP]);
    write(&ws, MAP, "map two");
    env.ok(&ws, "alice", &["add", MAP]);

    clock.advance(Duration::hours(9));
    env.ok(&ws, "bob", &["lock", MAP]);
    let (code, err) = refused(&env, &ws, "alice", &["commit", "-m", "x"]);
    assert_eq!(code, 128);
    assert!(err.contains("locked by bob"), "{err}");
}

#[test]
fn push_after_the_lease_lapses_is_rejected_with_lock_required_and_sends_nothing() {
    let clock = Arc::new(ManualClock::new(Utc::now()));
    let env = start_with_clock(clock.clone());
    env.seed("alice", MAP, "map one", None);
    let ws = clone_with(&env, "ws", &["--local-store"]);
    env.ok(&ws, "alice", &["lock", MAP]);
    write(&ws, MAP, "map two");
    env.ok(&ws, "alice", &["add", MAP]);
    env.ok(&ws, "alice", &["commit", "-m", "map"]);

    clock.advance(Duration::hours(9));
    let (code, err) = refused(&env, &ws, "alice", &["push"]);
    assert_eq!(code, 128);
    assert!(err.contains("(lock required)"), "{err}");
    assert!(err.contains("Content/m.umap"), "{err}");
    assert!(err.contains("hint: Run `pyn lock <path>`"), "{err}");
    assert!(err.contains("nothing was pushed"), "{err}");
    assert_eq!(env.ok(&env.dir("home"), "alice", &["get", MAP]), "map one");

    env.ok(&ws, "alice", &["lock", MAP]);
    env.ok(&ws, "alice", &["push"]);
    assert_eq!(env.ok(&ws, "alice", &["get", MAP]), "map two");
}

#[test]
fn push_while_another_user_holds_the_lock_is_rejected_with_lock_held() {
    let clock = Arc::new(ManualClock::new(Utc::now()));
    let env = start_with_clock(clock.clone());
    env.seed("alice", MAP, "map one", None);
    let ws = clone_with(&env, "ws", &["--local-store"]);
    env.ok(&ws, "alice", &["lock", MAP]);
    write(&ws, MAP, "map two");
    env.ok(&ws, "alice", &["add", MAP]);
    env.ok(&ws, "alice", &["commit", "-m", "map"]);

    clock.advance(Duration::hours(9));
    env.ok(&ws, "bob", &["lock", MAP]);
    let (code, err) = refused(&env, &ws, "alice", &["push"]);
    assert_eq!(code, 128);
    assert!(err.contains("(locked by someone else)"), "{err}");
    assert!(err.contains("Content/m.umap is locked by bob"), "{err}");
    assert!(err.contains("nothing was pushed"), "{err}");
    assert_eq!(env.ok(&ws, "bob", &["get", MAP]), "map one");
}

#[test]
fn push_rejected_as_non_fast_forward_still_exits_1() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "one", None);
    let ws = clone_with(&env, "ws", &["--local-store"]);
    let other = clone_with(&env, "other", &["--local-store"]);
    for (dir, text) in [(&other, "theirs"), (&ws, "mine")] {
        write(dir, "Source/b.cpp", text);
        env.ok(dir, "alice", &["add", "Source/b.cpp"]);
        env.ok(dir, "alice", &["commit", "-m", text]);
    }
    env.ok(&other, "alice", &["push"]);
    let (code, err) = refused(&env, &ws, "alice", &["push"]);
    assert_eq!(code, 1);
    assert!(err.contains("(non-fast-forward)"), "{err}");
}

#[test]
fn restore_of_an_exclusive_path_needs_the_lock() {
    let (env, ws) = edited_unlocked();
    let (code, err) = refused(&env, &ws, "alice", &["restore", MAP, "1", "--yes"]);
    assert_eq!(code, 128);
    assert!(
        err.contains("lock_required (409)") && err.contains("lock it before checking in"),
        "{err}"
    );

    env.ok(&ws, "alice", &["lock", MAP]);
    env.ok(&ws, "alice", &["restore", MAP, "1", "--yes"]);
    assert_eq!(env.ok(&ws, "alice", &["get", MAP]), "map one");
}

#[test]
fn restore_of_a_path_locked_by_another_user_names_the_holder() {
    let (env, ws) = edited_unlocked();
    env.ok(&ws, "bob", &["lock", MAP]);
    let (code, err) = refused(&env, &ws, "alice", &["restore", MAP, "1", "--yes"]);
    assert_eq!(code, 128);
    assert!(
        err.contains("lock_held (409)")
            && err.contains("Content/m.umap is locked by bob until")
            && err.contains("Ask bob to unlock it"),
        "{err}"
    );
    assert_eq!(env.ok(&ws, "alice", &["get", MAP]), "map two");
}

#[test]
fn restore_staged_needs_no_lock_and_keeps_the_file() {
    let env = start();
    env.seed("alice", MAP, "map one", None);
    let ws = clone_with(&env, "ws", &["--no-local-store"]);
    env.ok(&ws, "alice", &["lock", MAP]);
    write(&ws, MAP, "map two");
    env.ok(&ws, "alice", &["add", MAP]);
    env.ok(&ws, "alice", &["unlock", MAP]);

    env.ok(&ws, "alice", &["restore", "--staged", MAP]);
    let status = env.ok(&ws, "alice", &["status"]);
    assert!(status.contains("0 staged, 1 modified"), "{status}");
    assert_eq!(read(&ws, MAP), "map two");

    env.ok(&ws, "bob", &["lock", MAP]);
    env.ok(&ws, "alice", &["restore", "--staged", MAP]);
    assert_eq!(read(&ws, MAP), "map two");
}

/// Branch `topic` carries a different version of the map; the workspace is back on `main` with the map edited.
fn topic_with_a_different_map() -> (Env, PathBuf) {
    let env = start();
    env.seed("alice", MAP, "map one", None);
    let ws = clone_with(&env, "ws", &["--no-local-store"]);
    env.ok(&ws, "alice", &["branch", "topic"]);
    env.ok(&ws, "alice", &["switch", "topic"]);
    env.ok(&ws, "alice", &["lock", MAP]);
    write(&ws, MAP, "topic map");
    env.ok(&ws, "alice", &["add", MAP]);
    env.ok(&ws, "alice", &["commit", "-m", "topic map"]);
    env.ok(&ws, "alice", &["switch", "main"]);
    (env, ws)
}

#[test]
fn switch_over_an_edited_unlocked_exclusive_file_is_refused_until_it_is_put_back() {
    let (env, ws) = topic_with_a_different_map();
    write(&ws, MAP, "map edited");
    let (code, err) = refused(&env, &ws, "alice", &["switch", "topic"]);
    assert_eq!(code, 128);
    assert!(
        err.contains("local changes would be overwritten by the switch")
            && err.contains("Content/m.umap")
            && err.contains("nothing was changed"),
        "{err}"
    );
    assert_eq!(read(&ws, MAP), "map edited");

    env.ok(&ws, "alice", &["checkout", "--", MAP]);
    assert_eq!(read(&ws, MAP), "map one");
    assert!(read_only(&ws, MAP));
    env.ok(&ws, "alice", &["switch", "topic"]);
    assert_eq!(read(&ws, MAP), "topic map");
}

#[test]
fn switch_over_an_edit_made_under_the_lock_is_refused_and_keeps_the_lock() {
    let (env, ws) = topic_with_a_different_map();
    env.ok(&ws, "alice", &["lock", "--base", "1", MAP]);
    write(&ws, MAP, "map edited");
    let (code, err) = refused(&env, &ws, "alice", &["switch", "topic"]);
    assert_eq!(code, 128);
    assert!(err.contains("Content/m.umap"), "{err}");
    assert_eq!(read(&ws, MAP), "map edited");
    let locks = env.ok(&ws, "alice", &["locks", "--mine"]);
    assert!(locks.contains(MAP), "{locks}");
}

#[test]
fn switch_over_a_file_locked_by_another_user_still_writes_it_read_only() {
    let (env, ws) = topic_with_a_different_map();
    env.ok(&ws, "bob", &["lock", MAP]);
    env.ok(&ws, "alice", &["switch", "topic"]);
    assert_eq!(read(&ws, MAP), "topic map");
    assert!(read_only(&ws, MAP));
}

#[test]
fn checkout_dash_dash_discards_an_unlocked_edit_without_a_lock() {
    let (env, ws) = edited_unlocked();
    env.ok(&ws, "alice", &["checkout", "--", MAP]);
    assert_eq!(read(&ws, MAP), "map two");
    assert!(read_only(&ws, MAP));
    let status = env.ok(&ws, "alice", &["status"]);
    assert!(status.contains("everything is up to date"), "{status}");
}

#[test]
fn checkout_dash_dash_of_a_path_locked_by_another_user_restores_it_read_only() {
    let (env, ws) = edited_unlocked();
    env.ok(&ws, "bob", &["lock", MAP]);
    env.ok(&ws, "alice", &["checkout", "--", MAP]);
    assert_eq!(read(&ws, MAP), "map two");
    assert!(read_only(&ws, MAP));
}

#[test]
fn merge_over_an_edited_unlocked_exclusive_file_is_refused_until_it_is_put_back() {
    let (env, ws) = topic_with_a_different_map();
    write(&ws, MAP, "map edited");
    let (code, err) = refused(&env, &ws, "alice", &["merge", "topic"]);
    assert_eq!(code, 128);
    assert!(
        err.contains("your local changes to the following files would be overwritten by merge")
            && err.contains("Content/m.umap")
            && err.contains("nothing was changed"),
        "{err}"
    );
    assert_eq!(read(&ws, MAP), "map edited");

    env.ok(&ws, "alice", &["checkout", "--", MAP]);
    env.ok(&ws, "alice", &["merge", "topic"]);
    assert_eq!(read(&ws, MAP), "topic map");
}

#[test]
fn merge_over_a_file_locked_by_another_user_brings_the_later_revision_in_read_only() {
    let (env, ws) = topic_with_a_different_map();
    env.ok(&ws, "bob", &["lock", MAP]);
    env.ok(&ws, "alice", &["merge", "topic"]);
    assert_eq!(read(&ws, MAP), "topic map");
    assert!(read_only(&ws, MAP));
}
