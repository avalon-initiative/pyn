//! `pyn revert` in a workspace with `local_store` on, against a real server. Real git reads the store as an
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

/// Three local commits on `main`: a change to `a.cpp`, a new `Source/c.cpp` and a change to `b.cpp`.
fn with_history() -> (Env, PathBuf) {
    let (env, ws) = seeded();
    commit_local(&env, &ws, "Source/a.cpp", "ONE\ntwo\nthree\n", "change a");
    commit_local(&env, &ws, "Source/c.cpp", "cee", "add c");
    commit_local(&env, &ws, "Source/b.cpp", "bee2", "change b");
    (env, ws)
}

fn subjects(ws: &Path, range: &str) -> String {
    git(ws, &["log", "--format=%s", range])
}

#[test]
fn a_reverted_commit_becomes_a_new_local_commit() {
    let (env, ws) = with_history();
    let reverted = git(&ws, &["rev-parse", "main~1"]).trim().to_string();
    let out = env.ok(&ws, "alice", &["revert", &reverted[..10]]);
    assert!(out.contains("Revert \"add c\""), "{out}");
    assert!(!ws.join("Source/c.cpp").exists());
    assert_eq!(read(&ws, "Source/b.cpp"), "bee2");
    assert_eq!(
        subjects(&ws, "origin/main..main"),
        "Revert \"add c\"\nchange b\nadd c\nchange a\n"
    );
    assert_eq!(
        git(&ws, &["log", "-1", "--format=%B", "main"]),
        format!("Revert \"add c\"\n\nThis reverts commit {reverted}.\n\n")
    );
    assert_eq!(
        git(&ws, &["log", "-1", "--format=%an", "main"]),
        git(&ws, &["log", "-1", "--format=%cn", "main"])
    );
    git(&ws, &["fsck", "--strict"]);
    assert!(env.ok(&ws, "alice", &["status"]).contains("local commit"));

    env.ok(&ws, "alice", &["revert", "main"]);
    assert_eq!(
        git(&ws, &["log", "-1", "--format=%s", "main"]),
        "Reapply \"add c\"\n"
    );
    env.ok(&ws, "alice", &["push"]);
    let third = clone_with(&env, "third", &[]);
    assert_eq!(read(&third, "Source/b.cpp"), "bee2");
}

#[test]
fn reverting_a_revert_reapplies_and_a_range_goes_newest_first() {
    let (env, ws) = with_history();
    env.ok(&ws, "alice", &["revert", "main"]);
    env.ok(&ws, "alice", &["revert", "main"]);
    assert_eq!(
        git(&ws, &["log", "-1", "--format=%s", "main"]),
        "Reapply \"change b\"\n"
    );
    assert_eq!(read(&ws, "Source/b.cpp"), "bee2");

    let (env, ws) = with_history();
    let out = env.ok(&ws, "alice", &["revert", "main~3..main"]);
    assert_eq!(out.matches("] Revert").count(), 3, "{out}");
    assert_eq!(
        subjects(&ws, "main~3..main")
            .lines()
            .take(3)
            .collect::<Vec<_>>(),
        [
            "Revert \"change a\"",
            "Revert \"add c\"",
            "Revert \"change b\""
        ]
    );
    assert_eq!(read(&ws, "Source/a.cpp"), "one\ntwo\nthree\n");
    assert_eq!(read(&ws, "Source/b.cpp"), "bee");
    assert!(!ws.join("Source/c.cpp").exists());
    git(&ws, &["fsck", "--strict"]);
}

#[test]
fn a_commit_that_is_already_undone_is_dropped() {
    let (env, ws) = with_history();
    env.ok(&ws, "alice", &["revert", "main~1"]);
    let out = env.ok(&ws, "alice", &["revert", "main~2"]);
    assert!(out.contains("changes nothing here"), "{out}");
    assert_eq!(
        subjects(&ws, "origin/main..main").lines().count(),
        4,
        "{}",
        subjects(&ws, "origin/main..main")
    );
}

fn conflicting() -> (Env, PathBuf) {
    let (env, ws) = with_history();
    commit_local(&env, &ws, "Source/a.cpp", "mine\ntwo\nthree\n", "main a");
    (env, ws)
}

#[test]
fn a_conflict_stops_with_markers_and_continue_commits_the_resolution() {
    let (env, ws) = conflicting();
    let err = env.fails(&ws, "alice", &["revert", "main~3", "main~2"]);
    assert!(
        err.contains("could not apply")
            && err.contains("change a")
            && err.contains("commit 1 of 2"),
        "{err}"
    );
    assert!(ws.join("Source/c.cpp").exists(), "the next commit waits");
    let err = env.fails(&ws, "alice", &["revert", "main~1"]);
    assert!(
        err.contains("not concluded") && err.contains("revert"),
        "{err}"
    );

    let (env, ws) = conflicting();
    let err = env.fails(&ws, "alice", &["revert", "main~3", "main~1"]);
    assert!(
        err.contains("pyn revert --continue") && err.contains("--skip"),
        "{err}"
    );
    assert!(read(&ws, "Source/a.cpp").contains("<<<<<<< HEAD\nmine\n"));
    let status = env.ok(&ws, "alice", &["status"]);
    assert!(
        status.contains("reverting") && status.contains("unmerged"),
        "{status}"
    );
    let wrong = env.fails(&ws, "alice", &["cherry-pick", "--continue"]);
    assert!(wrong.contains("revert is in progress"), "{wrong}");
    let wrong = env.fails(&ws, "alice", &["pull", "--abort"]);
    assert!(wrong.contains("revert is in progress"), "{wrong}");
    assert!(
        env.fails(&ws, "alice", &["revert", "--continue"])
            .contains("unmerged")
    );
    write(&ws, "Source/a.cpp", "both\ntwo\nthree\n");
    env.ok(&ws, "alice", &["add", "Source/a.cpp"]);
    let out = env.ok(&ws, "alice", &["revert", "--continue"]);
    assert!(out.contains("Revert \"change b\""), "{out}");
    assert_eq!(
        subjects(&ws, "origin/main..main")
            .lines()
            .take(3)
            .collect::<Vec<_>>(),
        ["Revert \"change b\"", "Revert \"change a\"", "main a"]
    );
    assert!(!env.ok(&ws, "alice", &["status"]).contains("reverting"));
    git(&ws, &["fsck", "--strict"]);
    env.ok(&ws, "alice", &["push"]);
}

