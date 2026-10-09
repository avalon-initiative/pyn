//! Every listing command prints a header row and aligned columns with the longest-growing column last.

mod common;

use common::{cell_rows, start, start_empty};

#[test]
fn locks_files_and_history_have_stable_columns() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "a", None);
    let home = env.dir("home");
    env.ok(&home, "alice", &["checkout", "Content/m.umap"]);

    let locks = cell_rows(&env.ok(&home, "alice", &["locks"]));
    assert_eq!(locks[0], ["OWNER", "EXPIRES", "PATH"]);
    assert_eq!(locks[1][0], "alice");
    assert_eq!(locks[1][2], "Content/m.umap");

    let files = cell_rows(&env.ok(&home, "alice", &["files"]));
    assert_eq!(files[0], ["MODE", "REV", "LOCKED BY", "PATH"]);
    assert!(
        files
            .iter()
            .any(|r| r == &["shared", "r1", "-", "Source/a.cpp"])
    );
    assert!(
        files
            .iter()
            .any(|r| r == &["exclusive", "-", "alice", "Content/m.umap"])
    );

    let history = cell_rows(&env.ok(&home, "alice", &["history", "Source/a.cpp"]));
    assert_eq!(history[0], ["REV", "AUTHOR", "WHEN", "MESSAGE"]);
    assert_eq!(history[1], ["r1", "alice", history[1][2].as_str(), "seed"]);
}

#[test]
fn empty_listings_say_so_instead_of_printing_a_bare_header() {
    let env = start();
    let home = env.dir("home");
    for (args, text) in [
        (vec!["locks"], "no locks"),
        (vec!["files"], "no files"),
        (vec!["history", "nope.txt"], "no history"),
        (vec!["token", "list"], "no tokens"),
        (vec!["invite", "list"], "no invitations"),
        (vec!["config", "list"], "no settings"),
    ] {
        assert_eq!(env.ok(&home, "alice", &args).trim(), text, "{args:?}");
    }
}

#[test]
fn administration_listings_have_stable_columns() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "a", None);
    let home = env.dir("home");

    env.ok(&home, "alice", &["member", "set", "bob", "writer"]);
    let members = cell_rows(&env.ok(&home, "alice", &["member", "list"]));
    assert_eq!(members[0], ["ROLE", "USER"]);
    assert!(members.iter().any(|r| r == &["writer", "bob"]));

    let roles = cell_rows(&env.ok(&home, "alice", &["role", "list"]));
    assert_eq!(roles[0], ["ROLE", "PERMISSIONS"]);

    env.ok(&home, "alice", &["invite", "create", "--role", "writer"]);
    let invites = cell_rows(&env.ok(&home, "alice", &["invite", "list"]));
    assert_eq!(invites[0], ["ID", "ROLE", "EXPIRES", "STATE"]);
    assert_eq!(
        (&invites[1][1][..], &invites[1][3][..]),
        ("writer", "unused")
    );

    env.ok(
        &home,
        "alice",
        &["token", "create", "ci", "--permissions", "read"],
    );
    let tokens = cell_rows(&env.ok(&home, "alice", &["token", "list"]));
    assert_eq!(tokens[0], ["ID", "STATE", "EXPIRES", "NAME", "PERMISSIONS"]);
    assert_eq!(tokens[1][1], "active");
    assert_eq!(tokens[1][3..], ["ci", "read"]);

    let audit = cell_rows(&env.ok(&home, "alice", &["audit"]));
    assert_eq!(
        audit[0],
        ["ID", "WHEN", "ACTOR", "ACTION", "PATH", "DETAIL"]
    );
    assert!(audit.len() > 1);
}

#[test]
fn config_list_is_a_table() {
    let env = start_empty();
    let home = env.dir("home");
    env.ok(
        &home,
        "alice",
        &["config", "set", "--global", "user", "alice"],
    );
    let rows = cell_rows(&env.ok(&home, "alice", &["config", "list"]));
    assert_eq!(rows[0], ["KEY", "SCOPE", "VALUE"]);
    assert!(rows.iter().any(|r| r == &["user", "user", "alice"]));
}
