//! Offline work on exclusive paths (decision #1062): `add`, `commit` and `edit` need no lock, `status` marks the gap and
//! `push` refuses before sending until the pusher holds the lock. Hint wording is asserted only for its key facts.

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

fn writable(ws: &Path, path: &str) -> bool {
    std::fs::metadata(ws.join(path))
        .unwrap()
        .permissions()
        .mode()
        & 0o222
        != 0
}

fn out_err(out: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// A local-store workspace with the map edited offline (through `pyn edit`) and committed without a lock.
fn committed_offline(env: &Env) -> PathBuf {
    env.seed("alice", MAP, "map one", None);
    let ws = clone_with(env, "ws", &["--local-store"]);
    assert!(!writable(&ws, MAP));
    env.ok(&ws, "alice", &["edit", MAP]);
    std::fs::write(ws.join(MAP), "map two").unwrap();
    env.ok(&ws, "alice", &["add", MAP]);
    env.ok(&ws, "alice", &["commit", "-m", "offline"]);
    ws
}

fn server_copy(env: &Env) -> String {
    env.ok(&env.dir("home"), "alice", &["get", MAP])
}

#[test]
fn edit_makes_the_file_writable_without_a_lock_and_says_the_lock_is_needed() {
    let env = start();
    env.seed("alice", MAP, "map one", None);
    let ws = clone_with(&env, "ws", &["--local-store"]);
    let out = env.run(&ws, "alice", &["edit", MAP]);
    assert!(out.status.success());
    let text = out_err(&out);
    assert!(text.contains(MAP) && text.contains("pyn lock"), "{text}");
    assert!(writable(&ws, MAP));
    assert!(
        env.ok(&ws, "alice", &["locks"]).contains("no locks")
            || !env.ok(&ws, "alice", &["locks"]).contains(MAP)
    );
}

#[test]
fn edit_needs_no_server() {
    let env = start();
    env.seed("alice", MAP, "map one", None);
    let ws = clone_with(&env, "ws", &["--local-store"]);
    let out = env.run_with_vars(
        &ws,
        "alice",
        &["edit", MAP],
        "UTC",
        &[("PYN_SERVER", "http://127.0.0.1:1")],
    );
    assert!(out.status.success(), "{}", out_err(&out));
    assert!(writable(&ws, MAP));
}

#[test]
fn edit_of_an_untracked_path_fails() {
    let env = start();
    let ws = clone_with(&env, "ws", &["--local-store"]);
    let out = env.run(&ws, "alice", &["edit", "nope.txt"]);
    assert!(!out.status.success());
}

#[test]
fn add_and_commit_accept_an_exclusive_path_without_the_lock_and_say_so() {
    let env = start();
    env.seed("alice", MAP, "map one", None);
    let ws = clone_with(&env, "ws", &["--local-store"]);
    env.ok(&ws, "alice", &["edit", MAP]);
    std::fs::write(ws.join(MAP), "map two").unwrap();
    let add = env.run(&ws, "alice", &["add", MAP]);
    assert!(add.status.success());
    let text = out_err(&add);
    assert!(
        text.contains(MAP) && text.contains(&format!("pyn lock {MAP}")),
        "{text}"
    );
    let commit = env.run(&ws, "alice", &["commit", "-m", "offline"]);
    assert!(commit.status.success(), "{}", out_err(&commit));
    let text = out_err(&commit);
    assert!(text.contains(MAP) && text.contains("pyn lock"), "{text}");
    assert_eq!(server_copy(&env), "map one");
}

#[test]
fn commit_dash_a_commits_an_unlocked_exclusive_file_offline() {
    let env = start();
    env.seed("alice", MAP, "map one", None);
    let ws = clone_with(&env, "ws", &["--local-store"]);
    env.ok(&ws, "alice", &["edit", MAP]);
    std::fs::write(ws.join(MAP), "map two").unwrap();
    env.ok(&ws, "alice", &["commit", "-a", "-m", "offline"]);
    assert_eq!(server_copy(&env), "map one");
}

#[test]
fn add_without_the_local_store_stages_but_the_commit_is_still_a_publish() {
    let env = start();
    env.seed("alice", MAP, "map one", None);
    let ws = clone_with(&env, "ws", &["--no-local-store"]);
    env.ok(&ws, "alice", &["edit", MAP]);
    std::fs::write(ws.join(MAP), "map two").unwrap();
    env.ok(&ws, "alice", &["add", MAP]);
    let err = env.fails(&ws, "alice", &["commit", "-m", "x"]);
    assert!(err.contains("lock_required"), "{err}");
    assert_eq!(server_copy(&env), "map one");
}

#[test]
fn push_without_the_lock_is_refused_before_sending_with_the_hint() {
    let env = start();
    let ws = committed_offline(&env);
    let out = env.run(&ws, "alice", &["push"]);
    assert_eq!(out.status.code(), Some(128));
    let text = out_err(&out);
    assert!(text.contains(MAP) && text.contains("pyn lock"), "{text}");
    assert!(text.contains("nothing was pushed"), "{text}");
    assert_eq!(server_copy(&env), "map one");
}

#[test]
fn push_while_another_user_holds_the_lock_is_refused_naming_the_holder() {
    let env = start();
    let ws = committed_offline(&env);
    env.ok(&ws, "bob", &["lock", MAP]);
    let out = env.run(&ws, "alice", &["push"]);
    assert_eq!(out.status.code(), Some(128));
    let text = out_err(&out);
    assert!(text.contains(MAP) && text.contains("bob"), "{text}");
    assert_eq!(server_copy(&env), "map one");
}

#[test]
fn push_succeeds_after_taking_the_lock() {
    let env = start();
    let ws = committed_offline(&env);
    assert_eq!(env.run(&ws, "alice", &["push"]).status.code(), Some(128));
    env.ok(&ws, "alice", &["lock", MAP]);
    env.ok(&ws, "alice", &["push"]);
    assert_eq!(server_copy(&env), "map two");
}

#[test]
fn a_lapsed_lease_is_refused_at_push_and_the_new_lock_lets_it_through() {
    let clock = Arc::new(ManualClock::new(Utc::now()));
    let env = start_with_clock(clock.clone());
    env.seed("alice", MAP, "map one", None);
    let ws = clone_with(&env, "ws", &["--local-store"]);
    env.ok(&ws, "alice", &["lock", MAP]);
    std::fs::write(ws.join(MAP), "map two").unwrap();
    env.ok(&ws, "alice", &["add", MAP]);
    env.ok(&ws, "alice", &["commit", "-m", "map"]);
    clock.advance(Duration::hours(9));
    let out = env.run(&ws, "alice", &["push"]);
    assert_eq!(out.status.code(), Some(128));
    assert!(out_err(&out).contains("pyn lock"), "{}", out_err(&out));
    env.ok(&ws, "alice", &["lock", MAP]);
    env.ok(&ws, "alice", &["push"]);
    assert_eq!(server_copy(&env), "map two");
}

#[test]
fn a_push_of_shared_files_needs_no_lock() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "one", None);
    let ws = clone_with(&env, "ws", &["--local-store"]);
    std::fs::write(ws.join("Source/a.cpp"), "two").unwrap();
    env.ok(&ws, "alice", &["commit", "-a", "-m", "shared"]);
    env.ok(&ws, "alice", &["push"]);
}

