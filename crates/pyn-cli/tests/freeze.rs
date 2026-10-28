//! Owner freeze from the command line: an administrator freezes, writes are refused with `owner_frozen`, reads continue.

mod common;

use common::start_uninitialised;

const TOKEN: &str = "0123456789abcdef0123456789abcdef";
const PASSWORD: &str = "correct horse battery";

fn set_up() -> (common::Env, std::path::PathBuf) {
    let env = start_uninitialised(TOKEN);
    let home = env.dir("home");
    let input = format!("{PASSWORD}\n");
    let out = env.run_stdin(
        &home,
        "-",
        &[
            "setup",
            "root",
            "--setup-token",
            TOKEN,
            "--registration",
            "open",
            "--password-stdin",
        ],
        &input,
    );
    assert!(out.status.success());
    let out = env.run_stdin(
        &home,
        "-",
        &["register", "alice", "--password-stdin"],
        &input,
    );
    assert!(out.status.success());
    (env, home)
}

fn sign_in(env: &common::Env, home: &std::path::Path, name: &str) {
    let out = env.run_stdin(
        home,
        "-",
        &["login", name, "--password-stdin"],
        &format!("{PASSWORD}\n"),
    );
    assert!(out.status.success());
}

#[test]
fn a_frozen_owner_cannot_write_but_reads_and_the_state_shows() {
    let (env, home) = set_up();
    sign_in(&env, &home, "alice");
    env.ok(&home, "-", &["repo", "create", "game", "--no-policy"]);
    env.ok(&home, "-", &["logout"]);

    let out = env.ok(
        &home,
        "root",
        &["admin", "freeze", "alice", "--reason", "plan lapsed"],
    );
    assert!(out.contains("alice is frozen since"), "{out}");
    assert!(out.contains("plan lapsed"), "{out}");

    sign_in(&env, &home, "alice");
    let refused = env.fails(&home, "-", &["repo", "create", "other", "--no-policy"]);
    assert!(refused.contains("owner_frozen"), "{refused}");
    assert!(refused.contains("plan lapsed"), "{refused}");
    let refused = env.fails(&home, "-", &["repo", "visibility", "alice/game", "public"]);
    assert!(refused.contains("owner_frozen"), "{refused}");

    let listed = env.ok(&home, "-", &["repo", "list"]);
    assert!(listed.contains("alice/game"), "{listed}");
    let usage = env.ok(&home, "-", &["usage"]);
    assert!(usage.contains("alice is frozen since"), "{usage}");
    let shown = env.ok(&home, "-", &["admin", "frozen", "alice"]);
    assert!(shown.contains("plan lapsed"), "{shown}");
    let denied = env.fails(&home, "-", &["admin", "unfreeze", "alice"]);
    assert!(denied.contains("server_admin_required"), "{denied}");
    env.ok(&home, "-", &["logout"]);

    let out = env.ok(&home, "root", &["admin", "unfreeze", "alice"]);
    assert!(out.contains("alice is not frozen"), "{out}");
    sign_in(&env, &home, "alice");
    env.ok(&home, "-", &["repo", "create", "other", "--no-policy"]);
    let usage = env.ok(&home, "-", &["usage"]);
    assert!(!usage.contains("frozen"), "{usage}");
}

#[test]
fn a_freeze_needs_a_reason() {
    let (env, home) = set_up();
    let missing = env.run(&home, "root", &["admin", "freeze", "alice"]);
    assert!(!missing.status.success());
    let blank = env.fails(
        &home,
        "root",
        &["admin", "freeze", "alice", "--reason", " "],
    );
    assert!(blank.contains("invalid_request"), "{blank}");
}
