//! Server administration from the command line: setup, accounts, administrators, service credentials, organizations.

mod common;

use common::{cell_rows, start_uninitialised};

const TOKEN: &str = "0123456789abcdef0123456789abcdef";
const PASSWORD: &str = "correct horse battery";

fn text(out: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn setup_creates_the_administrator_once_and_does_not_sign_in() {
    let env = start_uninitialised(TOKEN);
    let home = env.dir("home");
    let input = format!("{PASSWORD}\n");

    let wrong = env.run_stdin(
        &home,
        "-",
        &["setup", "root", "--setup-token", "nope", "--password-stdin"],
        &input,
    );
    assert!(!wrong.status.success());
    assert!(
        text(&wrong).contains("invalid_setup_token"),
        "{}",
        text(&wrong)
    );

    let missing = env.run_stdin(&home, "-", &["setup", "root", "--password-stdin"], &input);
    assert!(!missing.status.success());
    assert!(
        text(&missing).contains("--setup-token"),
        "{}",
        text(&missing)
    );

    let done = env.run_stdin(
        &home,
        "-",
        &[
            "setup",
            "root",
            "--setup-token",
            TOKEN,
            "--server-name",
            "Studio",
            "--registration",
            "open",
            "--password-stdin",
        ],
        &input,
    );
    assert!(done.status.success(), "{}", text(&done));
    assert!(text(&done).contains("root as its first administrator"));

    let again = env.run_stdin(
        &home,
        "-",
        &["setup", "root", "--setup-token", TOKEN, "--password-stdin"],
        &input,
    );
    assert!(!again.status.success());
    assert!(text(&again).contains("already set up"), "{}", text(&again));

    let listed = cell_rows(&env.ok(&home, "root", &["admin", "user", "list"]));
    assert_eq!(listed[0], ["STATUS", "ROLE", "CREATED", "EMAIL", "USER"]);
    assert_eq!(listed[1][0], "active");
    assert_eq!(listed[1][1], "admin");
    assert_eq!(listed[1].last().unwrap(), "root");
}

fn set_up() -> (common::Env, std::path::PathBuf) {
    let env = start_uninitialised(TOKEN);
    let home = env.dir("home");
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
        &format!("{PASSWORD}\n"),
    );
    assert!(out.status.success(), "{}", text(&out));
    for name in ["alice", "bob"] {
        let out = env.run_stdin(
            &home,
            "-",
            &["register", name, "--password-stdin"],
            &format!("{PASSWORD}\n"),
        );
        assert!(out.status.success(), "{}", text(&out));
    }
    (env, home)
}

#[test]
fn administrators_are_granted_and_the_last_one_cannot_be_revoked() {
    let (env, home) = set_up();

    let login = env.run_stdin(
        &home,
        "-",
        &["login", "alice", "--password-stdin"],
        &format!("{PASSWORD}\n"),
    );
    assert!(login.status.success(), "{}", text(&login));
    let denied = env.fails(&home, "-", &["admin", "user", "list"]);
    assert!(denied.contains("server_admin_required"), "{denied}");
    env.ok(&home, "-", &["logout"]);

    let out = env.ok(&home, "root", &["admin", "grant", "alice"]);
    assert!(out.contains("alice is a server administrator"), "{out}");
    env.ok(&home, "alice", &["admin", "user", "list"]);

    let listed = cell_rows(&env.ok(&home, "root", &["admin", "user", "list"]));
    let roles: Vec<_> = listed[1..]
        .iter()
        .map(|r| (r.last().unwrap().as_str(), r[1].as_str()))
        .collect();
    assert_eq!(roles, [("root", "admin"), ("alice", "admin"), ("bob", "-")]);

    let out = env.ok(&home, "alice", &["admin", "revoke", "root"]);
    assert!(out.contains("root is not a server administrator"), "{out}");
    let last = env.fails(&home, "alice", &["admin", "revoke", "alice"]);
    assert!(last.contains("last_server_admin"), "{last}");
    let missing = env.fails(&home, "alice", &["admin", "grant", "nobody"]);
    assert!(missing.contains("user_not_found"), "{missing}");
}

#[test]
fn accounts_are_disabled_and_enabled() {
    let (env, home) = set_up();

    let out = env.ok(
        &home,
        "root",
        &["admin", "user", "disable", "bob", "--reason", "left"],
    );
    assert!(out.contains("disabled bob"), "{out}");
    let listed = cell_rows(&env.ok(
        &home,
        "root",
        &["admin", "user", "list", "--status", "disabled"],
    ));
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[1].last().unwrap(), "bob");

    let out = env.ok(&home, "root", &["admin", "user", "enable", "bob"]);
    assert!(out.contains("enabled bob"), "{out}");
    let bad = env.fails(&home, "root", &["admin", "user", "approve", "bob"]);
    assert!(bad.contains("invalid_request"), "{bad}");
    let waiting = env.ok(
        &home,
        "root",
        &["admin", "user", "list", "--status", "pending_approval"],
    );
    assert_eq!(waiting.trim(), "no accounts");
}

