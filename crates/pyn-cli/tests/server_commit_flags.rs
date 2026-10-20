//! `pyn commit -F` and `--allow-empty` on a pyn server home, with and without the local store.

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
    assert!(out.status.success(), "git {args:?}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn seeded(extra: &[&str]) -> (Env, PathBuf) {
    let env = start();
    env.seed("alice", "Source/a.cpp", "one", None);
    env.seed("alice", "Content/m.umap", "map", None);
    let ws = clone_with(&env, "ws", extra);
    (env, ws)
}

fn stage_edit(env: &Env, ws: &Path) {
    std::fs::write(ws.join("Source/a.cpp"), "two").unwrap();
    env.ok(ws, "alice", &["add", "Source/a.cpp"]);
}

#[test]
fn a_message_file_is_the_commit_message() {
    let (env, ws) = seeded(&["--local-store"]);
    stage_edit(&env, &ws);
    std::fs::write(ws.join("msg.txt"), "\n\nsubject  \n\n\nbody\n\n").unwrap();
    env.ok(&ws, "alice", &["commit", "-F", "msg.txt"]);
    assert_eq!(
        git(&ws, &["log", "-1", "--format=%B", "main"]),
        "subject\n\nbody\n\n"
    );
}

#[test]
fn dash_reads_the_message_from_standard_input() {
    let (env, ws) = seeded(&["--local-store"]);
    stage_edit(&env, &ws);
    let out = env.run_stdin(&ws, "alice", &["commit", "-F", "-"], "from stdin\n");
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        git(&ws, &["log", "-1", "--format=%s", "main"]),
        "from stdin\n"
    );
}

#[test]
fn a_missing_message_file_is_an_error_and_commits_nothing() {
    let (env, ws) = seeded(&["--local-store"]);
    stage_edit(&env, &ws);
    let tip = git(&ws, &["rev-parse", "main"]);
    let err = env.fails(&ws, "alice", &["commit", "-F", "nope.txt"]);
    assert!(err.contains("could not read log file 'nope.txt'"), "{err}");
    assert_eq!(git(&ws, &["rev-parse", "main"]), tip);
}

#[test]
fn an_empty_message_file_aborts_as_git_does() {
    let (env, ws) = seeded(&["--local-store"]);
    stage_edit(&env, &ws);
    std::fs::write(ws.join("msg.txt"), " \n\n").unwrap();
    let err = env.fails(&ws, "alice", &["commit", "-F", "msg.txt"]);
    assert!(
        err.contains("Aborting commit due to empty commit message."),
        "{err}"
    );
}

#[test]
fn a_message_file_and_dash_m_cannot_be_combined() {
    let (env, ws) = seeded(&["--local-store"]);
    stage_edit(&env, &ws);
    std::fs::write(ws.join("msg.txt"), "x\n").unwrap();
    let err = env.fails(&ws, "alice", &["commit", "-F", "msg.txt", "-m", "y"]);
    assert!(
        err.contains("'-m' and '-F' cannot be used together"),
        "{err}"
    );
}

#[test]
fn a_message_file_works_without_the_local_store() {
    let (env, ws) = seeded(&["--no-local-store"]);
    stage_edit(&env, &ws);
    std::fs::write(ws.join("msg.txt"), "published message\n").unwrap();
    env.ok(&ws, "alice", &["commit", "-F", "msg.txt"]);
    let log = env.ok(&ws, "alice", &["log", "--commits"]);
    assert!(log.contains("published message"), "{log}");
}

#[test]
fn an_exclusive_path_still_needs_its_lock_with_a_message_file() {
    let (env, ws) = seeded(&["--local-store"]);
    std::fs::set_permissions(
        ws.join("Content/m.umap"),
        std::os::unix::fs::PermissionsExt::from_mode(0o644),
    )
    .unwrap();
    std::fs::write(ws.join("Content/m.umap"), "edited").unwrap();
    std::fs::write(ws.join("msg.txt"), "x\n").unwrap();
    env.ok(&ws, "bob", &["lock", "Content/m.umap"]);
    let err = env.fails(&ws, "alice", &["add", "Content/m.umap"]);
    assert!(err.contains("locked by bob"), "{err}");
    let err = env.fails(&ws, "alice", &["commit", "-a", "-F", "msg.txt"]);
    assert!(err.contains("locked by bob"), "{err}");
}

#[test]
fn allow_empty_is_refused_because_the_server_refuses_empty_commits() {
    for extra in [&["--local-store"][..], &["--no-local-store"]] {
        let (env, ws) = seeded(extra);
        let out = env.run(&ws, "alice", &["commit", "--allow-empty", "-m", "x"]);
        assert_eq!(out.status.code(), Some(129));
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("not supported on a pyn server"), "{err}");
    }
}
