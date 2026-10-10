//! Limits and usage from the command line: opt-in, administrator-set, visible to the owner.

mod common;

use common::{cell_rows, start_uninitialised};

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
    for name in ["alice", "bob"] {
        let out = env.run_stdin(&home, "-", &["register", name, "--password-stdin"], &input);
        assert!(out.status.success());
    }
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
fn a_server_with_no_limits_shows_none_and_nothing_stops_creation() {
    let (env, home) = set_up();
    sign_in(&env, &home, "alice");
    for name in ["one", "two", "three"] {
        env.ok(&home, "-", &["repo", "create", name, "--no-policy"]);
    }
    let out = env.ok(&home, "-", &["usage"]);
    let rows = cell_rows(&out);
    assert_eq!(rows[0], ["alice (user)"]);
    assert_eq!(rows[1], ["USED", "LIMIT", "RESOURCE"]);
    assert_eq!(rows[2], ["3", "-", "repositories"]);
    assert_eq!(rows[3], ["0 B", "-", "storage"]);
    assert_eq!(rows[5], ["STORED", "FILES", "REVISIONS", "REPOSITORY"]);
    assert_eq!(rows[6], ["0 B", "0", "0", "alice/one"]);

    env.ok(&home, "-", &["logout"]);
    let out = env.ok(&home, "root", &["admin", "limits", "list"]);
    assert_eq!(out.trim(), "no owner has limits of its own");
}

#[test]
fn an_administrator_sets_limits_and_creation_is_refused_past_them() {
    let (env, home) = set_up();
    let out = env.ok(
        &home,
        "root",
        &[
            "admin",
            "limits",
            "set",
            "alice",
            "--repos",
            "1",
            "--storage",
            "2K",
        ],
    );
    assert!(out.contains("updated limits for alice"), "{out}");
    let rows = cell_rows(&out);
    assert_eq!(rows[1], ["LIMIT", "SET BY", "RESOURCE"]);
    assert_eq!(rows[2], ["1", "owner", "repositories"]);
    assert_eq!(rows[3], ["2.0 KiB", "owner", "storage"]);

    sign_in(&env, &home, "alice");
    env.ok(&home, "-", &["repo", "create", "one", "--no-policy"]);
    let refused = env.fails(&home, "-", &["repo", "create", "two", "--no-policy"]);
    assert!(refused.contains("repo_limit_reached"), "{refused}");
    let usage = cell_rows(&env.ok(&home, "-", &["usage"]));
    assert_eq!(usage[2], ["1", "1", "repositories"]);
    assert_eq!(usage[3], ["0 B", "2.0 KiB", "storage"]);
    let denied = env.fails(
        &home,
        "-",
        &["admin", "limits", "set", "alice", "--repos", "9"],
    );
    assert!(denied.contains("server_admin_required"), "{denied}");
    let other = env.fails(&home, "-", &["usage", "bob"]);
    assert!(other.contains("not_namespace_owner"), "{other}");
    env.ok(&home, "-", &["logout"]);

    let listed = cell_rows(&env.ok(&home, "root", &["admin", "limits", "list"]));
    assert_eq!(
        listed[0],
        ["REPOSITORIES", "MEMBERS", "STORAGE", "KIND", "OWNER"]
    );
    assert_eq!(listed[1], ["1", "-", "2.0 KiB", "user", "alice"]);

    env.ok(
        &home,
        "root",
        &["admin", "limits", "set", "alice", "--repos", "default"],
    );
    let shown = cell_rows(&env.ok(&home, "root", &["admin", "limits", "show", "alice"]));
    assert_eq!(shown[1], ["LIMIT", "SET BY", "RESOURCE"]);
    assert_eq!(shown[2], ["-", "-", "repositories"]);
    assert_eq!(shown[3], ["2.0 KiB", "owner", "storage"]);
}

#[test]
fn bad_values_and_empty_changes_are_refused_and_repo_usage_reports_a_repository() {
    let (env, home) = set_up();
    let none = env.fails(&home, "root", &["admin", "limits", "set", "alice"]);
    assert!(none.contains("nothing to change"), "{none}");
    let bad = env.fails(
        &home,
        "root",
        &["admin", "limits", "set", "alice", "--repos", "lots"],
    );
    assert!(bad.contains("--repos"), "{bad}");
    let member = env.fails(
        &home,
        "root",
        &["admin", "limits", "set", "alice", "--members", "3"],
    );
    assert!(member.contains("invalid_request"), "{member}");

    sign_in(&env, &home, "alice");
    env.ok(&home, "-", &["repo", "create", "game", "--no-policy"]);
    let rows = cell_rows(&env.ok(&home, "-", &["repo", "usage", "alice/game"]));
    assert_eq!(rows[0], ["STORED", "FILES", "REVISIONS", "REPOSITORY"]);
    assert_eq!(rows[1], ["0 B", "0", "0", "alice/game"]);
}
