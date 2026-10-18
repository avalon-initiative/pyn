//! `pyn migrate`, the default local clone, and the server capability check for local commits.

mod common;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};

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

fn legacy(env: &Env) -> PathBuf {
    env.seed("alice", "Source/a.cpp", "one", None);
    clone_with(env, "ws", &["--no-local-store"])
}

#[test]
fn a_clean_legacy_workspace_migrates_and_then_commits_locally() {
    let env = start();
    let ws = legacy(&env);
    assert!(!store(&ws).exists());
    let out = env.ok(&ws, "alice", &["migrate"]);
    assert!(out.contains("Migrated"), "{out}");
    assert!(store(&ws).join("HEAD").is_file());
    let config = std::fs::read_to_string(ws.join(".pyn/local_only/config.toml")).unwrap();
    assert!(config.contains("local_store = \"true\""), "{config}");

    std::fs::write(ws.join("Source/a.cpp"), "two").unwrap();
    env.ok(&ws, "alice", &["add", "Source/a.cpp"]);
    let out = env.ok(&ws, "alice", &["commit", "-m", "local"]);
    assert!(out.contains("not yet published"), "{out}");
    let log = env.ok(&ws, "alice", &["log"]);
    assert!(
        !log.contains("local"),
        "nothing reached the server yet: {log}"
    );
    env.ok(&ws, "alice", &["push"]);
    let log = env.ok(&ws, "alice", &["log"]);
    assert!(log.contains("local"), "{log}");
}

#[test]
fn migrating_twice_changes_nothing() {
    let env = start();
    let ws = legacy(&env);
    env.ok(&ws, "alice", &["migrate"]);
    let config = std::fs::read_to_string(ws.join(".pyn/local_only/config.toml")).unwrap();
    let out = env.ok(&ws, "alice", &["migrate"]);
    assert!(out.contains("already keeps local commits"), "{out}");
    assert_eq!(
        std::fs::read_to_string(ws.join(".pyn/local_only/config.toml")).unwrap(),
        config
    );
}

#[test]
fn a_default_clone_is_already_migrated() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "one", None);
    let ws = clone_with(&env, "ws", &[]);
    let out = env.ok(&ws, "alice", &["migrate"]);
    assert!(out.contains("already keeps local commits"), "{out}");
}

#[test]
fn a_workspace_with_working_changes_is_refused_and_left_alone() {
    let env = start();
    let ws = legacy(&env);
    std::fs::set_permissions(
        ws.join("Source/a.cpp"),
        std::os::unix::fs::PermissionsExt::from_mode(0o644),
    )
    .unwrap();
    std::fs::write(ws.join("Source/a.cpp"), "edited").unwrap();
    let err = env.fails(&ws, "alice", &["migrate"]);
    assert!(err.contains("changed in the working tree"), "{err}");
    assert!(err.contains("nothing was changed"), "{err}");
    assert!(!store(&ws).exists());
    let config = std::fs::read_to_string(ws.join(".pyn/local_only/config.toml")).unwrap();
    assert!(!config.contains("local_store"), "{config}");
    assert_eq!(
        std::fs::read_to_string(ws.join("Source/a.cpp")).unwrap(),
        "edited"
    );
}

#[test]
fn staged_changes_and_a_stale_workspace_are_refused() {
    let env = start();
    let ws = legacy(&env);
    std::fs::write(ws.join("Source/new.cpp"), "new").unwrap();
    env.ok(&ws, "alice", &["add", "Source/new.cpp"]);
    let err = env.fails(&ws, "alice", &["migrate"]);
    assert!(err.contains("staged change"), "{err}");
    env.ok(&ws, "alice", &["commit", "-m", "direct"]);

    env.seed("alice", "Source/b.cpp", "bee", None);
    let err = env.fails(&ws, "alice", &["migrate"]);
    assert!(err.contains("not up to date"), "{err}");
    env.ok(&ws, "alice", &["update"]);
    env.ok(&ws, "alice", &["migrate"]);
    assert!(store(&ws).join("HEAD").is_file());
}

/// A server that answers every request with 404, like one from before local commits.
fn old_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut stream = stream;
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let body = r#"{"code":"not_found","message":"not found"}"#;
            let _ = write!(
                stream,
                "HTTP/1.1 404 Not Found\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    format!("http://{addr}")
}

#[test]
fn local_mode_is_refused_against_an_older_server() {
    let env = start();
    let url = old_server();
    let dest = env.dir("ws");
    let run = |extra: &[&str]| {
        let mut args = vec!["--server", url.as_str(), "clone"];
        args.extend(extra);
        args.extend(["alice/game", dest.to_str().unwrap()]);
        env.run(&env.dir("home"), "alice", &args)
    };
    let out = run(&[]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("too old for local commits"), "{err}");
    assert!(err.contains("--no-local-store"), "{err}");
    assert!(!dest.join(".pyn").exists());
}

#[test]
fn migrate_is_refused_against_an_older_server() {
    let env = start();
    let ws = legacy(&env);
    let url = old_server();
    let out = env.run(&ws, "alice", &["--server", url.as_str(), "migrate"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("too old for local commits"), "{err}");
    assert!(!store(&ws).exists());
}
