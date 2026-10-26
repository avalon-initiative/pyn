//! `pyn cherry-pick` in a workspace with `local_store` on, against a real server. Real git reads the store as an
//! oracle for semantics only.

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

fn git(ws: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("--git-dir")
        .arg(ws.join(".pyn/local_only/store"))
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

fn read(ws: &Path, path: &str) -> String {
    std::fs::read_to_string(ws.join(path)).unwrap()
}

fn write(ws: &Path, path: &str, text: &str) {
    let target = ws.join(path);
    if target.exists() {
        let mut perms = std::fs::metadata(&target).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        std::fs::set_permissions(&target, perms).unwrap();
    }
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::fs::write(target, text).unwrap();
}

fn commit_local(env: &Env, ws: &Path, path: &str, text: &str, message: &str) {
    write(ws, path, text);
    env.ok(ws, "alice", &["add", path]);
    env.ok(ws, "alice", &["commit", "-m", message]);
}

fn seeded() -> (Env, PathBuf) {
    let env = start();
    env.seed("alice", "Source/a.cpp", "one\ntwo\nthree\n", None);
    env.seed("alice", "Source/b.cpp", "bee", None);
    env.seed("alice", "Content/m.umap", "map", None);
    let ws = clone_with(&env, "ws", &["--local-store"]);
    (env, ws)
}

/// `feature` holds three commits on top of the shared start: a change to `a.cpp`, a new `Source/c.cpp` and a change to
/// `b.cpp`. The workspace ends on `main`, which is untouched.
fn with_feature() -> (Env, PathBuf) {
    let (env, ws) = seeded();
    env.server_branch(&ws, "alice", "feature");
    commit_local(&env, &ws, "Source/a.cpp", "ONE\ntwo\nthree\n", "feature a");
    commit_local(&env, &ws, "Source/c.cpp", "cee", "feature c");
    commit_local(&env, &ws, "Source/b.cpp", "bee2", "feature b");
    env.ok(&ws, "alice", &["push"]);
    env.ok(&ws, "alice", &["switch", "main"]);
    env.ok(&ws, "alice", &["update"]);
    (env, ws)
}

fn subjects(ws: &Path, range: &str) -> String {
    git(ws, &["log", "--format=%s", range])
}

#[test]
fn a_picked_commit_becomes_a_new_local_commit() {
    let (env, ws) = with_feature();
    commit_local(&env, &ws, "Source/b.cpp", "bee on main", "main b");
    let picked = git(&ws, &["rev-parse", "feature~2"]).trim().to_string();
    let out = env.ok(&ws, "alice", &["cherry-pick", &picked[..10]]);
    assert!(out.contains("feature a"), "{out}");
    assert_eq!(read(&ws, "Source/a.cpp"), "ONE\ntwo\nthree\n");
    assert!(!ws.join("Source/c.cpp").exists());
    assert_eq!(subjects(&ws, "origin/main..main"), "feature a\nmain b\n");
    let new = git(&ws, &["rev-parse", "main"]).trim().to_string();
    assert_ne!(new, picked, "a new commit, not the picked one");
    assert_eq!(
        git(&ws, &["rev-parse", "main~2"]).trim(),
        git(&ws, &["rev-parse", "origin/main"]).trim()
    );
    assert_eq!(
        git(&ws, &["log", "-1", "--format=%an"]),
        git(&ws, &["log", "-1", "--format=%an", &picked])
    );
    git(&ws, &["fsck", "--strict"]);
    let status = env.ok(&ws, "alice", &["status"]);
    assert!(status.contains("modified in a local commit"), "{status}");

    env.ok(&ws, "alice", &["push"]);
    let third = clone_with(&env, "third", &[]);
    assert_eq!(read(&third, "Source/a.cpp"), "ONE\ntwo\nthree\n");
}

#[test]
fn record_origin_adds_the_picked_id() {
    let (env, ws) = with_feature();
    let picked = git(&ws, &["rev-parse", "feature"]).trim().to_string();
    env.ok(&ws, "alice", &["cherry-pick", "-x", &picked]);
    let body = git(&ws, &["log", "-1", "--format=%B", "main"]);
    assert_eq!(
        body,
        format!("feature b\n\n(cherry picked from commit {picked})\n\n")
    );
}

#[test]
fn a_range_is_applied_oldest_first_and_branch_names_work() {
    let (env, ws) = with_feature();
    let out = env.ok(&ws, "alice", &["cherry-pick", "main..feature"]);
    assert_eq!(out.matches("] feature").count(), 3, "{out}");
    assert_eq!(
        subjects(&ws, "origin/main..main"),
        "feature b\nfeature c\nfeature a\n"
    );
    assert_eq!(read(&ws, "Source/c.cpp"), "cee");
    assert_eq!(read(&ws, "Source/b.cpp"), "bee2");
    git(&ws, &["fsck", "--strict"]);

    let (env, ws) = with_feature();
    env.ok(&ws, "alice", &["cherry-pick", "feature", "feature~2"]);
    assert_eq!(subjects(&ws, "origin/main..main"), "feature a\nfeature b\n");
    drop(env);
}

#[test]
fn a_commit_with_nothing_new_is_dropped() {
    let (env, ws) = with_feature();
    env.ok(&ws, "alice", &["cherry-pick", "feature~2"]);
    let out = env.ok(&ws, "alice", &["cherry-pick", "feature~2"]);
    assert!(out.contains("changes nothing here"), "{out}");
    assert_eq!(subjects(&ws, "origin/main..main"), "feature a\n");
}

fn conflicting() -> (Env, PathBuf) {
    let (env, ws) = with_feature();
    commit_local(&env, &ws, "Source/a.cpp", "mine\ntwo\nthree\n", "main a");
    (env, ws)
}

#[test]
fn a_conflict_stops_with_markers_and_continue_commits_the_resolution() {
    let (env, ws) = conflicting();
    let err = env.fails(&ws, "alice", &["cherry-pick", "main..feature"]);
    assert!(
        err.contains("could not apply")
            && err.contains("feature a")
            && err.contains("commit 1 of 3"),
        "{err}"
    );
    assert!(
        err.contains("pyn cherry-pick --continue") && err.contains("--skip"),
        "{err}"
    );
    assert!(read(&ws, "Source/a.cpp").contains("<<<<<<< HEAD\nmine\n"));
    assert!(!ws.join("Source/c.cpp").exists(), "the next commit waits");
    let status = env.ok(&ws, "alice", &["status"]);
    assert!(
        status.contains("cherry-picking") && status.contains("unmerged"),
        "{status}"
    );
    let refused = env.fails(&ws, "alice", &["cherry-pick", "feature"]);
    assert!(
        refused.contains("not concluded") && refused.contains("cherry-pick"),
        "{refused}"
    );
    let wrong = env.fails(&ws, "alice", &["pull", "--abort"]);
    assert!(wrong.contains("cherry-pick is in progress"), "{wrong}");

    assert!(
        env.fails(&ws, "alice", &["cherry-pick", "--continue"])
            .contains("unmerged")
    );
    write(&ws, "Source/a.cpp", "both\ntwo\nthree\n");
    env.ok(&ws, "alice", &["add", "Source/a.cpp"]);
    let out = env.ok(&ws, "alice", &["cherry-pick", "--continue"]);
    assert!(
        out.contains("feature c") && out.contains("feature b"),
        "{out}"
    );
    assert_eq!(
        subjects(&ws, "origin/main..main"),
        "feature b\nfeature c\nfeature a\nmain a\n"
    );
    assert_eq!(
        git(&ws, &["cat-file", "blob", "main~2:Source/a.cpp"]),
        "both\ntwo\nthree\n"
    );
    assert!(!env.ok(&ws, "alice", &["status"]).contains("cherry-picking"));
    git(&ws, &["fsck", "--strict"]);
    env.ok(&ws, "alice", &["push"]);
}

#[test]
fn skip_drops_the_stopped_commit_and_abort_puts_everything_back() {
    let (env, ws) = conflicting();
    env.fails(&ws, "alice", &["cherry-pick", "main..feature"]);
    env.ok(&ws, "alice", &["cherry-pick", "--skip"]);
    assert_eq!(
        subjects(&ws, "origin/main..main"),
        "feature b\nfeature c\nmain a\n"
    );
    assert_eq!(read(&ws, "Source/a.cpp"), "mine\ntwo\nthree\n");
    assert_eq!(read(&ws, "Source/c.cpp"), "cee");

    let (env, ws) = conflicting();
    let before = git(&ws, &["rev-parse", "main"]);
    env.fails(&ws, "alice", &["cherry-pick", "main..feature"]);
    write(&ws, "Source/a.cpp", "scribble\n");
    env.ok(&ws, "alice", &["add", "Source/a.cpp"]);
    let out = env.ok(&ws, "alice", &["cherry-pick", "--abort"]);
    assert!(out.contains("given up"), "{out}");
    assert_eq!(git(&ws, &["rev-parse", "main"]), before);
    assert_eq!(read(&ws, "Source/a.cpp"), "mine\ntwo\nthree\n");
    assert!(
        env.fails(&ws, "alice", &["cherry-pick", "--abort"])
            .contains("no cherry-pick")
    );
}

#[test]
fn an_exclusive_path_both_sides_changed_is_a_conflict_that_needs_the_lock() {
    let (env, ws) = seeded();
    env.server_branch(&ws, "alice", "feature");
    env.ok(&ws, "alice", &["lock", "Content/m.umap"]);
    commit_local(&env, &ws, "Content/m.umap", "map on feature", "feature map");
    env.ok(&ws, "alice", &["push"]);
    env.ok(&ws, "alice", &["switch", "main"]);
    env.ok(&ws, "alice", &["update"]);
    env.ok(&ws, "alice", &["lock", "Content/m.umap"]);
    commit_local(&env, &ws, "Content/m.umap", "map on main", "main map");
    env.ok(&ws, "alice", &["unlock", "Content/m.umap"]);

    let err = env.fails(&ws, "alice", &["cherry-pick", "feature"]);
    assert!(
        err.contains("CONFLICT (exclusive): Content/m.umap") && err.contains("pyn lock"),
        "{err}"
    );
    assert_eq!(read(&ws, "Content/m.umap"), "map on main");
    env.ok(
        &ws,
        "alice",
        &["checkout", "--theirs", "--", "Content/m.umap"],
    );
    assert_eq!(read(&ws, "Content/m.umap"), "map on feature");
    env.ok(&ws, "alice", &["add", "Content/m.umap"]);
    let err = env.fails(&ws, "alice", &["cherry-pick", "--continue"]);
    assert!(err.contains("pyn lock Content/m.umap"), "{err}");
    env.ok(&ws, "alice", &["lock", "Content/m.umap"]);
    env.ok(&ws, "alice", &["cherry-pick", "--continue"]);
    assert_eq!(
        subjects(&ws, "origin/main..main"),
        "feature map\nmain map\n"
    );
    env.ok(&ws, "alice", &["push"]);
}

#[test]
fn an_exclusive_path_only_the_picked_commit_changed_is_taken() {
    let (env, ws) = seeded();
    env.server_branch(&ws, "alice", "feature");
    env.ok(&ws, "alice", &["lock", "Content/m.umap"]);
    commit_local(&env, &ws, "Content/m.umap", "map on feature", "feature map");
    env.ok(&ws, "alice", &["push"]);
    env.ok(&ws, "alice", &["switch", "main"]);
    env.ok(&ws, "alice", &["update"]);
    commit_local(&env, &ws, "Source/b.cpp", "bee on main", "main b");
    env.ok(&ws, "alice", &["cherry-pick", "feature"]);
    assert_eq!(read(&ws, "Content/m.umap"), "map on feature");
    let err = env.fails(&ws, "alice", &["push"]);
    assert!(err.contains("lock required"), "{err}");
}

#[test]
fn local_edits_and_staged_changes_stop_it_before_anything_changes() {
    let (env, ws) = with_feature();
    write(&ws, "Source/a.cpp", "edited\n");
    let err = env.fails(&ws, "alice", &["cherry-pick", "feature~2"]);
    assert!(err.contains("local changes"), "{err}");
    assert_eq!(subjects(&ws, "origin/main..main"), "");
    env.ok(&ws, "alice", &["checkout", "--", "Source/a.cpp"]);

    write(&ws, "Source/b.cpp", "staged\n");
    env.ok(&ws, "alice", &["add", "Source/b.cpp"]);
    let err = env.fails(&ws, "alice", &["cherry-pick", "feature~2"]);
    assert!(err.contains("staged changes"), "{err}");
}

#[test]
fn what_a_workspace_cannot_do_is_refused_by_name() {
    let (env, ws) = with_feature();
    for flag in ["-n", "-s", "--quit"] {
        let args: Vec<&str> = if flag == "--quit" {
            vec!["cherry-pick", flag]
        } else {
            vec!["cherry-pick", flag, "feature"]
        };
        let err = env.fails(&ws, "alice", &args);
        assert!(
            err.contains(flag) && err.contains("git repository"),
            "{flag}: {err}"
        );
    }
    let err = env.fails(&ws, "alice", &["cherry-pick", "-m", "1", "feature"]);
    assert!(
        err.contains("-m") && err.contains("git repository"),
        "{err}"
    );
    let err = env.fails(&ws, "alice", &["cherry-pick", "nosuchbranch"]);
    assert!(err.contains("bad revision"), "{err}");
    assert!(
        env.fails(&ws, "alice", &["cherry-pick", "--continue"])
            .contains("no cherry-pick")
    );
    assert_eq!(subjects(&ws, "origin/main..main"), "");
}
