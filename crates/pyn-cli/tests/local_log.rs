//! `pyn log` on a pyn home with a local store lists the unpublished commits, marked `local`, above the server's revisions.

mod common;

use std::path::PathBuf;

use common::{Env, REPO, start};

fn seeded() -> (Env, PathBuf) {
    let env = start();
    env.seed("alice", "Source/a.cpp", "one", None);
    env.seed("alice", "Content/m.umap", "map", None);
    let ws = env.dir("ws");
    let source = env.source(REPO);
    env.ok(
        &env.dir("home"),
        "alice",
        &[
            "clone",
            "--local-store",
            source.as_str(),
            ws.to_str().unwrap(),
        ],
    );
    (env, ws.canonicalize().unwrap())
}

fn commit(env: &Env, ws: &std::path::Path, path: &str, text: &str, message: &str) {
    std::fs::write(ws.join(path), text).unwrap();
    env.ok(ws, "alice", &["add", path]);
    env.ok(ws, "alice", &["commit", "-m", message]);
}

fn line_of(out: &str, needle: &str) -> usize {
    out.lines()
        .position(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("{needle} missing from\n{out}"))
}

#[test]
fn log_lists_local_commits_newest_first_above_the_server_revisions() {
    let (env, ws) = seeded();
    commit(&env, &ws, "Source/a.cpp", "two", "first local");
    commit(&env, &ws, "Source/b.cpp", "bee", "second local");
    let log = env.ok(&ws, "alice", &["log"]);
    let (second, first) = (line_of(&log, "second local"), line_of(&log, "first local"));
    assert!(second < first, "{log}");
    assert!(
        log.lines().nth(second).unwrap().starts_with("local"),
        "{log}"
    );
    assert!(
        log.lines().nth(second).unwrap().ends_with("Source/b.cpp"),
        "{log}"
    );
    assert!(log.lines().any(|l| l.starts_with("r1")), "{log}");
    assert!(
        first < log.lines().position(|l| l.starts_with("r1")).unwrap(),
        "{log}"
    );
}

#[test]
fn log_honours_limit_filter_and_path() {
    let (env, ws) = seeded();
    commit(&env, &ws, "Source/a.cpp", "two", "first local");
    commit(&env, &ws, "Source/b.cpp", "bee", "second local");

    let one = env.ok(&ws, "alice", &["log", "-n", "1"]);
    assert!(
        one.contains("second local") && !one.contains("first local"),
        "{one}"
    );
    assert!(!one.lines().any(|l| l.starts_with('r')), "{one}");

    let filtered = env.ok(&ws, "alice", &["log", "--filter", "b.cpp"]);
    assert!(
        filtered.contains("second local") && !filtered.contains("first local"),
        "{filtered}"
    );

    let path = env.ok(&ws, "alice", &["log", "Source/a.cpp"]);
    let local = line_of(&path, "first local");
    let server = path.lines().position(|l| l.starts_with("r1")).unwrap();
    assert!(local < server && !path.contains("second local"), "{path}");
}

#[test]
fn a_local_deletion_is_marked_and_published_commits_are_not_local() {
    let (env, ws) = seeded();
    env.ok(&ws, "alice", &["rm", "Source/a.cpp"]);
    env.ok(&ws, "alice", &["commit", "-m", "drop it"]);
    let log = env.ok(&ws, "alice", &["log"]);
    assert!(log.contains("drop it (deleted)"), "{log}");
    env.ok(&ws, "alice", &["push"]);
    let after = env.ok(&ws, "alice", &["log"]);
    assert!(!after.lines().any(|l| l.starts_with("local")), "{after}");
    assert!(after.contains("drop it"), "{after}");
}

#[test]
fn log_commits_marks_local_commits() {
    let (env, ws) = seeded();
    commit(&env, &ws, "Source/a.cpp", "two", "first local");
    let log = env.ok(&ws, "alice", &["log", "--commits"]);
    assert!(log.lines().nth(1).unwrap().contains("(local)"), "{log}");
    assert_eq!(log.matches("(local)").count(), 1, "{log}");
}
