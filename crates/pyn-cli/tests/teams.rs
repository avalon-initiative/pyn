//! Teams from the command line: lifecycle, members, repository grants and where a role comes from.

mod common;

use common::{cell_rows, start_with_accounts};

#[test]
fn teams_are_created_listed_shown_and_deleted() {
    let env = start_with_accounts(&["alice", "bob"]);
    let home = env.dir("home");
    env.ok(&home, "alice", &["org", "create", "acme"]);
    env.ok(&home, "alice", &["org", "member", "add", "acme", "bob"]);

    assert_eq!(
        env.ok(&home, "bob", &["team", "list", "acme"]).trim(),
        "no teams"
    );
    let out = env.ok(
        &home,
        "alice",
        &[
            "team",
            "create",
            "acme/art",
            "--name",
            "Art Team",
            "--description",
            "pixels",
        ],
    );
    assert!(out.contains("created team acme/art"), "{out}");
    env.ok(&home, "alice", &["team", "create", "acme/code"]);

    let dup = env.fails(&home, "alice", &["team", "create", "acme/art"]);
    assert!(dup.contains("team_exists"), "{dup}");
    let denied = env.fails(&home, "bob", &["team", "create", "acme/ops"]);
    assert!(denied.contains("not_org_owner"), "{denied}");
    let bad = env.fails(&home, "alice", &["team", "create", "acme"]);
    assert!(bad.contains("org/team"), "{bad}");

    let listed = cell_rows(&env.ok(&home, "bob", &["team", "list", "acme"]));
    assert_eq!(listed[0], ["MEMBERS", "REPOS", "CREATED", "NAME", "TEAM"]);
    assert_eq!(listed.len(), 3);
    assert_eq!(listed[1][3..], ["Art Team", "art"]);
    assert_eq!(listed[2][3..], ["code", "code"]);

    let shown = env.ok(&home, "bob", &["team", "show", "acme/art"]);
    assert!(shown.contains("team         acme/art"), "{shown}");
    assert!(shown.contains("name         Art Team"), "{shown}");
    assert!(shown.contains("description  pixels"), "{shown}");
    assert!(shown.contains("no members") && shown.contains("no repositories"));
    let missing = env.fails(&home, "alice", &["team", "show", "acme/nope"]);
    assert!(missing.contains("team_not_found"), "{missing}");

    let foreign = env.fails(&home, "bob", &["team", "delete", "acme/art", "--yes"]);
    assert!(foreign.contains("not_org_owner"), "{foreign}");
    let unconfirmed = env.fails(&home, "alice", &["team", "delete", "acme/art"]);
    assert!(unconfirmed.contains("not confirmed"), "{unconfirmed}");
    let out = env.ok(&home, "alice", &["team", "delete", "acme/art", "--yes"]);
    assert!(out.contains("deleted team acme/art"), "{out}");
    let listed = cell_rows(&env.ok(&home, "alice", &["team", "list", "acme"]));
    assert_eq!(listed.len(), 2);
}

#[test]
fn only_organization_members_join_a_team() {
    let env = start_with_accounts(&["alice", "bob", "carol"]);
    let home = env.dir("home");
    env.ok(&home, "alice", &["org", "create", "acme"]);
    env.ok(&home, "alice", &["org", "member", "add", "acme", "bob"]);
    env.ok(&home, "alice", &["team", "create", "acme/art"]);

    let out = env.ok(
        &home,
        "alice",
        &["team", "member", "add", "acme/art", "bob"],
    );
    assert!(out.contains("added bob to acme/art"), "{out}");
    env.ok(
        &home,
        "alice",
        &["team", "member", "add", "acme/art", "bob"],
    );
    let outsider = env.fails(
        &home,
        "alice",
        &["team", "member", "add", "acme/art", "carol"],
    );
    assert!(outsider.contains("user_not_org_member"), "{outsider}");
    let denied = env.fails(
        &home,
        "bob",
        &["team", "member", "add", "acme/art", "alice"],
    );
    assert!(denied.contains("not_org_owner"), "{denied}");

    let shown = cell_rows(&env.ok(&home, "bob", &["team", "show", "acme/art"]));
    assert!(shown.contains(&vec!["bob".to_string()]), "{shown:?}");
    let listed = cell_rows(&env.ok(&home, "bob", &["team", "list", "acme"]));
    assert_eq!(listed[1][..2], ["1", "0"]);

    let out = env.ok(
        &home,
        "alice",
        &["team", "member", "remove", "acme/art", "bob"],
    );
    assert!(out.contains("removed bob from acme/art"), "{out}");
    let again = env.fails(
        &home,
        "alice",
        &["team", "member", "remove", "acme/art", "bob"],
    );
    assert!(again.contains("team_member_not_found"), "{again}");
}