#[test]
fn service_credentials_are_created_listed_used_and_revoked() {
    let (env, home) = set_up();

    let made = env.run(
        &home,
        "root",
        &[
            "admin",
            "service-credential",
            "create",
            "provisioner",
            "--scopes",
            "manage_organizations",
        ],
    );
    assert!(made.status.success(), "{}", text(&made));
    let secret = String::from_utf8_lossy(&made.stdout).trim().to_string();
    assert!(!secret.is_empty());
    assert!(text(&made).contains("only time the secret is shown"));

    let dup = env.fails(
        &home,
        "root",
        &[
            "admin",
            "service-credential",
            "create",
            "provisioner",
            "--scopes",
            "manage_accounts",
        ],
    );
    assert!(dup.contains("service_credential_exists"), "{dup}");

    let list = ["admin", "service-credential", "list"];
    let listed = cell_rows(&env.ok(&home, "root", &list));
    assert_eq!(
        listed[0],
        [
            "STATE",
            "CREATED",
            "LAST USED",
            "FROM",
            "BY",
            "SCOPES",
            "NAME"
        ]
    );
    assert_eq!(listed[1][0], "active");
    assert_eq!(listed[1].last().unwrap(), "provisioner");
    assert!(!listed[1].join(" ").contains(&secret));

    let org = env.run_with_token(
        &home,
        &secret,
        &["admin", "org", "create", "acme", "--owner", "alice"],
    );
    assert!(org.status.success(), "{}", text(&org));
    assert!(text(&org).contains("created organization acme with alice as its owner"));
    let no_scope = env.run_with_token(&home, &secret, &["admin", "user", "list"]);
    assert!(
        text(&no_scope).contains("service_scope_required"),
        "{}",
        text(&no_scope)
    );
    let no_admin = env.run_with_token(&home, &secret, &["admin", "grant", "bob"]);
    assert!(
        text(&no_admin).contains("server_admin_required"),
        "{}",
        text(&no_admin)
    );

    let revoke = ["admin", "service-credential", "revoke", "provisioner"];
    let out = env.ok(&home, "root", &revoke);
    assert!(out.contains("revoked provisioner"), "{out}");
    let listed = cell_rows(&env.ok(&home, "root", &list));
    assert_eq!(listed[1][0], "revoked");
    let after = env.run_with_token(&home, &secret, &["admin", "org", "delete", "acme", "--yes"]);
    assert!(!after.status.success());
}

#[test]
fn a_service_credential_can_be_restricted_by_source_expiry_and_rate() {
    let (env, home) = set_up();
    let create = |name: &str, extra: &[&str]| {
        let mut args = vec![
            "admin",
            "service-credential",
            "create",
            name,
            "--scopes",
            "manage_accounts",
        ];
        args.extend_from_slice(extra);
        let made = env.run(&home, "root", &args);
        assert!(made.status.success(), "{}", text(&made));
        String::from_utf8_lossy(&made.stdout).trim().to_string()
    };
    let list_users = ["admin", "user", "list"];

    let elsewhere = create("elsewhere", &["--allow-from", "192.0.2.0/24,2001:db8::/32"]);
    let refused = env.run_with_token(&home, &elsewhere, &list_users);
    assert!(!refused.status.success());
    assert!(
        text(&refused).contains("unauthenticated"),
        "{}",
        text(&refused)
    );

    let local = create(
        "local",
        &["--allow-from", "127.0.0.1", "--rate-limit", "50"],
    );
    let ok = env.run_with_token(&home, &local, &list_users);
    assert!(ok.status.success(), "{}", text(&ok));

    let soon = create("soon", &["--expires-at", "2099-01-01T00:00:00Z"]);
    assert!(
        env.run_with_token(&home, &soon, &list_users)
            .status
            .success()
    );
    let past = env.fails(
        &home,
        "root",
        &[
            "admin",
            "service-credential",
            "create",
            "past",
            "--scopes",
            "manage_accounts",
            "--expires-at",
            "2001-01-01T00:00:00Z",
        ],
    );
    assert!(past.contains("invalid_request"), "{past}");

    let listed = cell_rows(&env.ok(&home, "root", &["admin", "service-credential", "list"]));
    let row = |name: &str| listed.iter().find(|r| r.last().unwrap() == name).unwrap();
    assert_eq!(row("local")[3], "127.0.0.1");
    assert_eq!(row("elsewhere")[3], "-");
}

