//! Local branches in the store of a `local_store` workspace, which behave as git's do: independent of the server's
//! branches, switchable offline with unpublished commits, and tied to a server branch only by their upstream. Real git
//! reads the store as an oracle for semantics only.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::{Env, REPO, start};

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

fn config(ws: &Path) -> String {
    std::fs::read_to_string(store(ws).join("config")).unwrap()
}

fn seeded() -> (Env, PathBuf) {
    let env = start();
    env.seed("alice", "Source/a.cpp", "one", None);
    env.seed("alice", "Source/b.cpp", "bee", None);
    let ws = env.dir("ws");
    env.ok(
        &env.dir("home"),
        "alice",
        &[
            "clone",
            "--local-store",
            &env.source(REPO),
            ws.to_str().unwrap(),
        ],
    );
    (env, ws.canonicalize().unwrap())
}

fn read(ws: &Path, path: &str) -> String {
    std::fs::read_to_string(ws.join(path)).unwrap()
}

fn commit(env: &Env, ws: &Path, path: &str, text: &str, message: &str) {
    std::fs::write(ws.join(path), text).unwrap();
    env.ok(ws, "alice", &["add", path]);
    env.ok(ws, "alice", &["commit", "-m", message]);
}

fn server_branch(env: &Env, name: &str) {
    env.ok(&env.dir("seed"), "alice", &["branch", name]);
}

fn same_as_git(env: &Env, ws: &Path, args: &[&str]) {
    let ours = env.ok(ws, "alice", args);
    assert_eq!(ours, git(ws, args), "pyn {args:?}");
}

#[test]
fn fetch_stores_every_server_branch_without_making_local_ones() {
    let (env, ws) = seeded();
    server_branch(&env, "feature");
    env.ok(&ws, "alice", &["fetch"]);
    assert_eq!(env.ok(&ws, "alice", &["branch"]), "* main\n");
    same_as_git(&env, &ws, &["branch", "-a"]);
    same_as_git(&env, &ws, &["branch", "-r"]);
    assert!(
        env.ok(&ws, "alice", &["branch", "-r"])
            .contains("origin/feature")
    );
    assert_eq!(
        git(&ws, &["rev-parse", "origin/feature"]),
        git(&ws, &["rev-parse", "origin/main"])
    );
}

#[test]
fn switching_to_a_remote_branch_creates_a_local_one_that_follows_it() {
    let (env, ws) = seeded();
    server_branch(&env, "feature");
    env.ok(&ws, "alice", &["fetch"]);
    env.ok(&ws, "alice", &["switch", "feature"]);
    assert_eq!(git(&ws, &["config", "branch.feature.remote"]), "origin\n");
    assert_eq!(
        git(&ws, &["config", "branch.feature.merge"]),
        "refs/heads/feature\n"
    );
    assert_eq!(
        git(&ws, &["rev-parse", "feature"]),
        git(&ws, &["rev-parse", "origin/feature"])
    );
    same_as_git(&env, &ws, &["branch", "-a"]);
    assert_eq!(
        env.ok(&ws, "alice", &["branch", "--show-current"]),
        "feature\n"
    );
}

#[test]
fn a_branch_the_store_has_not_seen_is_fetched_by_checkout() {
    let (env, ws) = seeded();
    server_branch(&env, "late");
    env.ok(&ws, "alice", &["checkout", "late"]);
    assert_eq!(
        env.ok(&ws, "alice", &["branch", "--show-current"]),
        "late\n"
    );
    assert!(git(&ws, &["rev-parse", "origin/late"]).trim().len() > 10);
    let err = env.fails(&ws, "alice", &["switch", "nowhere"]);
    assert!(err.contains("invalid reference"), "{err}");
}

#[test]
fn a_local_only_branch_is_made_and_used_without_the_server() {
    let (env, ws) = seeded();
    env.ok(&ws, "alice", &["switch", "-c", "topic"]);
    commit(&env, &ws, "Source/a.cpp", "topic one", "on topic");
    same_as_git(&env, &ws, &["branch"]);
    assert!(git(&ws, &["for-each-ref", "refs/remotes/origin/topic"]).is_empty());
    assert!(!config(&ws).contains("topic"));
}

