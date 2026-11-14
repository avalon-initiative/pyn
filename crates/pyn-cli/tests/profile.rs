//! `pyn profile` and `pyn whoami` against a real server.

mod common;

use common::start_with_accounts;

fn png() -> Vec<u8> {
    let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
    b.resize(80, 1);
    b
}

#[test]
fn a_profile_is_set_shown_and_whoami_names_the_display_name() {
    let env = start_with_accounts(&["alice", "bob"]);
    let home = env.dir("home");

    assert_eq!(env.ok(&home, "alice", &["whoami"]).trim(), "alice");
    let out = env.ok(
        &home,
        "alice",
        &[
            "profile",
            "set",
            "--name",
            "Alice A.",
            "--bio",
            "builds tools",
            "--location",
            "Lisbon",
            "--link",
            "https://a.example",
            "--link",
            "https://b.example",
        ],
    );
    assert!(out.contains("profile updated"), "{out}");
    assert_eq!(
        env.ok(&home, "alice", &["whoami"]).trim(),
        "alice (Alice A.)"
    );

    env.ok(&home, "alice", &["profile", "set", "--company", "Acme"]);
    let own = env.ok(&home, "alice", &["profile", "show"]);
    for want in [
        "Name:         Alice A.",
        "Bio:          builds tools",
        "Location:     Lisbon",
        "Company:      Acme",
        "Links:        https://a.example, https://b.example",
        "Repositories (0)",
    ] {
        assert!(own.contains(want), "{want:?} in\n{own}");
    }
    let seen_by_bob = env.ok(&home, "bob", &["profile", "show", "alice"]);
    assert_eq!(seen_by_bob, own, "the same page for anyone");

    env.ok(
        &home,
        "alice",
        &["profile", "set", "--name", "", "--link", ""],
    );
    let cleared = env.ok(&home, "alice", &["profile", "show"]);
    assert!(
        !cleared.contains("Name:") && !cleared.contains("Links:"),
        "{cleared}"
    );
    assert!(cleared.contains("Company:      Acme"), "{cleared}");
}

#[test]
fn profile_set_refuses_bad_values_and_an_empty_call() {
    let env = start_with_accounts(&["alice"]);
    let home = env.dir("home");
    let err = env.fails(&home, "alice", &["profile", "set"]);
    assert!(err.contains("nothing to change"), "{err}");
    let err = env.fails(
        &home,
        "alice",
        &["profile", "set", "--link", "javascript:alert(1)"],
    );
    assert!(err.contains("invalid_request"), "{err}");
    let err = env.fails(
        &home,
        "alice",
        &["profile", "set", "--bio", &"x".repeat(161)],
    );
    assert!(err.contains("invalid_request"), "{err}");
    let err = env.fails(&home, "alice", &["profile", "show", "nobody"]);
    assert!(err.contains("user_not_found"), "{err}");
}

#[test]
fn the_profile_lists_repositories_with_their_visibility() {
    let env = start_with_accounts(&["alice"]);
    let home = env.dir("home");
    env.ok(&home, "alice", &["repo", "create", "hidden", "--no-policy"]);
    env.ok(&home, "alice", &["repo", "create", "open", "--no-policy"]);
    env.ok(
        &home,
        "alice",
        &["repo", "visibility", "alice/open", "public"],
    );

    let alice = env.ok(&home, "alice", &["profile", "show"]);
    assert!(
        alice.contains("Repositories (2)") && alice.contains("alice/hidden"),
        "{alice}"
    );
}

#[test]
fn an_avatar_is_uploaded_checked_and_removed() {
    let env = start_with_accounts(&["alice"]);
    let home = env.dir("home");
    let good = home.join("me.png");
    std::fs::write(&good, png()).unwrap();
    let bad = home.join("me.gif");
    std::fs::write(&bad, b"GIF89a....").unwrap();
    let huge = home.join("huge.png");
    let mut big = png();
    big.resize((1 << 20) + 1, 0);
    std::fs::write(&huge, big).unwrap();

    let err = env.fails(
        &home,
        "alice",
        &["profile", "avatar", "set", bad.to_str().unwrap()],
    );
    assert!(err.contains("unsupported_image"), "{err}");
    let err = env.fails(
        &home,
        "alice",
        &["profile", "avatar", "set", huge.to_str().unwrap()],
    );
    assert!(err.contains("object_too_large"), "{err}");
    let err = env.fails(&home, "alice", &["profile", "avatar", "set", "missing.png"]);
    assert!(err.contains("reading"), "{err}");

    env.ok(
        &home,
        "alice",
        &["profile", "avatar", "set", good.to_str().unwrap()],
    );
    let shown = env.ok(&home, "alice", &["profile", "show"]);
    assert!(
        shown.contains("Avatar:       /v1/users/alice/avatar?v="),
        "{shown}"
    );
    env.ok(&home, "alice", &["profile", "avatar", "remove"]);
    let shown = env.ok(&home, "alice", &["profile", "show"]);
    assert!(!shown.contains("Avatar:"), "{shown}");
}
