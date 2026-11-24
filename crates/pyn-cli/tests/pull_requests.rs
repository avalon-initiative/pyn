//! `pyn pr` on a pyn server.

mod common;

use std::path::PathBuf;

use common::{Env, REPO, cell_rows, start};

fn clone(env: &Env, name: &str) -> PathBuf {
    let ws = env.dir(name);
    env.ok(
        &env.dir("home"),
        "alice",
        &[
            "clone",
            "--no-local-store",
            &env.source(REPO),
            ws.to_str().unwrap(),
        ],
    );
    ws.canonicalize().unwrap()
}

#[test]
fn a_pull_request_is_opened_listed_discussed_and_merged() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "one", None);
    let ws = clone(&env, "ws");
    env.ok(&ws, "alice", &["switch", "-c", "feature"]);
    std::fs::write(ws.join("Source/b.cpp"), "new").unwrap();
    env.ok(&ws, "alice", &["commit", "Source/b.cpp", "-m", "add b"]);

    let out = env.ok(
        &ws,
        "alice",
        &["pr", "create", "--title", "Add b", "--body", "why not"],
    );
    assert_eq!(out.trim(), "opened #1 feature -> main");
    let err = env.fails(&ws, "alice", &["pr", "create", "--title", "Again"]);
    assert!(err.contains("pull_request_exists"), "{err}");

    let listed = env.ok(&ws, "alice", &["pr", "list"]);
    let rows = cell_rows(&listed);
    assert_eq!(
        rows[0],
        ["NUMBER", "STATE", "AUTHOR", "COMMENTS", "BRANCHES", "TITLE"]
    );
    assert_eq!(
        rows[1],
        ["#1", "open", "alice", "0", "feature -> main", "Add b"]
    );

    env.ok(&ws, "bob", &["pr", "comment", "1", "--body", "looks good"]);
    let shown = env.ok(&ws, "alice", &["pr", "show", "1", "--commits"]);
    assert!(shown.contains("#1 Add b"), "{shown}");
    assert!(
        shown.contains("open by alice: feature into main"),
        "{shown}"
    );
    assert!(
        shown.contains("1 commits, 1 files changed, +1 -0"),
        "{shown}"
    );
    assert!(shown.contains("can be merged by fast-forward"), "{shown}");
    assert!(shown.contains("why not"), "{shown}");
    assert!(shown.contains("add b"), "{shown}");
    assert!(
        shown.contains("bob commented") && shown.contains("looks good"),
        "{shown}"
    );
    assert_eq!(cell_rows(&env.ok(&ws, "alice", &["pr", "list"]))[1][3], "1");

    env.ok(&ws, "alice", &["pr", "close", "1"]);
    assert_eq!(
        env.ok(&ws, "alice", &["pr", "list"]).trim(),
        "no pull requests"
    );
    let closed = env.ok(&ws, "alice", &["pr", "list", "--state", "closed"]);
    assert_eq!(cell_rows(&closed)[1][1], "closed");
    env.ok(&ws, "alice", &["pr", "reopen", "1"]);

    let merged = env.ok(&ws, "alice", &["pr", "merge", "1"]);
    assert!(merged.starts_with("merged #1 into main as "), "{merged}");
    let main = clone(&env, "main-ws");
    assert!(main.join("Source/b.cpp").exists());
    let again = env.fails(&ws, "alice", &["pr", "merge", "1"]);
    assert!(again.contains("pull_request_not_open"), "{again}");
    let all = env.ok(&ws, "alice", &["pr", "list", "--state", "all"]);
    assert_eq!(cell_rows(&all)[1][1], "merged");
    let err = env.fails(&ws, "alice", &["pr", "list", "--state", "weird"]);
    assert!(err.contains("unknown state"), "{err}");
}
