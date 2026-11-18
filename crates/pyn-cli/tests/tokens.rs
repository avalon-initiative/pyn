//! Fine-grained tokens made from the command line against the in-process server.

mod common;

use common::{cell_rows, start, start_with_accounts};

fn row_of(env: &common::Env, home: &std::path::Path, user: &str, name: &str) -> Vec<String> {
    cell_rows(&env.ok(home, user, &["token", "list"]))
        .into_iter()
        .find(|r| r.iter().any(|c| c == name))
        .unwrap_or_else(|| panic!("no token {name}"))
}

#[test]
fn a_token_takes_owner_expiry_access_and_grants() {
    let env = start();
    let home = env.dir("home");
    env.ok(
        &home,
        "alice",
        &[
            "token",
            "create",
            "ci",
            "--permissions",
            "contents:write,locks:admin",
            "--repos",
            "alice/game",
            "--expires",
            "2999-01-31",
        ],
    );
    let row = row_of(&env, &home, "alice", "ci");
    assert_eq!(row[2], "alice");
    assert_eq!(row[4], "selected:alice/game");
    assert!(row[3].contains("2999"), "{row:?}");
    assert_eq!(row[6], "metadata:read,contents:write,locks:admin");

    env.ok(
        &home,
        "alice",
        &[
            "token",
            "create",
            "forever",
            "--permissions",
            "contents:read",
            "--access",
            "all",
            "--never",
        ],
    );
    let row = row_of(&env, &home, "alice", "forever");
    assert_eq!((row[3].as_str(), row[4].as_str()), ("never", "all"));

    env.ok(
        &home,
        "alice",
        &[
            "token",
            "create",
            "keys",
            "--permissions",
            "account_ssh_keys:write",
        ],
    );
    let row = row_of(&env, &home, "alice", "keys");
    assert_eq!(
        (row[4].as_str(), row[6].as_str()),
        ("public", "account_ssh_keys:write")
    );
}

#[test]
fn a_token_can_belong_to_an_organization() {
    let env = start_with_accounts(&["alice"]);
    let home = env.dir("home");
    env.ok(&home, "alice", &["org", "create", "acme"]);
    env.ok(
        &home,
        "alice",
        &[
            "token",
            "create",
            "teams",
            "--permissions",
            "org_teams:write,org_members:read",
            "--owner",
            "acme",
        ],
    );
    let row = row_of(&env, &home, "alice", "teams");
    assert_eq!((row[2].as_str(), row[4].as_str()), ("acme", "public"));
    assert_eq!(row[6], "org_members:read,org_teams:write");

    let err = env.fails(
        &home,
        "alice",
        &["token", "create", "x", "--permissions", "org_teams:read"],
    );
    assert!(err.contains("--owner"), "{err}");
}

#[test]
fn bad_input_is_refused_before_the_server() {
    let env = start();
    let home = env.dir("home");
    let cases: &[(&[&str], &str)] = &[
        (&["--permissions", "contents"], "resource:level"),
        (&["--permissions", "files:read"], "unknown resource"),
        (&["--permissions", "policy:read"], "write only"),
        (
            &["--permissions", "contents:read", "--access", "public"],
            "public",
        ),
        (
            &["--permissions", "account_locks:read", "--access", "all"],
            "repository permission",
        ),
        (
            &["--permissions", "contents:read", "--expires", "soon"],
            "date",
        ),
        (
            &["--permissions", "contents:read", "--expires", "2001-01-01"],
            "past",
        ),
        (
            &["--permissions", "contents:read", "--repos", "a/x,b/y"],
            "one owner",
        ),
    ];
    for (args, want) in cases {
        let mut full = vec!["token", "create", "x"];
        full.extend_from_slice(args);
        let err = env.fails(&home, "alice", &full);
        assert!(err.contains(want), "{args:?}: {err}");
    }
    let both = env.fails(
        &home,
        "alice",
        &[
            "token",
            "create",
            "x",
            "--permissions",
            "contents:read",
            "--never",
            "--expires-days",
            "3",
        ],
    );
    assert!(both.contains("cannot be used with"), "{both}");
}

#[test]
fn the_server_still_refuses_what_the_creator_does_not_hold() {
    let env = start_with_accounts(&["alice", "bob"]);
    let home = env.dir("home");
    env.ok(&home, "alice", &["org", "create", "acme"]);
    let err = env.fails(
        &home,
        "bob",
        &[
            "token",
            "create",
            "x",
            "--permissions",
            "org_teams:read",
            "--owner",
            "acme",
        ],
    );
    assert!(err.contains("not_org_member"), "{err}");
}
