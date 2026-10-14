//! `pyn branch`, `pyn switch` and `pyn checkout` on a pyn server, and the workspace following its branch.

mod common;

use std::path::{Path, PathBuf};

use common::{Env, REPO, start};

fn clone(env: &Env, name: &str) -> PathBuf {
    let ws = env.dir(name);
    env.ok(
        &env.dir("home"),
        "alice",
        &["clone", &env.source(REPO), ws.to_str().unwrap()],
    );
    ws.canonicalize().unwrap()
}

fn read(ws: &Path, path: &str) -> String {
    std::fs::read_to_string(ws.join(path)).unwrap()
}

/// What the command printed to standard error, whether or not it succeeded.
fn stderr(env: &Env, cwd: &Path, args: &[&str]) -> String {
    String::from_utf8_lossy(&env.run(cwd, "alice", args).stderr).into_owned()
}

#[test]
fn branches_are_listed_created_renamed_and_deleted() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "one", None);
    let ws = clone(&env, "ws");

    assert_eq!(env.ok(&ws, "alice", &["branch"]), "* main\n");
    assert_eq!(
        env.ok(&ws, "alice", &["branch", "--show-current"]),
        "main\n"
    );
    env.ok(&ws, "alice", &["branch", "topic/one"]);
    env.ok(&ws, "alice", &["branch", "zeta", "main"]);
    assert_eq!(
        env.ok(&ws, "alice", &["branch"]),
        "* main\n  topic/one\n  zeta\n"
    );
    let verbose = env.ok(&ws, "alice", &["branch", "-v"]);
    let first = verbose.lines().next().unwrap();
    assert!(first.starts_with("* main"), "{verbose}");
    assert!(first.ends_with("  seed"), "{verbose}");

    let err = env.fails(&ws, "alice", &["branch", "topic"]);
    assert!(err.contains("branch_exists"), "{err}");
    let err = env.fails(&ws, "alice", &["branch", "bad name"]);
    assert!(err.contains("invalid_branch_name"), "{err}");

    env.ok(&ws, "alice", &["branch", "-m", "zeta", "omega"]);
    assert!(env.ok(&ws, "alice", &["branch"]).contains("  omega\n"));
    let deleted = env.ok(&ws, "alice", &["branch", "-d", "topic/one"]);
    assert!(
        deleted.starts_with("Deleted branch topic/one (was "),
        "{deleted}"
    );
    let err = env.fails(&ws, "alice", &["branch", "-d", "nope"]);
    assert!(err.contains("branch 'nope' not found"), "{err}");
    let err = env.fails(&ws, "alice", &["branch", "-d", "main"]);
    assert!(err.contains("this workspace is on it"), "{err}");
}

#[test]
fn switching_brings_the_files_of_the_branch_and_commits_land_on_it() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "one", None);
    let ws = clone(&env, "ws");

    let err = stderr(&env, &ws, &["switch", "-c", "feature"]);
    assert_eq!(err.trim(), "Switched to branch 'feature'");
    assert_eq!(
        env.ok(&ws, "alice", &["branch", "--show-current"]),
        "feature\n"
    );
    assert!(
        env.ok(&ws, "alice", &["status"])
            .starts_with("On branch feature\n")
    );

    std::fs::write(ws.join("Source/a.cpp"), "two").unwrap();
    std::fs::write(ws.join("Source/b.cpp"), "new").unwrap();
    env.ok(
        &ws,
        "alice",
        &["commit", "Source/a.cpp", "Source/b.cpp", "-m", "on feature"],
    );

    let main = clone(&env, "main-ws");
    assert_eq!(read(&main, "Source/a.cpp"), "one");
    assert!(!main.join("Source/b.cpp").exists());

    let log = env.ok(&ws, "alice", &["log", "--commits"]);
    assert!(log.contains("on feature") && log.contains("seed"), "{log}");
    let log = env.ok(&main, "alice", &["log", "--commits"]);
    assert!(!log.contains("on feature"), "{log}");

    env.run(&ws, "alice", &["switch", "main"]);
    assert_eq!(read(&ws, "Source/a.cpp"), "one");
    assert!(
        !ws.join("Source/b.cpp").exists(),
        "files only on the branch are removed"
    );
    assert!(
        env.ok(&ws, "alice", &["status"])
            .contains("everything is up to date"),
        "a switch leaves a clean workspace"
    );

    let err = stderr(&env, &ws, &["switch", "feature"]);
    assert_eq!(err.trim(), "Switched to branch 'feature'");
    assert_eq!(read(&ws, "Source/a.cpp"), "two");
    assert_eq!(read(&ws, "Source/b.cpp"), "new");
    assert!(!stderr(&env, &ws, &["switch", "feature"]).contains("Switched"));

    let err = env.fails(&ws, "alice", &["switch", "nope"]);
    assert!(err.contains("invalid reference: nope"), "{err}");
    let err = env.fails(&ws, "alice", &["switch"]);
    assert!(err.contains("missing branch name"), "{err}");
}