#[test]
fn skip_drops_the_stopped_commit_and_abort_puts_everything_back() {
    let (env, ws) = conflicting();
    env.fails(&ws, "alice", &["revert", "main~3", "main~2"]);
    env.ok(&ws, "alice", &["revert", "--skip"]);
    assert_eq!(read(&ws, "Source/a.cpp"), "mine\ntwo\nthree\n");
    assert!(!ws.join("Source/c.cpp").exists());
    assert_eq!(
        subjects(&ws, "origin/main..main").lines().next(),
        Some("Revert \"add c\"")
    );

    let (env, ws) = conflicting();
    let before = git(&ws, &["rev-parse", "main"]);
    env.fails(&ws, "alice", &["revert", "main~3", "main~2"]);
    write(&ws, "Source/a.cpp", "scribble\n");
    env.ok(&ws, "alice", &["add", "Source/a.cpp"]);
    let out = env.ok(&ws, "alice", &["revert", "--abort"]);
    assert!(out.contains("given up"), "{out}");
    assert_eq!(git(&ws, &["rev-parse", "main"]), before);
    assert_eq!(read(&ws, "Source/a.cpp"), "mine\ntwo\nthree\n");
    assert!(
        env.fails(&ws, "alice", &["revert", "--abort"])
            .contains("no revert")
    );
}

fn with_exclusive() -> (Env, PathBuf) {
    let (env, ws) = seeded();
    env.ok(&ws, "alice", &["lock", "Content/m.umap"]);
    commit_local(&env, &ws, "Content/m.umap", "map one", "map one");
    (env, ws)
}

#[test]
fn an_exclusive_path_only_the_reverted_commit_changed_is_taken() {
    let (env, ws) = with_exclusive();
    env.ok(&ws, "alice", &["revert", "main"]);
    assert_eq!(read(&ws, "Content/m.umap"), "map");
}

#[test]
fn an_exclusive_path_both_sides_changed_is_a_conflict_that_needs_the_lock() {
    let (env, ws) = with_exclusive();
    commit_local(&env, &ws, "Content/m.umap", "map two", "map two");
    env.ok(&ws, "alice", &["unlock", "Content/m.umap"]);
    let err = env.fails(&ws, "alice", &["revert", "main~1"]);
    assert!(
        err.contains("CONFLICT (exclusive): Content/m.umap") && err.contains("pyn lock"),
        "{err}"
    );
    assert_eq!(read(&ws, "Content/m.umap"), "map two");
    env.ok(
        &ws,
        "alice",
        &["checkout", "--theirs", "--", "Content/m.umap"],
    );
    assert_eq!(read(&ws, "Content/m.umap"), "map");
    env.ok(&ws, "alice", &["add", "Content/m.umap"]);
    let err = env.fails(&ws, "alice", &["revert", "--continue"]);
    assert!(err.contains("pyn lock Content/m.umap"), "{err}");
    env.ok(&ws, "alice", &["lock", "Content/m.umap"]);
    env.ok(&ws, "alice", &["revert", "--continue"]);
    assert_eq!(
        subjects(&ws, "origin/main..main").lines().next(),
        Some("Revert \"map one\"")
    );
}

#[test]
fn local_edits_and_staged_changes_stop_it_before_anything_changes() {
    let (env, ws) = with_history();
    write(&ws, "Source/a.cpp", "edited\n");
    let err = env.fails(&ws, "alice", &["revert", "main~2"]);
    assert!(err.contains("local changes"), "{err}");
    assert_eq!(subjects(&ws, "origin/main..main").lines().count(), 3);
    env.ok(&ws, "alice", &["checkout", "--", "Source/a.cpp"]);

    write(&ws, "Source/b.cpp", "staged\n");
    env.ok(&ws, "alice", &["add", "Source/b.cpp"]);
    let err = env.fails(&ws, "alice", &["revert", "main~1"]);
    assert!(err.contains("staged changes"), "{err}");
}

#[test]
fn what_a_workspace_cannot_do_is_refused_by_name() {
    let (env, ws) = with_history();
    for flag in ["-n", "-s"] {
        let err = env.fails(&ws, "alice", &["revert", flag, "main"]);
        assert!(
            err.contains(flag) && err.contains("git repository"),
            "{flag}: {err}"
        );
    }
    let err = env.fails(&ws, "alice", &["revert", "--quit"]);
    assert!(
        err.contains("--quit") && err.contains("git repository"),
        "{err}"
    );
    let err = env.fails(&ws, "alice", &["revert", "-m", "1", "main"]);
    assert!(
        err.contains("-m") && err.contains("git repository"),
        "{err}"
    );
    let err = env.fails(&ws, "alice", &["revert", "nosuchbranch"]);
    assert!(err.contains("bad revision"), "{err}");
    assert!(
        env.fails(&ws, "alice", &["revert", "--continue"])
            .contains("no revert")
    );
    assert_eq!(subjects(&ws, "origin/main..main").lines().count(), 3);
}
