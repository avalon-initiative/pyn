//! Organizations from the command line: create, list, show, delete, members, audit and repositories they own.

mod common;

use common::{cell_rows, start_with_accounts};

#[test]
fn organizations_are_created_listed_shown_and_deleted() {
    let env = start_with_accounts(&["alice", "bob"]);
    let home = env.dir("home");

    assert_eq!(
        env.ok(&home, "alice", &["org", "list"]).trim(),
        "no organizations"
    );
    let out = env.ok(&home, "alice", &["org", "create", "acme"]);
    assert!(out.contains("created organization acme"), "{out}");
    env.ok(&home, "bob", &["org", "create", "beta"]);

    let listed = cell_rows(&env.ok(&home, "alice", &["org", "list"]));
    assert_eq!(listed[0], ["ROLE", "CREATED", "ORGANIZATION"]);
    assert_eq!(listed.len(), 2);
    assert_eq!(
        (listed[1][0].as_str(), listed[1][2].as_str()),
        ("owner", "acme")
    );

    let taken = env.fails(&home, "bob", &["org", "create", "acme"]);
    assert!(taken.contains("user_exists"), "{taken}");
    let taken = env.fails(&home, "bob", &["org", "create", "alice"]);
    assert!(taken.contains("user_exists"), "{taken}");

    let shown = env.ok(&home, "alice", &["org", "show", "acme"]);
    assert!(shown.contains("organization  acme"), "{shown}");
    assert!(shown.contains("your role     owner"), "{shown}");
    assert!(shown.contains("owner  alice"), "{shown}");
    let stranger = env.ok(&home, "bob", &["org", "show", "acme"]);
    assert!(stranger.contains("your role     -"), "{stranger}");
    assert!(!stranger.contains("alice"), "{stranger}");
    let missing = env.fails(&home, "alice", &["org", "show", "nope"]);
    assert!(missing.contains("org_not_found"), "{missing}");

    let foreign = env.fails(&home, "bob", &["org", "delete", "acme", "--yes"]);
    assert!(foreign.contains("not_org_owner"), "{foreign}");
    let out = env.ok(&home, "alice", &["org", "delete", "acme", "--yes"]);
    assert!(out.contains("deleted organization acme"), "{out}");
    assert_eq!(
        env.ok(&home, "alice", &["org", "list"]).trim(),
        "no organizations"
    );
}

#[test]
fn deleting_without_confirmation_needs_the_name_and_does_nothing_otherwise() {
    let env = start_with_accounts(&["alice"]);
    let home = env.dir("home");
    env.ok(&home, "alice", &["org", "create", "acme"]);

    let err = env.fails(&home, "alice", &["org", "delete", "acme"]);
    assert!(err.contains("not confirmed"), "{err}");
    assert!(
        env.ok(&home, "alice", &["org", "list"]).contains("acme"),
        "still there"
    );
}

