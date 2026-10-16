//! `add`, `commit`, `status` and `branch` against the local store (`local_store`): a commit is recorded locally in git's
//! encoding and not published. Real git reads the store as an oracle for semantics only.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::{Env, REPO, start};

fn clone_with(env: &Env, name: &str, extra: &[&str]) -> PathBuf {
    let ws = env.dir(name);
    let source = env.source(REPO);
    let mut args = vec!["clone"];
    args.extend(extra);
    args.extend([source.as_str(), ws.to_str().unwrap()]);
    env.ok(&env.dir("home"), "alice", &args);
    ws.canonicalize().unwrap()
}

fn store(ws: &Path) -> PathBuf {
    ws.join(".pyn/local_only/store")
}

fn git(ws: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("--git-dir")
        .arg(store(ws))
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn seeded() -> (Env, PathBuf) {
    let env = start();
    env.seed("alice", "Source/a.cpp", "one", None);
    env.seed("alice", "Content/m.umap", "map", None);
    let ws = clone_with(&env, "ws", &["--local-store"]);
    (env, ws)
}

fn server_commits(env: &Env, ws: &Path) -> usize {
    env.ok(ws, "alice", &["log", "--commits"]).lines().count()
}

#[test]
fn a_shared_path_commits_locally_and_publishes_nothing() {
    let (env, ws) = seeded();
    let before = server_commits(&env, &ws);
    let tip = git(&ws, &["rev-parse", "main"]);
    std::fs::write(ws.join("Source/a.cpp"), "two").unwrap();
    std::fs::write(ws.join("Source/new.cpp"), "fresh").unwrap();
    env.ok(&ws, "alice", &["add", "Source"]);
    let out = env.ok(&ws, "alice", &["commit", "-m", "local work"]);
    assert!(
        out.contains("local and not yet published"),
        "the output must say the commit is local: {out}"
    );
    assert!(!out.contains("revision"), "{out}");

    assert_eq!(server_commits(&env, &ws), before, "the server is untouched");
    assert_eq!(
        git(&ws, &["rev-parse", "refs/remotes/origin/main"]),
        tip,
        "the server's tip is as it was"
    );
    let local = git(&ws, &["rev-parse", "main"]);
    assert_ne!(local, tip);
    assert_eq!(git(&ws, &["rev-parse", "main~1"]), tip);
    assert_eq!(
        git(&ws, &["log", "-1", "--format=%s", "main"]),
        "local work\n"
    );
    assert!(git(&ws, &["log", "-1", "--format=%an <%ae>", "main"]).starts_with("alice <alice@"));
    assert_eq!(
        git(&ws, &["diff-tree", "-r", "--name-status", "main~1", "main"]),
        "M\tSource/a.cpp\nA\tSource/new.cpp\n"
    );
    assert_eq!(git(&ws, &["cat-file", "blob", "main:Source/a.cpp"]), "two");
    assert_eq!(
        git(&ws, &["cat-file", "blob", "main:Source/new.cpp"]),
        "fresh"
    );
    git(&ws, &["fsck", "--strict"]);
}

#[test]
fn a_committed_workspace_is_clean_and_status_reports_the_local_commit() {
    let (env, ws) = seeded();
    std::fs::write(ws.join("Source/a.cpp"), "two").unwrap();
    env.ok(&ws, "alice", &["add", "Source/a.cpp"]);
    env.ok(&ws, "alice", &["commit", "-m", "x"]);
    let status = env.ok(&ws, "alice", &["status"]);
    assert!(status.contains("modified in a local commit"), "{status}");
    assert!(
        status.contains("1 local commit(s) not yet published"),
        "{status}"
    );
    assert!(!status.contains("staged\n") && status.contains("0 staged, 0 modified"));
    let again = env.fails(&ws, "alice", &["commit", "-m", "again"]);
    assert!(again.contains("nothing to commit"), "{again}");
}

#[test]
fn a_second_local_commit_builds_on_the_first() {
    let (env, ws) = seeded();
    for (text, message) in [("two", "first"), ("three", "second")] {
        std::fs::write(ws.join("Source/a.cpp"), text).unwrap();
        env.ok(&ws, "alice", &["add", "Source/a.cpp"]);
        env.ok(&ws, "alice", &["commit", "-m", message]);
    }
    assert_eq!(
        git(&ws, &["rev-list", "--count", "origin/main..main"]),
        "2\n"
    );
    assert_eq!(
        git(&ws, &["cat-file", "blob", "main:Source/a.cpp"]),
        "three"
    );
    let status = env.ok(&ws, "alice", &["status"]);
    assert!(status.contains("2 local commit(s)"), "{status}");
}

#[test]
fn a_deletion_is_committed_locally_and_update_leaves_it_alone() {
    let (env, ws) = seeded();
    std::fs::remove_file(ws.join("Source/a.cpp")).unwrap();
    env.ok(&ws, "alice", &["add", "Source/a.cpp"]);
    env.ok(&ws, "alice", &["commit", "-m", "drop"]);
    assert!(!git(&ws, &["ls-tree", "-r", "--name-only", "main"]).contains("a.cpp"));
    let status = env.ok(&ws, "alice", &["status"]);
    assert!(status.contains("deleted in a local commit"), "{status}");
    let update = env.ok(&ws, "alice", &["update"]);
    assert!(
        update.contains("committed locally and not yet published"),
        "{update}"
    );
    assert!(!ws.join("Source/a.cpp").exists());
}

#[test]
fn update_keeps_local_commits_and_their_files_when_the_server_moves() {
    let (env, ws) = seeded();
    std::fs::write(ws.join("Source/a.cpp"), "mine").unwrap();
    env.ok(&ws, "alice", &["add", "Source/a.cpp"]);
    env.ok(&ws, "alice", &["commit", "-m", "mine"]);
    let local = git(&ws, &["rev-parse", "main"]);
    env.seed("alice", "Source/a.cpp", "theirs", Some(1));
    env.seed("alice", "Source/b.cpp", "bee", None);
    let out = env.ok(&ws, "alice", &["update"]);
    assert!(out.contains("Source/a.cpp: committed locally"), "{out}");
    assert_eq!(
        std::fs::read_to_string(ws.join("Source/a.cpp")).unwrap(),
        "mine"
    );
    assert_eq!(
        std::fs::read_to_string(ws.join("Source/b.cpp")).unwrap(),
        "bee"
    );
    assert_eq!(
        git(&ws, &["rev-parse", "main"]),
        local,
        "the local branch did not move"
    );
    assert_ne!(git(&ws, &["rev-parse", "origin/main"]), local);
}

#[test]
fn an_exclusive_path_needs_the_lock_at_commit() {
    let (env, ws) = seeded();
    env.ok(&ws, "alice", &["lock", "Content/m.umap"]);
    std::fs::write(ws.join("Content/m.umap"), "map2").unwrap();
    env.ok(&ws, "alice", &["add", "Content/m.umap"]);
    env.ok(&ws, "alice", &["unlock", "Content/m.umap"]);
    let err = env.fails(&ws, "alice", &["commit", "-m", "map"]);
    assert!(err.contains("exclusive") && err.contains("lock"), "{err}");
    assert_eq!(
        git(&ws, &["rev-list", "--count", "origin/main..main"]),
        "0\n"
    );

    env.ok(&ws, "alice", &["lock", "Content/m.umap"]);
    let out = env.ok(&ws, "alice", &["commit", "-m", "map"]);
    assert!(out.contains("not yet published"), "{out}");
    assert_eq!(
        git(&ws, &["cat-file", "blob", "main:Content/m.umap"]),
        "map2"
    );
}

#[test]
fn an_unlocked_exclusive_path_is_refused_at_add() {
    let (env, ws) = seeded();
    env.ok(&ws, "alice", &["lock", "Content/m.umap"]);
    std::fs::write(ws.join("Content/m.umap"), "map2").unwrap();
    env.ok(&ws, "alice", &["unlock", "Content/m.umap"]);
    let err = env.fails(&ws, "alice", &["add", "Content/m.umap"]);
    assert!(err.contains("exclusive"), "{err}");
}

#[test]
fn without_the_setting_commit_publishes_as_before() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "one", None);
    let ws = clone_with(&env, "ws", &[]);
    std::fs::write(ws.join("Source/a.cpp"), "two").unwrap();
    env.ok(&ws, "alice", &["add", "Source/a.cpp"]);
    let out = env.ok(&ws, "alice", &["commit", "-m", "direct"]);
    assert!(out.contains("is now at revision 2"), "{out}");
    assert!(!store(&ws).exists());
    let status = env.ok(&ws, "alice", &["status"]);
    assert!(!status.contains("local commit"), "{status}");
}