#[test]
fn a_team_is_granted_and_revoked_a_role_and_member_list_shows_the_source() {
    let env = start_with_accounts(&["alice", "bob", "carol"]);
    let home = env.dir("home");
    env.ok(&home, "alice", &["org", "create", "acme"]);
    env.ok(&home, "alice", &["org", "member", "add", "acme", "bob"]);
    env.ok(&home, "alice", &["org", "member", "add", "acme", "carol"]);
    env.ok(&home, "alice", &["repo", "create", "acme/game"]);
    env.ok(&home, "alice", &["repo", "create", "alice/solo"]);
    env.ok(&home, "alice", &["team", "create", "acme/art"]);
    env.ok(
        &home,
        "alice",
        &["team", "member", "add", "acme/art", "bob"],
    );

    let out = env.ok(
        &home,
        "alice",
        &["team", "grant", "acme/art", "acme/game", "--role", "writer"],
    );
    assert!(out.contains("acme/art is now writer on acme/game"), "{out}");
    let personal = env.fails(
        &home,
        "alice",
        &[
            "team",
            "grant",
            "acme/art",
            "alice/solo",
            "--role",
            "reader",
        ],
    );
    assert!(personal.contains("not_org_repo"), "{personal}");

    let access = cell_rows(&env.ok(&home, "alice", &["team", "access", "acme/game"]));
    assert_eq!(access, [["ROLE", "NAME", "TEAM"], ["writer", "art", "art"]]);
    let shown = env.ok(&home, "bob", &["team", "show", "acme/art"]);
    assert!(
        shown.contains("writer") && shown.contains("acme/game"),
        "{shown}"
    );

    env.ok(
        &home,
        "alice",
        &["member", "set", "carol", "reader", "--repo", "acme/game"],
    );
    let members = cell_rows(&env.ok(&home, "alice", &["member", "list", "--repo", "acme/game"]));
    assert_eq!(
        members,
        [
            ["ROLE", "SOURCE", "USER"],
            ["admin", "org_owner", "alice"],
            ["writer", "team", "bob"],
            ["reader", "direct", "carol"]
        ]
    );

    let out = env.ok(&home, "alice", &["team", "revoke", "acme/art", "acme/game"]);
    assert!(
        out.contains("acme/art no longer has a role on acme/game"),
        "{out}"
    );
    env.ok(&home, "alice", &["team", "revoke", "acme/art", "acme/game"]);
    let access = env.ok(&home, "alice", &["team", "access", "acme/game"]);
    assert_eq!(access.trim(), "no teams");
}

#[test]
fn granting_is_capped_by_the_actors_own_permissions() {
    let env = start_with_accounts(&["alice", "bob"]);
    let home = env.dir("home");
    env.ok(&home, "alice", &["org", "create", "acme"]);
    env.ok(&home, "alice", &["org", "member", "add", "acme", "bob"]);
    env.ok(&home, "alice", &["repo", "create", "acme/game"]);
    env.ok(&home, "alice", &["team", "create", "acme/art"]);
    env.ok(
        &home,
        "alice",
        &["member", "set", "bob", "admin", "--repo", "acme/game"],
    );
    let token = env.ok(
        &home,
        "bob",
        &[
            "token",
            "create",
            "narrow",
            "--permissions",
            "read,manage_users",
            "--repos",
            "acme/game",
        ],
    );
    let token = token.trim();
    let grant = |role: &str| {
        env.run_with_token(
            &home,
            token,
            &["team", "grant", "acme/art", "acme/game", "--role", role],
        )
    };

    let capped = grant("admin");
    assert!(!capped.status.success());
    assert!(
        String::from_utf8_lossy(&capped.stderr).contains("forbidden"),
        "{capped:?}"
    );
    assert!(grant("reader").status.success());
    let access = env.ok(&home, "alice", &["team", "access", "acme/game"]);
    assert!(access.contains("reader"), "{access}");
}