#[test]
fn members_are_added_changed_and_removed_and_the_last_owner_stays() {
    let env = start_with_accounts(&["alice", "bob", "carol"]);
    let home = env.dir("home");
    env.ok(&home, "alice", &["org", "create", "acme"]);

    let out = env.ok(&home, "alice", &["org", "member", "add", "acme", "bob"]);
    assert!(out.contains("added bob to acme as member"), "{out}");
    env.ok(
        &home,
        "alice",
        &["org", "member", "add", "acme", "carol", "--role", "owner"],
    );
    let again = env.fails(&home, "alice", &["org", "member", "add", "acme", "bob"]);
    assert!(again.contains("already_org_member"), "{again}");
    let ghost = env.fails(&home, "alice", &["org", "member", "add", "acme", "zed"]);
    assert!(ghost.contains("user_not_found"), "{ghost}");
    let denied = env.fails(&home, "bob", &["org", "member", "add", "acme", "alice"]);
    assert!(denied.contains("not_org_owner"), "{denied}");

    let listed = env.ok(&home, "bob", &["org", "member", "list", "acme"]);
    assert_eq!(
        cell_rows(&listed),
        [
            ["ROLE", "USER"],
            ["owner", "alice"],
            ["member", "bob"],
            ["owner", "carol"]
        ]
    );

    let out = env.ok(
        &home,
        "alice",
        &["org", "member", "set", "acme", "bob", "owner"],
    );
    assert!(out.contains("bob is now owner of acme"), "{out}");
    let bad = env.fails(
        &home,
        "alice",
        &["org", "member", "set", "acme", "bob", "boss"],
    );
    assert!(bad.contains("invalid"), "{bad}");

    env.ok(
        &home,
        "alice",
        &["org", "member", "set", "acme", "bob", "member"],
    );
    let out = env.ok(&home, "bob", &["org", "member", "remove", "acme", "bob"]);
    assert!(out.contains("removed bob from acme"), "{out}");
    env.ok(
        &home,
        "alice",
        &["org", "member", "remove", "acme", "carol"],
    );

    let demote = env.fails(
        &home,
        "alice",
        &["org", "member", "set", "acme", "alice", "member"],
    );
    assert!(demote.contains("last_org_owner"), "{demote}");
    let leave = env.fails(
        &home,
        "alice",
        &["org", "member", "remove", "acme", "alice"],
    );
    assert!(leave.contains("last_org_owner"), "{leave}");

    let members = cell_rows(&env.ok(&home, "alice", &["org", "member", "list", "acme"]));
    assert_eq!(members, [["ROLE", "USER"], ["owner", "alice"]]);
}

#[test]
fn an_organization_owner_creates_and_lists_its_repositories() {
    let env = start_with_accounts(&["alice", "bob"]);
    let home = env.dir("home");
    env.ok(&home, "alice", &["org", "create", "acme"]);
    env.ok(&home, "alice", &["org", "member", "add", "acme", "bob"]);

    let out = env.ok(&home, "alice", &["repo", "create", "acme/game"]);
    assert!(out.contains("created acme/game"), "{out}");
    let denied = env.fails(&home, "bob", &["repo", "create", "acme/tools"]);
    assert!(denied.contains("not_org_owner"), "{denied}");
    let outsider = env.fails(&home, "bob", &["repo", "create", "beta/tools"]);
    assert!(outsider.contains("not_namespace_owner"), "{outsider}");

    let listed = env.ok(&home, "alice", &["repo", "list", "--owner", "acme"]);
    assert_eq!(
        cell_rows(&listed),
        [
            ["VISIBILITY", "ROLE", "REPOSITORY"],
            ["private", "admin", "acme/game"]
        ]
    );

    let busy = env.fails(&home, "alice", &["org", "delete", "acme", "--yes"]);
    assert!(busy.contains("org_not_empty"), "{busy}");
    env.ok(&home, "alice", &["repo", "delete", "acme/game", "--yes"]);
    env.ok(&home, "alice", &["org", "delete", "acme", "--yes"]);
}

#[test]
fn the_audit_log_is_for_owners() {
    let env = start_with_accounts(&["alice", "bob"]);
    let home = env.dir("home");
    env.ok(&home, "alice", &["org", "create", "acme"]);
    env.ok(&home, "alice", &["org", "member", "add", "acme", "bob"]);

    let log = env.ok(&home, "alice", &["org", "audit", "acme"]);
    let rows = cell_rows(&log);
    assert_eq!(rows[0], ["ID", "WHEN", "ACTOR", "ACTION", "DETAIL"]);
    assert!(
        log.contains("org_member_added") && log.contains("org_created"),
        "{log}"
    );
    assert!(rows[1].contains(&"alice".to_string()), "{log}");

    let denied = env.fails(&home, "bob", &["org", "audit", "acme"]);
    assert!(denied.contains("not_org_owner"), "{denied}");
}
