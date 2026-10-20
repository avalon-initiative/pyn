//! `remote`, `fetch`, `pull`, `push` and `branch -a`/`-r` on a pyn server home, where the one remote is the server.

mod common;

use std::path::PathBuf;

use common::{Env, REPO, start};

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
fn remote_verbose_names_the_server_and_repository() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "one", None);
    let ws = clone(&env, "ws");
    assert_eq!(env.ok(&ws, "alice", &["remote"]), "origin\n");
    let url = env.source(REPO);
    assert_eq!(
        env.ok(&ws, "alice", &["remote", "-v"]),
        format!("origin\t{url} (fetch)\norigin\t{url} (push)\n")
    );
}

#[test]
fn get_url_prints_the_verbose_url() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "one", None);
    let ws = clone(&env, "ws");
    let url = env.source(REPO);
    for args in [
        vec!["remote", "get-url", "origin"],
        vec!["remote", "get-url", "--push", "origin"],
        vec!["remote", "get-url", "--all", "origin"],
    ] {
        assert_eq!(env.ok(&ws, "alice", &args), format!("{url}\n"), "{args:?}");
    }
    let out = env.run(&ws, "alice", &["remote", "get-url", "nope"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("error: No such remote 'nope'"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn editing_subcommands_say_there_is_one_home() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "one", None);
    let ws = clone(&env, "ws");
    for args in [
        vec!["remote", "add", "x", "http://h/o/n"],
        vec!["remote", "remove", "origin"],
        vec!["remote", "rename", "origin", "x"],
        vec!["remote", "set-url", "origin", "http://h/o/n"],
    ] {
        let out = env.run(&ws, "alice", &args);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("exactly one home"),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn fetch_and_pull_run_update() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "one", None);
    let ws = clone(&env, "ws");
    env.seed("alice", "Source/b.cpp", "two", None);
    for cmd in ["fetch", "pull"] {
        let other = clone(&env, &format!("ws-{cmd}"));
        env.seed("alice", &format!("Source/{cmd}.cpp"), "x", None);
        env.ok(&other, "alice", &[cmd]);
        assert!(other.join(format!("Source/{cmd}.cpp")).is_file(), "{cmd}");
    }
    env.ok(&ws, "alice", &["pull"]);
    assert!(ws.join("Source/b.cpp").is_file());
}

#[test]
fn fetch_with_arguments_is_a_usage_error() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "one", None);
    let ws = clone(&env, "ws");
    let out = env.run(&ws, "alice", &["fetch", "--prune"]);
    assert_eq!(out.status.code(), Some(129));
}

#[test]
fn push_says_commits_already_reach_the_server() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "one", None);
    let ws = clone(&env, "ws");
    let out = env.run(&ws, "alice", &["push"]);
    assert!(out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("commits go straight to the server"),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
fn branch_all_and_remotes_list_the_servers_branches() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "one", None);
    let ws = clone(&env, "ws");
    env.ok(&ws, "alice", &["branch", "topic"]);
    assert_eq!(
        env.ok(&ws, "alice", &["branch", "-r"]),
        "  origin/main\n  origin/topic\n"
    );
    assert_eq!(
        env.ok(&ws, "alice", &["branch", "-a"]),
        "* main\n  topic\n  remotes/origin/main\n  remotes/origin/topic\n"
    );
    let verbose = env.ok(&ws, "alice", &["branch", "-r", "-v"]);
    assert!(verbose.starts_with("  origin/main"), "{verbose}");
    assert!(
        verbose.lines().next().unwrap().ends_with("  seed"),
        "{verbose}"
    );
}