#[test]
fn a_switch_refuses_when_local_changes_would_be_overwritten() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "one", None);
    env.seed("alice", "Source/keep.cpp", "keep", None);
    let ws = clone(&env, "ws");
    env.ok(&ws, "alice", &["switch", "-c", "feature"]);
    std::fs::write(ws.join("Source/a.cpp"), "feature").unwrap();
    std::fs::write(ws.join("Source/only.cpp"), "only on feature").unwrap();
    env.ok(
        &ws,
        "alice",
        &["commit", "Source/a.cpp", "Source/only.cpp", "-m", "feature"],
    );
    env.ok(&ws, "alice", &["switch", "main"]);

    env.ok(&ws, "alice", &["switch", "feature"]);
    std::fs::write(ws.join("Source/a.cpp"), "local edit").unwrap();
    std::fs::write(ws.join("Source/keep.cpp"), "also edited").unwrap();
    let err = env.fails(&ws, "alice", &["switch", "main"]);
    assert!(err.contains("Source/a.cpp"), "{err}");
    assert!(err.contains("nothing was changed"), "{err}");
    assert!(
        !err.contains("Source/keep.cpp"),
        "a file the switch does not touch carries over: {err}"
    );
    assert_eq!(read(&ws, "Source/a.cpp"), "local edit");
    assert_eq!(
        env.ok(&ws, "alice", &["branch", "--show-current"]),
        "feature\n"
    );

    let err = env.fails(&ws, "alice", &["checkout", "main"]);
    assert!(err.contains("Source/a.cpp"), "{err}");

    env.ok(&ws, "alice", &["switch", "--force", "main"]);
    assert_eq!(read(&ws, "Source/a.cpp"), "one");
    assert_eq!(
        read(&ws, "Source/keep.cpp"),
        "also edited",
        "changes to files both branches share are carried over"
    );
    assert!(!ws.join("Source/only.cpp").exists());
}

#[test]
fn an_untracked_file_in_the_way_stops_the_switch() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "one", None);
    let ws = clone(&env, "ws");
    env.ok(&ws, "alice", &["switch", "-c", "feature"]);
    std::fs::write(ws.join("Source/new.cpp"), "from feature").unwrap();
    env.ok(&ws, "alice", &["commit", "Source/new.cpp", "-m", "add"]);
    env.ok(&ws, "alice", &["switch", "main"]);

    std::fs::write(ws.join("Source/new.cpp"), "something else").unwrap();
    let err = env.fails(&ws, "alice", &["switch", "feature"]);
    assert!(
        err.contains("Source/new.cpp") && err.contains("untracked"),
        "{err}"
    );
    std::fs::write(ws.join("Source/new.cpp"), "from feature").unwrap();
    env.ok(&ws, "alice", &["switch", "feature"]);
    assert!(
        env.ok(&ws, "alice", &["status"])
            .contains("everything is up to date"),
        "an untracked file that already has the branch's content is taken over"
    );
}

#[test]
fn checkout_follows_gits_forms() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "one", None);
    env.seed("alice", "Source/b.cpp", "bee", None);
    let ws = clone(&env, "ws");

    let err = stderr(&env, &ws, &["checkout", "-b", "work"]);
    assert_eq!(err.trim(), "Switched to branch 'work'");
    std::fs::write(ws.join("Source/a.cpp"), "work").unwrap();
    env.ok(&ws, "alice", &["commit", "Source/a.cpp", "-m", "work"]);
    let err = stderr(&env, &ws, &["checkout", "main"]);
    assert_eq!(err.trim(), "Switched to branch 'main'");
    assert_eq!(read(&ws, "Source/a.cpp"), "one");

    std::fs::write(ws.join("Source/a.cpp"), "scribble").unwrap();
    std::fs::write(ws.join("Source/b.cpp"), "scribble").unwrap();
    env.ok(&ws, "alice", &["checkout", "--", "Source/a.cpp"]);
    assert_eq!(read(&ws, "Source/a.cpp"), "one");
    assert_eq!(read(&ws, "Source/b.cpp"), "scribble");
    env.ok(&ws, "alice", &["checkout", "--", "."]);
    assert_eq!(read(&ws, "Source/b.cpp"), "bee");

    env.ok(&ws, "alice", &["checkout", "work", "--", "Source/a.cpp"]);
    assert_eq!(read(&ws, "Source/a.cpp"), "work");
    let status = env.ok(&ws, "alice", &["status"]);
    assert!(
        status.contains("modified"),
        "taken from another branch, so it shows as a change: {status}"
    );

    let err = env.fails(&ws, "alice", &["checkout", "--", "Source/missing.cpp"]);
    assert!(err.contains("did not match any file known to pyn"), "{err}");
    env.ok(&ws, "alice", &["checkout", "--", "Source"]);
    assert_eq!(read(&ws, "Source/a.cpp"), "one");

    let err = env.fails(&ws, "alice", &["checkout", "nowhere"]);
    assert!(err.contains("pyn lock"), "{err}");
    let head = env.ok(&ws, "alice", &["log", "--commits"]);
    let id = head
        .lines()
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap();
    let err = env.fails(&ws, "alice", &["checkout", id]);
    assert!(err.contains("no detached HEAD"), "{err}");
    let err = env.fails(&ws, "alice", &["checkout", "-b", "work"]);
    assert!(err.contains("branch_exists"), "{err}");
}