#[test]
fn status_marks_an_exclusive_file_changed_without_the_lock() {
    let env = start();
    env.seed("alice", MAP, "map one", None);
    let ws = clone_with(&env, "ws", &["--local-store"]);
    env.ok(&ws, "alice", &["edit", MAP]);
    std::fs::write(ws.join(MAP), "map two").unwrap();

    let table = env.ok(&ws, "alice", &["status"]);
    assert!(
        table.contains("needs lock") && table.contains(MAP),
        "{table}"
    );

    let json: serde_json::Value =
        serde_json::from_str(env.ok(&ws, "alice", &["--json", "status"]).trim()).unwrap();
    let file = &json["files"][0];
    assert_eq!(file["path"], MAP);
    assert!(
        file["states"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s == "needs_lock")
    );
    assert_eq!(json["counts"]["needs_lock"], 1);
    assert_eq!(json["counts"]["blocked_by_lock"], 0);

    env.ok(&ws, "bob", &["lock", MAP]);
    let json: serde_json::Value =
        serde_json::from_str(env.ok(&ws, "alice", &["--json", "status"]).trim()).unwrap();
    let file = &json["files"][0];
    assert!(
        file["states"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s == "blocked_by_lock")
    );
    assert_eq!(json["counts"]["blocked_by_lock"], 1);
    assert_eq!(json["counts"]["needs_lock"], 0);
    let table = env.ok(&ws, "alice", &["status"]);
    assert!(
        table.contains("bob") && table.contains("push blocked"),
        "{table}"
    );
}

#[test]
fn status_is_quiet_about_a_file_changed_under_my_lock() {
    let env = start();
    env.seed("alice", MAP, "map one", None);
    let ws = clone_with(&env, "ws", &["--local-store"]);
    env.ok(&ws, "alice", &["lock", MAP]);
    std::fs::write(ws.join(MAP), "map two").unwrap();
    let json: serde_json::Value =
        serde_json::from_str(env.ok(&ws, "alice", &["--json", "status"]).trim()).unwrap();
    let states = json["files"][0]["states"].as_array().unwrap().clone();
    assert!(
        !states
            .iter()
            .any(|s| s == "needs_lock" || s == "blocked_by_lock")
    );
    assert_eq!(json["counts"]["needs_lock"], 0);
}
