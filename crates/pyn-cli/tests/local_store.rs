//! The opt-in partial local store of a pyn server workspace (`local_store`), checked against the server's commit
//! ids and, as an oracle for the on-disk format only, real git reading the store.

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

fn seed_history(env: &Env) {
    env.seed("alice", "Source/a.cpp", "one", None);
    env.seed("alice", "Source/b.cpp", "bee", None);
    env.seed("alice", "Source/a.cpp", "two", Some(1));
}

#[test]
fn a_clone_without_the_option_has_no_store() {
    let env = start();
    seed_history(&env);
    let ws = clone_with(&env, "ws", &[]);
    assert!(!store(&ws).exists());
    let config = std::fs::read_to_string(ws.join(".pyn/local_only/config.toml")).unwrap();
    assert!(!config.contains("local_store"), "{config}");
    env.ok(&ws, "alice", &["update"]);
    assert!(!store(&ws).exists());
}

#[test]
fn a_local_store_clone_keeps_every_commit_and_tree_but_only_checked_out_blobs() {
    let env = start();
    seed_history(&env);
    let ws = clone_with(&env, "ws", &["--local-store"]);

    let config = std::fs::read_to_string(ws.join(".pyn/local_only/config.toml")).unwrap();
    assert!(config.contains("local_store = \"true\""), "{config}");

    let ids: Vec<String> = git(&ws, &["rev-list", "main"])
        .lines()
        .map(String::from)
        .collect();
    assert_eq!(ids.len(), 3);
    let log = env.ok(&ws, "alice", &["log", "--commits"]);
    for id in &ids {
        assert!(log.contains(&id[..10]), "{id} missing from\n{log}");
    }
    assert_eq!(
        git(&ws, &["rev-parse", "HEAD", "refs/remotes/origin/main"]),
        format!("{0}\n{0}\n", ids[0])
    );

    let tree = git(&ws, &["ls-tree", "-r", "HEAD"]);
    assert!(
        tree.contains("Source/a.cpp") && tree.contains("Source/b.cpp"),
        "{tree}"
    );
    let first_tree = git(&ws, &["ls-tree", "-r", ids.last().unwrap()]);
    assert!(!first_tree.contains("b.cpp"), "{first_tree}");

    let blob = |text: &str| -> String {
        let out = Command::new("git")
            .arg("--git-dir")
            .arg(store(&ws))
            .args(["cat-file", "-t", text])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    for line in tree.lines() {
        let id = line.split_whitespace().nth(2).unwrap();
        assert_eq!(blob(id), "blob", "checked-out blob {id} is stored");
    }
    let old = first_tree
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(2)
        .unwrap();
    assert_eq!(
        blob(old),
        "",
        "a blob only old history names is not fetched"
    );
}

#[test]
fn update_brings_new_commits_into_the_store() {
    let env = start();
    seed_history(&env);
    let ws = clone_with(&env, "ws", &["--local-store"]);
    env.seed("alice", "Source/c.cpp", "sea", None);
    env.ok(&ws, "alice", &["update"]);

    let ids = git(&ws, &["rev-list", "main"]);
    assert_eq!(ids.lines().count(), 4);
    assert!(git(&ws, &["ls-tree", "-r", "HEAD"]).contains("Source/c.cpp"));
    assert_eq!(
        git(&ws, &["rev-parse", "HEAD"]),
        git(&ws, &["rev-parse", "refs/remotes/origin/main"])
    );
}

#[test]
fn the_setting_turns_the_store_on_for_an_existing_workspace() {
    let env = start();
    seed_history(&env);
    let ws = clone_with(&env, "ws", &[]);
    env.ok(&ws, "alice", &["config", "set", "local_store", "true"]);
    assert_eq!(
        env.ok(&ws, "alice", &["config", "get", "local_store"]),
        "true\n"
    );
    env.ok(&ws, "alice", &["update"]);
    assert_eq!(git(&ws, &["rev-list", "main"]).lines().count(), 3);

    let err = env.fails(&ws, "alice", &["config", "set", "local_store", "maybe"]);
    assert!(err.contains("true or false"), "{err}");
}

#[test]
fn the_user_setting_applies_to_new_clones() {
    let env = start();
    seed_history(&env);
    env.ok(
        &env.dir("home"),
        "alice",
        &["config", "set", "--global", "local_store", "true"],
    );
    let ws = clone_with(&env, "ws", &[]);
    assert!(store(&ws).join("HEAD").is_file());
}

#[test]
fn a_git_clone_refuses_the_option() {
    let env = start();
    let err = env.run(
        &env.dir("home"),
        "alice",
        &["clone", "--local-store", "ssh://host/x.git"],
    );
    assert!(!err.status.success());
    assert!(String::from_utf8_lossy(&err.stderr).contains("--local-store"));
}