#[test]
fn a_local_only_branch_has_no_upstream_to_push_to() {
    let (env, ws) = seeded();
    env.ok(&ws, "alice", &["switch", "-c", "topic"]);
    commit(&env, &ws, "Source/a.cpp", "topic one", "on topic");
    let err = env.fails(&ws, "alice", &["push"]);
    assert!(err.contains("has no upstream branch"), "{err}");
    assert!(err.contains("--set-upstream origin topic"), "{err}");
    let err = env.fails(&ws, "alice", &["pull"]);
    assert!(err.contains("no tracking information"), "{err}");
    let err = env.fails(&ws, "alice", &["update"]);
    assert!(err.contains("not on the server"), "{err}");
}

#[test]
fn switching_keeps_unpublished_commits_and_carries_unrelated_changes() {
    let (env, ws) = seeded();
    env.ok(&ws, "alice", &["branch", "other"]);
    commit(&env, &ws, "Source/a.cpp", "main two", "main work");
    std::fs::write(ws.join("Source/b.cpp"), "dirty bee").unwrap();
    env.ok(&ws, "alice", &["switch", "other"]);
    assert_eq!(read(&ws, "Source/a.cpp"), "one");
    assert_eq!(read(&ws, "Source/b.cpp"), "dirty bee");
    env.ok(&ws, "alice", &["switch", "main"]);
    assert_eq!(read(&ws, "Source/a.cpp"), "main two");
    assert_eq!(
        git(&ws, &["rev-list", "--count", "origin/main..main"]),
        "1\n"
    );
}

#[test]
fn a_switch_that_would_overwrite_a_local_change_is_refused() {
    let (env, ws) = seeded();
    env.ok(&ws, "alice", &["switch", "-c", "topic"]);
    commit(&env, &ws, "Source/a.cpp", "topic one", "on topic");
    env.ok(&ws, "alice", &["switch", "main"]);
    std::fs::write(ws.join("Source/a.cpp"), "dirty").unwrap();
    let err = env.fails(&ws, "alice", &["switch", "topic"]);
    assert!(err.contains("would be overwritten"), "{err}");
    assert_eq!(read(&ws, "Source/a.cpp"), "dirty");
    env.ok(&ws, "alice", &["switch", "--force", "topic"]);
    assert_eq!(read(&ws, "Source/a.cpp"), "topic one");
}

#[test]
fn branches_are_listed_created_renamed_and_deleted_like_git() {
    let (env, ws) = seeded();
    env.ok(&ws, "alice", &["branch", "one"]);
    env.ok(&ws, "alice", &["branch", "two", "one"]);
    same_as_git(&env, &ws, &["branch"]);
    env.ok(&ws, "alice", &["branch", "-m", "two", "three"]);
    same_as_git(&env, &ws, &["branch"]);
    let out = env.ok(&ws, "alice", &["branch", "-d", "one"]);
    assert!(out.starts_with("Deleted branch one (was "), "{out}");
    same_as_git(&env, &ws, &["branch"]);
    let err = env.fails(&ws, "alice", &["branch", "-d", "main"]);
    assert!(err.contains("cannot delete branch"), "{err}");
    let err = env.fails(&ws, "alice", &["branch", "three"]);
    assert!(err.contains("already exists"), "{err}");
}

#[test]
fn an_unmerged_branch_needs_a_capital_d() {
    let (env, ws) = seeded();
    env.ok(&ws, "alice", &["switch", "-c", "topic"]);
    commit(&env, &ws, "Source/a.cpp", "topic one", "on topic");
    env.ok(&ws, "alice", &["switch", "main"]);
    let err = env.fails(&ws, "alice", &["branch", "-d", "topic"]);
    assert!(err.contains("not fully merged"), "{err}");
    env.ok(&ws, "alice", &["branch", "-D", "topic"]);
    assert_eq!(env.ok(&ws, "alice", &["branch"]), "* main\n");
}

#[test]
fn the_upstream_follows_git_config_and_shows_in_status() {
    let (env, ws) = seeded();
    server_branch(&env, "feature");
    env.ok(&ws, "alice", &["fetch"]);
    env.ok(&ws, "alice", &["branch", "mine", "origin/feature"]);
    assert_eq!(
        git(&ws, &["config", "branch.mine.merge"]),
        "refs/heads/feature\n"
    );
    env.ok(&ws, "alice", &["branch", "--unset-upstream", "mine"]);
    assert!(!config(&ws).contains("\"mine\""));
    env.ok(&ws, "alice", &["branch", "-u", "origin/feature", "mine"]);
    env.ok(&ws, "alice", &["switch", "mine"]);
    commit(&env, &ws, "Source/a.cpp", "mine one", "mine work");
    let out = env.ok(&ws, "alice", &["status", "--short", "--branch"]);
    assert!(
        out.starts_with("## mine...origin/feature [ahead 1]"),
        "{out}"
    );
}

