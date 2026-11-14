//! `pyn account`: email addresses and renames with the real `pyn` binary.

mod common;

use common::{cell_rows, start_with_accounts};

const PASSWORD: &str = "correct horse battery\n";

#[test]
fn addresses_are_added_listed_refused_when_taken_and_removed() {
    let env = start_with_accounts(&["alice", "bob"]);
    let home = env.dir("home");

    assert_eq!(
        env.ok(&home, "alice", &["account", "email", "list"]).trim(),
        "no addresses"
    );
    let out = env.ok(
        &home,
        "alice",
        &["account", "email", "add", "A@Example.org"],
    );
    assert!(out.contains("added a@example.org"), "{out}");

    let listed = env.ok(&home, "alice", &["account", "email", "list"]);
    let rows = cell_rows(&listed);
    assert_eq!(rows[0], ["PRIMARY", "STATUS", "ADDED", "ADDRESS"]);
    assert_eq!(rows[1][0], "pending", "no primary yet: {listed}");
    assert_eq!(rows[1].last().unwrap(), "a@example.org");

    let taken = env.fails(&home, "bob", &["account", "email", "add", "a@example.org"]);
    assert!(taken.contains("email_unavailable"), "{taken}");
    assert!(!taken.contains("alice"), "never says whose: {taken}");

    let unverified = env.fails(
        &home,
        "alice",
        &["account", "email", "primary", "a@example.org"],
    );
    assert!(unverified.contains("must be verified"), "{unverified}");
    let out = env.ok(
        &home,
        "alice",
        &["account", "email", "resend", "a@example.org"],
    );
    assert!(out.contains("sent a new link"), "{out}");

    let out = env.ok(
        &home,
        "alice",
        &["account", "email", "remove", "a@example.org"],
    );
    assert!(out.contains("removed a@example.org"), "{out}");
    let gone = env.fails(
        &home,
        "alice",
        &["account", "email", "remove", "a@example.org"],
    );
    assert!(gone.contains("email_not_found"), "{gone}");
    let again = env.ok(&home, "bob", &["account", "email", "add", "a@example.org"]);
    assert!(
        again.contains("added a@example.org"),
        "a removed address is free again: {again}"
    );
}

#[test]
fn a_rename_warns_asks_for_the_name_and_leaves_a_note_on_old_addresses() {
    let env = start_with_accounts(&["alice"]);
    let home = env.dir("home");
    env.ok(&home, "alice", &["repo", "create", "game", "--no-policy"]);

    let refused = env.run_stdin(
        &home,
        "alice",
        &["account", "rename", "alicia", "--password-stdin"],
        "nope\n",
    );
    assert!(!refused.status.success());
    let text = String::from_utf8_lossy(&refused.stderr);
    for want in [
        "alice becomes free",
        "stops, one repository at a time",
        "not confirmed",
    ] {
        assert!(text.contains(want), "{want}: {text}");
    }
    assert_eq!(
        env.ok(&home, "alice", &["whoami"]).trim(),
        "alice",
        "nothing changed"
    );

    let done = env.run_stdin(
        &home,
        "alice",
        &["account", "rename", "alicia", "--yes", "--password-stdin"],
        PASSWORD,
    );
    let out = String::from_utf8_lossy(&done.stdout);
    assert!(
        done.status.success(),
        "{out}{}",
        String::from_utf8_lossy(&done.stderr)
    );
    assert!(out.contains("renamed alice to alicia"), "{out}");
    assert!(out.contains("update your remotes"), "{out}");
    assert_eq!(env.ok(&home, "alicia", &["whoami"]).trim(), "alicia");

    let old = env.run(&home, "alicia", &["--repo", "alice/game", "summary"]);
    assert!(
        old.status.success(),
        "{}",
        String::from_utf8_lossy(&old.stderr)
    );
    let note = String::from_utf8_lossy(&old.stderr);
    assert!(
        note.contains("alice/game has moved to alicia/game"),
        "the CLI says to update the remote: {note}"
    );
    let direct = env.run(&home, "alicia", &["--repo", "alicia/game", "summary"]);
    assert!(direct.status.success());
    assert!(!String::from_utf8_lossy(&direct.stderr).contains("moved"));
}

#[test]
fn a_rename_needs_the_right_password_and_a_free_name() {
    let env = start_with_accounts(&["alice", "bob"]);
    let home = env.dir("home");
    let wrong = env.run_stdin(
        &home,
        "alice",
        &["account", "rename", "alicia", "--yes", "--password-stdin"],
        "not the password\n",
    );
    assert!(String::from_utf8_lossy(&wrong.stderr).contains("password is wrong"));
    let taken = env.run_stdin(
        &home,
        "alice",
        &["account", "rename", "bob", "--yes", "--password-stdin"],
        PASSWORD,
    );
    assert!(String::from_utf8_lossy(&taken.stderr).contains("user_exists"));
    assert_eq!(env.ok(&home, "alice", &["whoami"]).trim(), "alice");
}