#[test]
fn exclusive_files_have_a_pointer_per_branch() {
    let env = start();
    env.seed("alice", "Content/m.umap", "map one", None);
    let ws = clone(&env, "ws");
    env.ok(&ws, "alice", &["switch", "-c", "feature"]);
    env.ok(&ws, "alice", &["lock", "Content/m.umap"]);
    std::fs::write(ws.join("Content/m.umap"), "map two").unwrap();
    let out = env.ok(
        &ws,
        "alice",
        &["commit", "Content/m.umap", "-m", "feature map"],
    );
    assert!(out.contains("revision 2"), "{out}");

    let other = clone(&env, "other");
    assert_eq!(read(&other, "Content/m.umap"), "map one");
    env.ok(&other, "bob", &["lock", "Content/m.umap"]);
    std::fs::write(other.join("Content/m.umap"), "map three").unwrap();
    let out = env.ok(
        &other,
        "bob",
        &["commit", "Content/m.umap", "-m", "main map"],
    );
    assert!(
        out.contains("revision 3"),
        "main's pointer was r1, not the feature's r2: {out}"
    );

    env.ok(&ws, "alice", &["switch", "main"]);
    env.ok(&ws, "alice", &["pull"]);
    assert_eq!(read(&ws, "Content/m.umap"), "map three");
    assert!(
        std::fs::metadata(ws.join("Content/m.umap"))
            .unwrap()
            .permissions()
            .readonly(),
        "an exclusive file is read-only on every branch"
    );
    env.ok(&ws, "alice", &["switch", "feature"]);
    assert_eq!(read(&ws, "Content/m.umap"), "map two");
    let history = env.ok(&ws, "alice", &["log", "Content/m.umap"]);
    assert_eq!(history.lines().count(), 3, "{history}");
    env.ok(&ws, "alice", &["switch", "main"]);
    let history = env.ok(&ws, "alice", &["log", "Content/m.umap"]);
    assert_eq!(
        history.lines().count(),
        3,
        "main's view is r1 and r3, not the feature's r2: {history}"
    );
}

#[test]
fn a_branch_deleted_elsewhere_is_reported_and_switch_recovers() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "one", None);
    let ws = clone(&env, "ws");
    env.ok(&ws, "alice", &["switch", "-c", "gone"]);
    let other = clone(&env, "other");
    env.ok(&other, "alice", &["branch", "-D", "gone"]);

    let err = env.fails(&ws, "alice", &["status"]);
    assert!(err.contains("branch_not_found"), "{err}");
    env.ok(&ws, "alice", &["switch", "main"]);
    assert!(
        env.ok(&ws, "alice", &["status"])
            .contains("everything is up to date")
    );
}

#[test]
fn staged_changes_follow_a_switch_only_where_both_branches_agree() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "one", None);
    env.seed("alice", "Source/b.cpp", "bee", None);
    let ws = clone(&env, "ws");
    env.ok(&ws, "alice", &["switch", "-c", "feature"]);
    std::fs::write(ws.join("Source/b.cpp"), "feature bee").unwrap();
    env.ok(&ws, "alice", &["add", "Source/b.cpp"]);
    env.ok(&ws, "alice", &["commit", "-m", "feature b"]);
    env.ok(&ws, "alice", &["switch", "main"]);

    std::fs::write(ws.join("Source/a.cpp"), "staged a").unwrap();
    env.ok(&ws, "alice", &["add", "Source/a.cpp"]);
    env.ok(&ws, "alice", &["switch", "feature"]);
    let status = env.ok(&ws, "alice", &["status"]);
    assert!(
        status.contains("added") || status.contains("modified"),
        "{status}"
    );
    env.ok(&ws, "alice", &["commit", "-m", "a on feature"]);
    assert_eq!(read(&ws, "Source/a.cpp"), "staged a");
    env.ok(&ws, "alice", &["switch", "main"]);
    assert_eq!(
        read(&ws, "Source/a.cpp"),
        "one",
        "the commit went to feature only"
    );

    std::fs::write(ws.join("Source/b.cpp"), "staged b").unwrap();
    env.ok(&ws, "alice", &["add", "Source/b.cpp"]);
    let err = env.fails(&ws, "alice", &["switch", "feature"]);
    assert!(err.contains("Source/b.cpp"), "{err}");
    env.ok(&ws, "alice", &["switch", "--force", "feature"]);
    assert_eq!(read(&ws, "Source/b.cpp"), "feature bee");
    let status = env.ok(&ws, "alice", &["status"]);
    assert!(status.contains("everything is up to date"), "{status}");
}
