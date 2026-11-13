//! Account deletion from the command line: `pyn account`, and `pyn admin user delete`.

mod common;

use common::start_uninitialised;

const TOKEN: &str = "0123456789abcdef0123456789abcdef";
const PASSWORD: &str = "correct horse battery";

fn text(out: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
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

fn login(env: &common::Env, home: &std::path::Path, name: &str) {
    let out = env.run_stdin(
        home,
        "-",
        &["login", name, "--password-stdin"],
        &format!("{PASSWORD}\n"),
    );
    assert!(out.status.success(), "{}", text(&out));
}

#[test]
fn deleting_an_account_needs_the_password_and_restoring_brings_it_back() {
    let (env, home) = set_up();
    login(&env, &home, "alice");

    let status = env.ok(&home, "-", &["account", "status"]);
    assert!(
        status.contains("alice is not scheduled for deletion"),
        "{status}"
    );

    let wrong = env.run_stdin(
        &home,
        "-",
        &["account", "delete", "--password-stdin"],
        "not the password\n",
    );
    assert!(!wrong.status.success());
    assert!(text(&wrong).contains("unauthenticated"), "{}", text(&wrong));

    let done = env.run_stdin(
        &home,
        "-",
        &["account", "delete", "--password-stdin"],
        &format!("{PASSWORD}\n"),
    );
    assert!(done.status.success(), "{}", text(&done));
    let out = text(&done);
    assert!(out.contains("alice is scheduled for deletion on"), "{out}");
    assert!(out.contains("pyn account restore"), "{out}");

    let status = env.ok(&home, "-", &["account", "status"]);
    assert!(
        status.contains("alice is scheduled for deletion on"),
        "{status}"
    );

    let refused = env.fails(&home, "-", &["whoami"]);
    assert!(refused.contains("account_pending_deletion"), "{refused}");
    assert!(refused.contains("pyn account restore"), "{refused}");

    env.ok(&home, "-", &["logout"]);
    login(&env, &home, "alice");
    let restored = env.ok(&home, "-", &["account", "restore"]);
    assert!(restored.contains("restored alice"), "{restored}");
    assert!(env.ok(&home, "-", &["whoami"]).contains("alice"));
    let again = env.fails(&home, "-", &["account", "restore"]);
    assert!(again.contains("not scheduled for deletion"), "{again}");
}

#[test]
fn an_administrator_deletes_a_disabled_account_with_a_reason() {
    let (env, home) = set_up();

    let enabled = env.fails(
        &home,
        "root",
        &["admin", "user", "delete", "alice", "--reason", "spam"],
    );
    assert!(enabled.contains("account_not_disabled"), "{enabled}");
    env.ok(&home, "root", &["admin", "user", "disable", "alice"]);

    let missing = env.run(&home, "root", &["admin", "user", "delete", "alice"]);
    assert!(!missing.status.success(), "a reason is required");
    assert!(text(&missing).contains("--reason"), "{}", text(&missing));

    let scheduled = env.ok(
        &home,
        "root",
        &["admin", "user", "delete", "alice", "--reason", "inactive"],
    );
    assert!(
        scheduled.contains("scheduled for deletion on"),
        "{scheduled}"
    );
    assert!(scheduled.contains("admin user enable alice"), "{scheduled}");

    let done = env.ok(
        &home,
        "root",
        &[
            "admin", "user", "delete", "alice", "--now", "--reason", "inactive",
        ],
    );
    assert!(done.contains("deleted alice"), "{done}");
    let listed = env.ok(&home, "root", &["admin", "user", "list"]);
    assert!(!listed.contains("alice"), "{listed}");

    let again = env.run_stdin(
        &home,
        "-",
        &["register", "alice", "--password-stdin"],
        &format!("{PASSWORD}\n"),
    );
    assert!(!again.status.success());
    assert!(text(&again).contains("name_reserved"), "{}", text(&again));
}