#[test]
fn a_tracking_branch_is_pushed_to_the_branch_it_follows() {
    let (env, ws) = seeded();
    server_branch(&env, "feature");
    env.ok(&ws, "alice", &["switch", "feature"]);
    commit(&env, &ws, "Source/a.cpp", "feature one", "feature work");
    env.ok(&ws, "alice", &["switch", "main"]);
    let out = env.ok(&ws, "alice", &["push", "origin", "feature"]);
    assert!(out.contains("feature -> feature"), "{out}");
    assert_eq!(
        git(&ws, &["rev-parse", "feature"]),
        git(&ws, &["rev-parse", "origin/feature"])
    );
    assert_eq!(
        env.ok(&ws, "alice", &["branch", "--show-current"]),
        "main\n"
    );
}

#[test]
fn a_server_branch_is_deleted_through_push() {
    let (env, ws) = seeded();
    server_branch(&env, "gone");
    server_branch(&env, "gone2");
    env.ok(&ws, "alice", &["fetch"]);
    let out = env.ok(&ws, "alice", &["push", "--delete", "gone"]);
    assert!(out.contains(" - [deleted]         gone"), "{out}");
    env.ok(&ws, "alice", &["push", "origin", ":gone2"]);
    let listed = env.ok(&env.dir("seed"), "alice", &["branch"]);
    assert_eq!(listed, "* main\n", "{listed}");
    same_as_git(&env, &ws, &["branch", "-r"]);
    let err = env.fails(&ws, "alice", &["push", "--delete", "gone"]);
    assert!(err.contains("remote ref does not exist"), "{err}");
}

#[test]
fn branches_made_on_the_server_by_the_web_flow_stay_server_side_without_a_store() {
    let (env, _) = seeded();
    let plain = env.dir("plain");
    env.ok(
        &env.dir("home"),
        "alice",
        &[
            "clone",
            "--no-local-store",
            &env.source(REPO),
            plain.to_str().unwrap(),
        ],
    );
    env.ok(&plain, "alice", &["branch", "direct"]);
    assert!(
        env.ok(&env.dir("seed"), "alice", &["branch"])
            .contains("direct")
    );
}

fn writable(ws: &Path, path: &str) -> bool {
    !std::fs::metadata(ws.join(path))
        .unwrap()
        .permissions()
        .readonly()
}

#[test]
fn unpublished_content_takes_its_mode_from_the_policy_at_that_commit() {
    let (env, ws) = seeded();
    env.ok(&ws, "alice", &["switch", "-c", "topic"]);
    std::fs::create_dir_all(ws.join("Assets")).unwrap();
    commit(&env, &ws, "Assets/x.bin", "bits", "asset");
    commit(
        &env,
        &ws,
        ".pyn/pyn.toml",
        "[meta]\ndefault = \"shared\"\n[exclusive]\npaths = [\"Assets/\"]\n",
        "policy",
    );
    env.ok(&ws, "alice", &["switch", "main"]);
    assert!(!ws.join("Assets/x.bin").exists());
    env.ok(&ws, "alice", &["switch", "topic"]);
    assert!(
        !writable(&ws, "Assets/x.bin"),
        "an exclusive path is read-only without its lock"
    );
    assert!(writable(&ws, "Source/a.cpp"));
    let status = env.ok(&ws, "alice", &["status"]);
    let row = status.lines().find(|l| l.contains("Assets/x.bin")).unwrap();
    assert!(row.contains("exclusive"), "{status}");
}

#[test]
fn a_policy_that_makes_a_new_path_shared_leaves_it_writable() {
    let (env, ws) = seeded();
    env.ok(&ws, "alice", &["switch", "-c", "topic"]);
    std::fs::create_dir_all(ws.join("Notes")).unwrap();
    commit(&env, &ws, "Notes/n.txt", "words", "note");
    commit(
        &env,
        &ws,
        ".pyn/pyn.toml",
        "[meta]\ndefault = \"exclusive\"\n[shared]\npaths = [\"Notes/\", \"Source/\"]\n",
        "policy",
    );
    env.ok(&ws, "alice", &["switch", "main"]);
    env.ok(&ws, "alice", &["switch", "topic"]);
    assert!(writable(&ws, "Notes/n.txt"));
}

