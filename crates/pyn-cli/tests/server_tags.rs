//! `pyn tag --server`, and `pyn tag` in a workspace without the local store: immutable tags on the pyn server.

mod common;

use std::path::PathBuf;

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

#[test]
fn a_workspace_without_the_local_store_tags_on_the_server() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "one", None);
    let ws = clone_with(&env, "plain", &["--no-local-store"]);

    assert_eq!(env.ok(&ws, "alice", &["tag"]), "");
    let made = env.ok(&ws, "alice", &["tag", "rel/1.0", "-m", "first release"]);
    assert!(made.starts_with("tagged rel/1.0 at "), "{made}");
    env.ok(&ws, "alice", &["tag", "v2", "main"]);
    assert_eq!(env.ok(&ws, "alice", &["tag"]), "rel/1.0\nv2\n");
    assert_eq!(env.ok(&ws, "alice", &["tag", "-l", "rel/*"]), "rel/1.0\n");

    let again = env.fails(&ws, "alice", &["tag", "v2"]);
    assert!(again.contains("tag_exists"), "{again}");
    let forced = env.fails(&ws, "alice", &["tag", "-f", "v2"]);
    assert!(forced.contains("never moves"), "{forced}");
    let bad = env.fails(&ws, "alice", &["tag", "bad name"]);
    assert!(bad.contains("invalid_tag_name"), "{bad}");

    let other = clone_with(&env, "other", &["--no-local-store"]);
    assert_eq!(
        env.ok(&other, "alice", &["tag"]),
        "rel/1.0\nv2\n",
        "tags are the repository's, not the workspace's"
    );
    let deleted = env.ok(&other, "alice", &["tag", "-d", "v2"]);
    assert!(deleted.starts_with("Deleted tag 'v2' (was "), "{deleted}");
    let missing = env.fails(&other, "alice", &["tag", "-d", "v2"]);
    assert!(missing.contains("tag_not_found"), "{missing}");
    assert_eq!(env.ok(&ws, "alice", &["tag"]), "rel/1.0\n");
}

#[test]
fn server_flag_keeps_the_local_store_tags_apart() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "one", None);
    let ws = clone_with(&env, "local", &[]);

    env.ok(&ws, "alice", &["tag", "mine"]);
    env.ok(&ws, "alice", &["tag", "--server", "shared"]);
    assert_eq!(env.ok(&ws, "alice", &["tag"]), "mine\n");
    assert_eq!(env.ok(&ws, "alice", &["tag", "--server", "-l"]), "shared\n");
    env.ok(&ws, "alice", &["tag", "--server", "-d", "shared"]);
    assert_eq!(env.ok(&ws, "alice", &["tag", "--server"]), "");
}
