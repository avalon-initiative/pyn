//! The read-only default for exclusive files as a repository and organization setting (decision #1062): the policy
//! file wins, then the organization, then on.

#![cfg(unix)]

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use common::{Env, REPO, start, start_with_accounts};

const MAP: &str = "Content/m.umap";
const OFF: &str = "[meta]\ndefault = \"shared\"\nexclusive_read_only = false\n[exclusive]\npaths = [\"Content/\"]\n";
const PLAIN: &str = "[meta]\ndefault = \"shared\"\n[exclusive]\npaths = [\"Content/\"]\n";

fn writable(ws: &Path, path: &str) -> bool {
    std::fs::metadata(ws.join(path))
        .unwrap()
        .permissions()
        .mode()
        & 0o222
        != 0
}

fn clone(env: &Env, repo: &str, name: &str) -> PathBuf {
    let ws = env.dir(name);
    let source = env.source(repo);
    env.ok(
        &env.dir("home"),
        "alice",
        &["clone", source.as_str(), ws.to_str().unwrap()],
    );
    ws.canonicalize().unwrap()
}

fn commit_file(env: &Env, ws: &Path, path: &str, text: &str) {
    let file = env.dir("upload").join("f");
    std::fs::write(&file, text).unwrap();
    env.ok(
        ws,
        "alice",
        &["commit", path, "--file", file.to_str().unwrap(), "-m", "m"],
    );
}

#[test]
fn a_repository_whose_policy_turns_the_default_off_keeps_exclusive_files_writable() {
    let env = start();
    env.seed("alice", ".pyn/pyn.toml", OFF, None);
    env.seed("alice", MAP, "map one", None);
    let ws = clone(&env, REPO, "ws");
    assert!(writable(&ws, MAP), "the policy file turned the default off");
    let json = env.ok(&ws, "alice", &["--json", "status"]);
    assert!(
        json.contains("\"exclusive_read_only\": false")
            || json.contains("\"exclusive_read_only\":false"),
        "{json}"
    );
}

#[test]
fn a_repository_without_the_setting_keeps_exclusive_files_read_only() {
    let env = start();
    env.seed("alice", ".pyn/pyn.toml", PLAIN, None);
    env.seed("alice", MAP, "map one", None);
    let ws = clone(&env, REPO, "ws");
    assert!(!writable(&ws, MAP));
}

#[test]
fn the_organization_setting_applies_where_the_policy_file_is_silent() {
    let env = start_with_accounts(&["alice"]);
    let home = env.dir("home");
    env.ok(&home, "alice", &["org", "create", "acme"]);
    env.ok(
        &home,
        "alice",
        &["repo", "create", "acme/game", "--no-policy"],
    );
    let ws = clone(&env, "acme/game", "ws");
    commit_file(&env, &ws, ".pyn/pyn.toml", PLAIN);
    env.ok(&ws, "alice", &["lock", MAP]);
    std::fs::create_dir_all(ws.join("Content")).unwrap();
    std::fs::write(ws.join(MAP), "map one").unwrap();
    env.ok(&ws, "alice", &["commit", MAP, "-m", "map"]);
    env.ok(&ws, "alice", &["update"]);
    assert!(!writable(&ws, MAP), "on by default");

    let shown = env.ok(&home, "alice", &["org", "settings", "show", "acme"]);
    assert!(shown.contains("unset (on)"), "{shown}");
    let set = env.ok(
        &home,
        "alice",
        &[
            "org",
            "settings",
            "set",
            "acme",
            "--exclusive-read-only",
            "off",
        ],
    );
    assert!(set.contains("off"), "{set}");
    env.ok(&ws, "alice", &["update"]);
    assert!(writable(&ws, MAP), "the organization turned it off");

    env.ok(
        &home,
        "alice",
        &[
            "org",
            "settings",
            "set",
            "acme",
            "--exclusive-read-only",
            "on",
        ],
    );
    env.ok(&ws, "alice", &["update"]);
    assert!(!writable(&ws, MAP), "and back on");

    env.ok(
        &home,
        "alice",
        &[
            "org",
            "settings",
            "set",
            "acme",
            "--exclusive-read-only",
            "off",
        ],
    );
    let fresh = clone(&env, "acme/game", "fresh");
    assert!(writable(&fresh, MAP), "a new clone follows the setting");

    env.ok(&home, "alice", &["org", "settings", "set", "acme"]);
    let shown = env.ok(&home, "alice", &["org", "settings", "show", "acme"]);
    assert!(shown.contains("unset (on)"), "{shown}");
}

#[test]
fn only_organization_owners_read_or_change_the_settings() {
    let env = start_with_accounts(&["alice", "bob"]);
    let home = env.dir("home");
    env.ok(&home, "alice", &["org", "create", "acme"]);
    env.join_org(&home, "alice", "acme", "bob");
    env.fails(&home, "bob", &["org", "settings", "show", "acme"]);
    env.fails(
        &home,
        "bob",
        &[
            "org",
            "settings",
            "set",
            "acme",
            "--exclusive-read-only",
            "off",
        ],
    );
}