#[test]
fn a_direct_commit_still_publishes_with_the_setting_on() {
    let (env, ws) = seeded();
    let file = env.dir("up").join("f");
    std::fs::write(&file, "direct").unwrap();
    let out = env.ok(
        &ws,
        "alice",
        &[
            "commit",
            "Source/a.cpp",
            "--file",
            file.to_str().unwrap(),
            "-m",
            "d",
        ],
    );
    assert!(out.contains("is now at revision 2"), "{out}");
}

#[test]
fn branch_verbose_shows_the_local_tip_of_the_current_branch() {
    let (env, ws) = seeded();
    let plain = env.ok(&ws, "alice", &["branch", "-v"]);
    assert!(!plain.contains("local,"), "{plain}");
    std::fs::write(ws.join("Source/a.cpp"), "two").unwrap();
    env.ok(&ws, "alice", &["add", "Source/a.cpp"]);
    env.ok(&ws, "alice", &["commit", "-m", "local work"]);
    let out = env.ok(&ws, "alice", &["branch", "-v"]);
    let tip = git(&ws, &["rev-parse", "main"]);
    assert!(out.contains(&tip[..10]), "{out}");
    assert!(
        out.contains("local work (local, 1 not yet published)"),
        "{out}"
    );
}

#[test]
fn switching_away_from_unpublished_commits_is_refused() {
    let (env, ws) = seeded();
    env.ok(&ws, "alice", &["branch", "other"]);
    std::fs::write(ws.join("Source/a.cpp"), "two").unwrap();
    env.ok(&ws, "alice", &["add", "Source/a.cpp"]);
    env.ok(&ws, "alice", &["commit", "-m", "local work"]);
    let err = env.fails(&ws, "alice", &["switch", "other"]);
    assert!(err.contains("not yet published"), "{err}");
}
