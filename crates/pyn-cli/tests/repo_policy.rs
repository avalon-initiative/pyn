//! The organization repository-creation policy from the command line.

mod common;

use common::{cell_rows, start_with_accounts};

#[test]
fn owners_show_set_and_edit_the_policy() {
    let env = start_with_accounts(&["alice", "bob"]);
    let home = env.dir("home");
    env.ok(&home, "alice", &["org", "create", "acme"]);
    env.ok(&home, "alice", &["org", "member", "add", "acme", "bob"]);
    env.ok(&home, "alice", &["team", "create", "acme/art"]);

    let shown = env.ok(&home, "alice", &["org", "policy", "show", "acme"]);
    assert!(shown.starts_with("members can create  none"), "{shown}");
    assert!(shown.contains("no rules"), "{shown}");

    let out = env.ok(
        &home,
        "alice",
        &["org", "policy", "set", "acme", "--members", "private"],
    );
    assert!(out.contains("can create: private"), "{out}");

    let out = env.ok(
        &home,
        "alice",
        &["org", "policy", "allow", "acme", "team", "art"],
    );
    assert!(out.contains("allow team art (both repositories)"), "{out}");
    let deny = ["org", "policy", "deny", "acme", "user", "bob"];
    env.ok(
        &home,
        "alice",
        &[&deny[..], &["--scope", "public"]].concat(),
    );
    env.ok(
        &home,
        "alice",
        &[
            "org", "policy", "allow", "acme", "role", "member", "--scope", "private",
        ],
    );
    env.ok(
        &home,
        "alice",
        &["org", "policy", "allow", "acme", "role", "member"],
    );

    let rows: Vec<_> = cell_rows(&env.ok(&home, "alice", &["org", "policy", "show", "acme"]))
        .into_iter()
        .filter(|r| !r.is_empty())
        .collect();
    assert_eq!(rows[0], ["members can create", "private"]);
    assert_eq!(rows[1], ["EFFECT", "KIND", "SCOPE", "SUBJECT"]);
    assert_eq!(rows[2], ["allow", "role", "both", "member"]);
    assert_eq!(rows[3], ["allow", "team", "both", "art"]);
    assert_eq!(rows[4], ["deny", "user", "public", "bob"]);
    assert_eq!(rows.len(), 5);

    let out = env.ok(
        &home,
        "alice",
        &["org", "policy", "remove", "acme", "allow", "team", "art"],
    );
    assert!(out.contains("removed the allow rule for team art"), "{out}");
    let gone = env.fails(
        &home,
        "alice",
        &["org", "policy", "remove", "acme", "allow", "team", "art"],
    );
    assert!(gone.contains("creation_rule_not_found"), "{gone}");
}

#[test]
fn bad_policy_changes_are_refused() {
    let env = start_with_accounts(&["alice", "bob", "carol"]);
    let home = env.dir("home");
    env.ok(&home, "alice", &["org", "create", "acme"]);
    env.ok(&home, "alice", &["org", "member", "add", "acme", "bob"]);

    let owner = env.fails(
        &home,
        "alice",
        &["org", "policy", "deny", "acme", "role", "owner"],
    );
    assert!(owner.contains("invalid_request"), "{owner}");
    let team = env.fails(
        &home,
        "alice",
        &["org", "policy", "allow", "acme", "team", "nope"],
    );
    assert!(team.contains("team_not_found"), "{team}");
    let user = env.fails(
        &home,
        "alice",
        &["org", "policy", "allow", "acme", "user", "carol"],
    );
    assert!(user.contains("user_not_org_member"), "{user}");
    let scope = env.fails(
        &home,
        "alice",
        &[
            "org", "policy", "allow", "acme", "user", "bob", "--scope", "all",
        ],
    );
    assert!(scope.contains("invalid_request"), "{scope}");
    let base = env.fails(
        &home,
        "alice",
        &["org", "policy", "set", "acme", "--members", "some"],
    );
    assert!(base.contains("invalid_request"), "{base}");

    let member = env.fails(&home, "bob", &["org", "policy", "show", "acme"]);
    assert!(member.contains("not_org_owner"), "{member}");
    let set = env.fails(
        &home,
        "bob",
        &["org", "policy", "set", "acme", "--members", "both"],
    );
    assert!(set.contains("not_org_owner"), "{set}");
    let outsider = env.fails(&home, "carol", &["org", "policy", "show", "acme"]);
    assert!(
        outsider.contains("forbidden") || outsider.contains("not_org"),
        "{outsider}"
    );
}

#[test]
fn a_member_is_blocked_then_allowed_and_a_deny_wins() {
    let env = start_with_accounts(&["alice", "bob", "carol"]);
    let home = env.dir("home");
    env.ok(&home, "alice", &["org", "create", "acme"]);
    env.ok(&home, "alice", &["org", "member", "add", "acme", "bob"]);

    let blocked = env.fails(&home, "bob", &["repo", "create", "acme/one"]);
    assert!(
        blocked.contains("acme does not let you create private repositories"),
        "{blocked}"
    );
    assert!(blocked.contains("repo_create_forbidden"), "{blocked}");
    let public = env.fails(
        &home,
        "bob",
        &["repo", "create", "acme/one", "--visibility", "public"],
    );
    assert!(public.contains("create public repositories"), "{public}");
    let outsider = env.fails(&home, "carol", &["repo", "create", "acme/one"]);
    assert!(
        outsider.contains("you are not a member of acme"),
        "{outsider}"
    );

    env.ok(
        &home,
        "alice",
        &[
            "org", "policy", "allow", "acme", "user", "bob", "--scope", "private",
        ],
    );
    let out = env.ok(&home, "bob", &["repo", "create", "acme/one"]);
    assert!(out.contains("created acme/one"), "{out}");
    let still = env.fails(
        &home,
        "bob",
        &["repo", "create", "acme/two", "--visibility", "public"],
    );
    assert!(still.contains("create public repositories"), "{still}");

    env.ok(
        &home,
        "alice",
        &["org", "policy", "deny", "acme", "role", "member"],
    );
    let denied = env.fails(&home, "bob", &["repo", "create", "acme/three"]);
    assert!(denied.contains("repo_create_forbidden"), "{denied}");

    env.ok(
        &home,
        "alice",
        &["org", "policy", "remove", "acme", "deny", "role", "member"],
    );
    env.ok(&home, "bob", &["repo", "create", "acme/three"]);
}
