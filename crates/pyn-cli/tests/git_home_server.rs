//! Account and server-level commands run from inside a git repository; commands that act on the repository refuse.

mod common;

use std::path::{Path, PathBuf};

use common::gitfix::{Repo, snapshot};
use common::{Env, cell_rows};

const TOKEN: &str = "0123456789abcdef0123456789abcdef";
const PASSWORD: &str = "correct horse battery";

fn stdin_ok(env: &Env, cwd: &Path, user: &str, args: &[&str], input: &str) -> String {
    let out = env.run_stdin(cwd, user, args, input);
    assert!(
        out.status.success(),
        "pyn {args:?}: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn key(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../pyn-core/tests/fixtures/keys")
        .join(name)
}

#[test]
fn account_and_server_commands_run_inside_a_git_repository() {
    let env = common::start_uninitialised(TOKEN);
    let repo = Repo::new("sha1");
    let cwd = &repo.main;
    let before = snapshot(cwd);
    let input = format!("{PASSWORD}\n");

    stdin_ok(
        &env,
        cwd,
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
    let out = stdin_ok(
        &env,
        cwd,
        "-",
        &["register", "alice", "--password-stdin"],
        &input,
    );
    assert!(out.contains("created alice"), "{out}");
    let out = stdin_ok(
        &env,
        cwd,
        "-",
        &["login", "alice", "--password-stdin"],
        &input,
    );
    assert!(out.contains("signed in as alice"), "{out}");
    assert_eq!(env.ok(cwd, "-", &["whoami"]).trim(), "alice");

    env.ok(cwd, "-", &["repo", "create", "game", "--no-policy"]);
    assert!(env.ok(cwd, "-", &["repo", "list"]).contains("alice/game"));
    env.ok(cwd, "-", &["repo", "explore"]);
    assert!(env.ok(cwd, "-", &["usage"]).contains("alice (user)"));
    env.ok(cwd, "-", &["org", "create", "acme"]);
    assert!(env.ok(cwd, "-", &["org", "list"]).contains("acme"));
    env.ok(cwd, "-", &["team", "create", "acme/art"]);
    assert!(env.ok(cwd, "-", &["team", "list", "acme"]).contains("art"));

    let ed1 = key("ed1.pub");
    env.ok(
        cwd,
        "-",
        &["key", "add", ed1.to_str().unwrap(), "--title", "laptop"],
    );
    let listed = env.ok(cwd, "-", &["key", "list"]);
    assert!(listed.contains("laptop"), "{listed}");
    let id = cell_rows(&listed)[1][0].clone();
    env.ok(cwd, "-", &["key", "remove", &id]);

    let made = env.ok(
        cwd,
        "-",
        &[
            "token",
            "create",
            "ci",
            "--permissions",
            "read",
            "--repos",
            "alice/game",
        ],
    );
    assert!(!made.trim().is_empty());
    assert!(env.ok(cwd, "-", &["token", "list"]).contains("ci"));

    let denied = env.fails(cwd, "-", &["admin", "user", "list"]);
    assert!(denied.contains("server_admin_required"), "{denied}");

    let out = env.run_stdin(
        cwd,
        "-",
        &["password", "--password-stdin"],
        &format!("{PASSWORD}\nnew horse battery staple\n"),
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(String::from_utf8_lossy(&out.stdout).contains("`pyn login alice`"));
    assert!(env.ok(cwd, "-", &["logout"]).contains("not signed in"));
    assert_eq!(snapshot(cwd), before, "a command changed the repository");
}

#[test]
fn user_add_without_a_repository_asks_for_one_on_a_git_home() {
    let env = common::start_with_accounts(&["alice"]);
    let repo = Repo::new("sha1");
    let cwd = &repo.main;
    stdin_ok(
        &env,
        cwd,
        "-",
        &["login", "alice", "--password-stdin"],
        &format!("{PASSWORD}\n"),
    );
    let out = env.run_stdin(
        cwd,
        "-",
        &["user", "add", "bob", "--password-stdin"],
        &format!("{PASSWORD}\n"),
    );
    assert!(!out.status.success());
    let missing = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(!missing.contains("needs a pyn server"), "{missing}");
    assert!(
        missing.contains("this is a git repository, so pass --repo"),
        "{missing}"
    );
}

#[test]
fn repository_commands_take_an_explicit_repository_on_a_git_home() {
    let env = common::start();
    let repo = Repo::new("sha1");
    let before = snapshot(&repo.main);
    let cwd = &repo.main;

    env.ok(
        cwd,
        "alice",
        &["member", "set", "bob", "reader", "--repo", "alice/game"],
    );
    assert!(
        env.ok(cwd, "alice", &["role", "list", "--repo", "alice/game"])
            .contains("reader")
    );
    let invite = env.ok(
        cwd,
        "alice",
        &[
            "invite",
            "create",
            "--role",
            "reader",
            "--repo",
            "alice/game",
        ],
    );
    assert!(!invite.trim().is_empty());
    assert!(
        env.ok(cwd, "alice", &["invite", "list", "--repo", "alice/game"])
            .contains("unused")
    );
    let me = env.ok(cwd, "alice", &["whoami", "--repo", "alice/game"]);
    assert!(me.starts_with("alice\t"), "{me}");
    assert_eq!(snapshot(cwd), before);
}

#[test]
fn repository_commands_without_a_repository_ask_for_one_on_a_git_home() {
    let env = common::start_empty();
    let repo = Repo::new("sha1");
    for args in [
        vec!["member", "list"],
        vec!["role", "list"],
        vec!["invite", "list"],
        vec!["invite", "create"],
        vec!["token", "create", "ci", "--permissions", "read"],
    ] {
        let err = env.fails(&repo.main, "alice", &args);
        assert!(!err.contains("needs a pyn server"), "{args:?}: {err}");
        assert!(
            err.contains("this is a git repository, so "),
            "{args:?}: {err}"
        );
        assert!(
            err.contains("--repo") || err.contains("--repos"),
            "{args:?}: {err}"
        );
    }
}

#[test]
fn commands_that_act_on_the_working_repository_still_refuse() {
    let env = common::start();
    let repo = Repo::new("sha1");
    let before = snapshot(&repo.main);
    for args in [
        vec!["lock", "README.md"],
        vec!["unlock", "README.md"],
        vec!["locks"],
        vec!["policy", "preview", "x.toml"],
        vec!["audit"],
        vec!["verify"],
        vec!["summary"],
    ] {
        let out = env.run(&repo.main, "alice", &args);
        assert_eq!(out.status.code(), Some(128), "{args:?}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.starts_with(&format!("fatal: `pyn {}` needs a pyn server, and this repository is a git repository; `pyn convert` moves it to a pyn server", args[0])),
            "{args:?}: {err}"
        );
    }
    assert_eq!(snapshot(&repo.main), before);
}
