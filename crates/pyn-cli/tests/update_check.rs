//! `pyn version --check` against an in-process server whose release source is a fake.

mod common;

use std::sync::Arc;

use common::start_uninitialised_with_updates;
use pyn_core::SystemClock;
use pyn_server::update_check::{FakeReleaseSource, Release, UpdateChecker};

const TOKEN: &str = "0123456789abcdef0123456789abcdef";
const CURRENT: &str = "0.1.4-alpha";
const PASSWORD: &str = "correct horse battery";

fn release(tag: &str, prerelease: bool) -> Release {
    Release {
        tag: tag.into(),
        prerelease,
        url: format!("https://example.test/releases/{tag}"),
    }
}

/// A set-up server (root is the administrator, alice an ordinary user); `source` answers the lookup when `Some`.
fn world(source: Option<Arc<FakeReleaseSource>>) -> (common::Env, std::path::PathBuf) {
    let checker = source.map(|s| Arc::new(UpdateChecker::new(s, Arc::new(SystemClock), CURRENT)));
    let env = start_uninitialised_with_updates(TOKEN, checker.clone());
    if let Some(c) = &checker {
        env.block_on(c.refresh_if_due());
    }
    let home = env.dir("home");
    let args = ["setup", "root", "--setup-token", TOKEN];
    let mut full = args.to_vec();
    full.extend(["--registration", "open", "--password-stdin"]);
    let out = env.run_stdin(&home, "-", &full, &format!("{PASSWORD}\n"));
    assert!(out.status.success());
    let out = env.run_stdin(
        &home,
        "-",
        &["register", "alice", "--password-stdin"],
        &format!("{PASSWORD}\n"),
    );
    assert!(out.status.success());
    (env, home)
}

#[test]
fn an_administrator_sees_the_newer_release_as_one_line_and_as_json() {
    let source = Arc::new(FakeReleaseSource::new(Ok(vec![
        release("v99.0.0-alpha", true),
        release("v98.0.0", false),
    ])));
    let (env, home) = world(Some(source));
    let version = CURRENT;

    let out = env.run(&home, "root", &["version", "--check"]);
    assert!(out.status.success());
    let line = String::from_utf8_lossy(&out.stdout);
    assert_eq!(line.lines().count(), 1, "{line}");
    assert!(
        line.contains(version) && line.contains("99.0.0-alpha"),
        "{line}"
    );
    assert!(
        line.contains("pre-release") && line.contains("example.test"),
        "{line}"
    );

    let doc: serde_json::Value =
        serde_json::from_str(&env.ok(&home, "root", &["version", "--check", "--json"])).unwrap();
    assert_eq!(doc["version"], 1);
    assert_eq!(doc["update"]["enabled"], true);
    assert_eq!(doc["update"]["current"], version);
    assert_eq!(doc["update"]["latest"], "99.0.0-alpha");
    assert_eq!(doc["update"]["prerelease"], true);
}

#[test]
fn a_server_that_is_up_to_date_says_so_and_exits_zero() {
    let source = Arc::new(FakeReleaseSource::new(Ok(vec![release("v0.0.1", false)])));
    let (env, home) = world(Some(source));
    let out = env.ok(&home, "root", &["version", "--check"]);
    assert!(out.contains("up to date"), "{out}");
    let doc: serde_json::Value =
        serde_json::from_str(&env.ok(&home, "root", &["version", "--check", "--json"])).unwrap();
    assert!(doc["update"].get("latest").is_none());
    assert!(doc["update"]["checked_at"].is_string());
}

#[test]
fn an_off_check_and_a_non_administrator_are_each_told_plainly_with_exit_zero() {
    let (env, home) = world(None);
    let off = env.ok(&home, "root", &["version", "--check"]);
    assert_eq!(off.lines().count(), 1, "{off}");
    assert!(off.contains("PYN_UPDATE_CHECK"), "{off}");
    let doc: serde_json::Value =
        serde_json::from_str(&env.ok(&home, "root", &["version", "--check", "--json"])).unwrap();
    assert_eq!(doc["update"]["enabled"], false);

    let login = env.run_stdin(
        &home,
        "-",
        &["login", "alice", "--password-stdin"],
        &format!("{PASSWORD}\n"),
    );
    assert!(login.status.success());
    let denied = env.ok(&home, "-", &["version", "--check"]);
    assert!(denied.contains("administrator"), "{denied}");
}

#[test]
fn plain_version_prints_the_client_version_and_json_is_refused_without_check() {
    let (env, home) = world(None);
    let out = env.ok(&home, "root", &["version"]);
    assert_eq!(out.trim(), format!("pyn {}", env!("CARGO_PKG_VERSION")));
    assert!(
        !env.run(&home, "root", &["version", "--json"])
            .status
            .success()
    );
}