#[test]
fn organizations_are_created_for_an_owner_and_deleted_with_confirmation() {
    let (env, home) = set_up();

    let create = ["admin", "org", "create", "acme", "--owner", "alice"];
    let out = env.ok(&home, "root", &create);
    assert!(
        out.contains("created organization acme with alice as its owner"),
        "{out}"
    );
    let listed = cell_rows(&env.ok(&home, "alice", &["org", "list"]));
    assert_eq!(listed[1][0], "owner");
    let nobody = ["admin", "org", "create", "nope-org", "--owner", "nobody"];
    let missing = env.fails(&home, "root", &nobody);
    assert!(missing.contains("user_not_found"), "{missing}");

    let unconfirmed = env.run_stdin(
        &home,
        "root",
        &["admin", "org", "delete", "acme"],
        "wrong\n",
    );
    assert!(!unconfirmed.status.success());
    assert!(text(&unconfirmed).contains("not confirmed"));
    let out = env.ok(&home, "root", &["admin", "org", "delete", "acme", "--yes"]);
    assert!(out.contains("deleted organization acme"), "{out}");
}

#[test]
fn changing_the_password_says_the_saved_sign_in_ended_and_forgets_it() {
    let (env, home) = set_up();
    let new = "another long password";
    let login = env.run_stdin(
        &home,
        "-",
        &["login", "alice", "--password-stdin"],
        &format!("{PASSWORD}\n"),
    );
    assert!(login.status.success(), "{}", text(&login));

    let changed = env.run_stdin(
        &home,
        "-",
        &["password", "--password-stdin"],
        &format!("{PASSWORD}\n{new}\n"),
    );
    let out = text(&changed);
    assert!(changed.status.success(), "{out}");
    assert!(out.contains("password changed"), "{out}");
    assert!(
        out.contains("sign-in") && out.contains("ended") && out.contains("`pyn login alice`"),
        "{out}"
    );
    let after = env.ok(&home, "-", &["logout"]);
    assert!(
        after.contains("not signed in"),
        "no dead token kept: {after}"
    );

    let again = env.run_stdin(
        &home,
        "-",
        &["login", "alice", "--password-stdin"],
        &format!("{new}\n"),
    );
    assert!(again.status.success(), "{}", text(&again));
    let whoami = env.ok(&home, "-", &["whoami"]);
    assert!(whoami.contains("alice"), "{whoami}");
}

#[test]
fn an_access_session_acts_as_an_account_until_it_is_ended() {
    let (env, home) = set_up();
    let start = ["admin", "access-session", "start", "alice"];

    let no_notes = env.fails(
        &home,
        "root",
        &[&start[..], &["--reason", "other"]].concat(),
    );
    assert!(no_notes.contains("invalid_request"), "{no_notes}");
    let bad = env.fails(
        &home,
        "root",
        &[&start[..], &["--reason", "boredom"]].concat(),
    );
    assert!(bad.contains("invalid_request"), "{bad}");

    let made = env.run(
        &home,
        "root",
        &[&start[..], &["--reason", "other", "--notes", "ticket 12"]].concat(),
    );
    assert!(made.status.success(), "{}", text(&made));
    let secret = String::from_utf8_lossy(&made.stdout).trim().to_string();
    assert!(secret.starts_with("pyna_"), "{secret}");
    assert!(text(&made).contains("only time the secret is shown"));

    let acting = env.run_with_token(&home, &secret, &["whoami"]);
    assert!(acting.status.success(), "{}", text(&acting));
    assert!(text(&acting).contains("alice"), "{}", text(&acting));
    let refused = env.run_with_token(&home, &secret, &["token", "list"]);
    assert!(
        text(&refused).contains("access_session_not_allowed"),
        "{}",
        text(&refused)
    );

    let listed = cell_rows(&env.ok(&home, "root", &["admin", "access-session", "list"]));
    assert_eq!(
        listed[0],
        [
            "STATE", "ID", "STARTED", "ENDS", "BY", "REASON", "REQUESTS", "USER"
        ]
    );
    assert_eq!(listed[1][0], "active");
    assert_eq!(listed[1].last().unwrap(), "alice");
    assert!(!listed[1].join(" ").contains(&secret));

    let json = env.ok(
        &home,
        "root",
        &["--json", "admin", "access-session", "list"],
    );
    let doc: serde_json::Value = serde_json::from_str(json.trim()).unwrap();
    assert_eq!(doc["version"], 1);
    let session = &doc["sessions"][0];
    assert_eq!(session["user"], "alice");
    assert_eq!(session["reason"], "other");
    assert_eq!(session["notes"], "ticket 12");
    assert!(session["reached"].as_array().is_some_and(|r| !r.is_empty()));
    let id = session["id"].as_str().unwrap().to_string();

    let ended = env.ok(
        &home,
        "root",
        &["--json", "admin", "access-session", "end", &id],
    );
    let doc: serde_json::Value = serde_json::from_str(ended.trim()).unwrap();
    assert_eq!(doc["session"]["state"], "ended");
    let after = env.run_with_token(&home, &secret, &["whoami"]);
    assert!(!after.status.success());

    let mine = env.ok(&home, "alice", &["--json", "whoami"]);
    assert!(mine.contains("alice"), "{mine}");
}