#[test]
fn pushing_a_local_only_branch_creates_it_on_the_server() {
    let (env, ws) = seeded();
    env.ok(&ws, "alice", &["switch", "-c", "topic"]);
    commit(&env, &ws, "Source/a.cpp", "topic one", "on topic");
    commit(&env, &ws, "Source/b.cpp", "topic bee", "more on topic");
    let out = env.ok(&ws, "alice", &["push", "-u", "origin", "topic"]);
    assert!(out.contains(" * [new branch]      topic -> topic"), "{out}");
    assert!(
        out.contains("branch 'topic' set up to track 'origin/topic'."),
        "{out}"
    );
    assert_eq!(
        git(&ws, &["rev-parse", "topic"]),
        git(&ws, &["rev-parse", "origin/topic"])
    );
    assert_eq!(
        git(&ws, &["config", "branch.topic.merge"]),
        "refs/heads/topic\n"
    );
    let listed = env.ok(&env.dir("seed"), "alice", &["branch"]);
    assert!(listed.contains("topic"), "{listed}");
    assert_eq!(env.ok(&ws, "alice", &["push"]), "Everything up-to-date\n");

    commit(&env, &ws, "Source/a.cpp", "topic two", "third");
    let out = env.ok(&ws, "alice", &["push"]);
    assert!(out.contains("topic -> topic"), "{out}");
    assert!(!out.contains("[new branch]"), "{out}");
    let status = env.ok(&ws, "alice", &["status", "--short", "--branch"]);
    assert!(status.starts_with("## topic...origin/topic\n"), "{status}");
}

#[test]
fn a_branch_can_be_published_under_another_name() {
    let (env, ws) = seeded();
    env.ok(&ws, "alice", &["switch", "-c", "topic"]);
    commit(&env, &ws, "Source/a.cpp", "topic one", "on topic");
    let out = env.ok(&ws, "alice", &["push", "origin", "topic:renamed"]);
    assert!(
        out.contains(" * [new branch]      topic -> renamed"),
        "{out}"
    );
    assert_eq!(
        git(&ws, &["rev-parse", "origin/renamed"]),
        git(&ws, &["rev-parse", "topic"])
    );
    let err = env.fails(&ws, "alice", &["pull"]);
    assert!(err.contains("no tracking information"), "{err}");
}

#[test]
fn a_new_branch_with_no_commit_of_its_own_is_not_pushed() {
    let (env, ws) = seeded();
    env.ok(&ws, "alice", &["branch", "empty"]);
    let err = env.fails(&ws, "alice", &["push", "origin", "empty"]);
    assert!(err.contains("would create nothing"), "{err}");
}

#[test]
fn a_published_branch_can_be_fetched_by_another_clone_and_deleted_again() {
    let (env, ws) = seeded();
    env.ok(&ws, "alice", &["switch", "-c", "shared"]);
    commit(&env, &ws, "Source/a.cpp", "from one", "one");
    env.ok(&ws, "alice", &["push", "-u", "origin", "shared"]);
    let other = env.dir("other");
    env.ok(
        &env.dir("home"),
        "alice",
        &[
            "clone",
            "--local-store",
            &env.source(REPO),
            other.to_str().unwrap(),
        ],
    );
    let other = other.canonicalize().unwrap();
    env.ok(&other, "alice", &["switch", "shared"]);
    assert_eq!(read(&other, "Source/a.cpp"), "from one");
    env.ok(&ws, "alice", &["push", "--delete", "shared"]);
    assert!(
        !env.ok(&env.dir("seed"), "alice", &["branch"])
            .contains("shared")
    );
}

#[test]
fn a_workspace_without_a_store_deletes_server_branches_through_push() {
    let (env, _) = seeded();
    let plain = env.dir("plain2");
    env.ok(
        &env.dir("home"),
        "alice",
        &[
            "clone",
            "--no-local-store",
            &env.source(REPO),
            plain.to_str().unwrap(),
        ],
    );
    env.ok(&plain, "alice", &["branch", "one"]);
    env.ok(&plain, "alice", &["branch", "two"]);
    let out = env.ok(&plain, "alice", &["push", "--delete", "one"]);
    assert!(out.contains(" - [deleted]         one"), "{out}");
    env.ok(&plain, "alice", &["push", "origin", ":two"]);
    assert_eq!(env.ok(&plain, "alice", &["branch"]), "* main\n");
}
