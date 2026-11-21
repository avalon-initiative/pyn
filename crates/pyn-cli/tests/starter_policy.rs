//! The policy a new repository starts with, through the real binaries (decision #1165).

mod common;

use common::real::Server;

const REPO: [&str; 2] = ["--repo", "alice/plain"];

fn lock(server: &Server, path: &str) -> Result<(), String> {
    let cwd = server.dir("work");
    let args = [&["lock", path][..], &REPO].concat();
    let out = server.run_with(&cwd, "alice", &args, None);
    match out.status.success() {
        true => Ok(()),
        false => Err(String::from_utf8_lossy(&out.stderr).into_owned()),
    }
}

#[test]
fn a_repository_created_without_a_policy_carries_the_starter_file() {
    let server = Server::with_game_env(&[]);
    let cwd = server.dir("work");
    let out = server.ok(&cwd, "alice", &["repo", "create", "plain"]);
    assert!(out.contains("added .pyn/pyn.toml"), "{out}");
    let shown = server.ok(
        &cwd,
        "alice",
        &[&["get", ".pyn/pyn.toml"][..], &REPO].concat(),
    );
    assert_eq!(shown, pyn_core::rules::DEFAULT_INITIAL_POLICY);
    let files = server.ok(&cwd, "alice", &[&["files"][..], &REPO].concat());
    let row = files.lines().find(|l| l.contains(".pyn/pyn.toml")).unwrap();
    assert!(row.contains("exclusive"), "{files}");
    for kind in ["x.png", "art/b.psd", "m.uasset"] {
        lock(&server, kind).unwrap_or_else(|e| panic!("{kind} should need a lock: {e}"));
    }
    for kind in ["notes.txt", "src/a.rs", "a/b.toml"] {
        let err = lock(&server, kind).expect_err(kind);
        assert!(err.contains("not_exclusive"), "{kind}: {err}");
    }
}

#[test]
fn no_policy_leaves_the_operators_configured_policy_in_force() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("pyn.toml");
    std::fs::write(
        &config,
        "[meta]\ndefault = \"shared\"\n[exclusive]\npaths = [\"notes.txt\"]\n",
    )
    .unwrap();
    let server = Server::with_game_env(&[("PYN_CONFIG", config.to_str().unwrap())]);
    let cwd = server.dir("work");
    let out = server.ok(&cwd, "alice", &["repo", "create", "plain", "--no-policy"]);
    assert!(!out.contains("added"), "{out}");
    lock(&server, "notes.txt").expect("the configured list locks it");
    let err = lock(&server, "x.png").expect_err("neither the starter nor a client default applies");
    assert!(err.contains("not_exclusive"), "{err}");
}

fn created_with(extra: &[&str], input: Option<&str>) -> (Server, std::process::Output) {
    let server = Server::with_game_env(&[]);
    let cwd = server.dir("work");
    let args = [&["repo", "create", "plain"][..], extra].concat();
    let out = server.run_with(&cwd, "alice", &args, input);
    (server, out)
}

fn default_of(server: &Server) -> String {
    let cwd = server.dir("work");
    server.ok(
        &cwd,
        "alice",
        &[&["get", ".pyn/pyn.toml"][..], &REPO].concat(),
    )
}

#[test]
fn the_default_flag_sets_the_starter_default_and_the_lock_behaviour() {
    let (server, out) = created_with(&["--default", "exclusive"], None);
    assert!(out.status.success());
    assert!(default_of(&server).contains("default = \"exclusive\""));
    lock(&server, "notes.txt").expect("an exclusive default needs a lock");
    lock(&server, "x.png").expect("the listed kinds stay exclusive");

    let (server, out) = created_with(&["--default", "shared"], None);
    assert!(out.status.success());
    assert!(default_of(&server).contains("default = \"shared\""));
    let err = lock(&server, "notes.txt").expect_err("shared needs no lock");
    assert!(err.contains("not_exclusive"), "{err}");
}

#[test]
fn without_a_terminal_nothing_is_asked_and_shared_applies() {
    let (server, out) = created_with(&[], Some("exclusive\n"));
    assert!(out.status.success());
    assert!(!String::from_utf8_lossy(&out.stderr).contains("shared (edits"));
    assert_eq!(default_of(&server), pyn_core::rules::DEFAULT_INITIAL_POLICY);
}

#[test]
fn the_default_flag_conflicts_with_policy_options_and_rejects_bad_values() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("p.toml");
    std::fs::write(&file, "[meta]\ndefault = \"shared\"\n").unwrap();
    let file = file.to_str().unwrap();
    for extra in [
        vec!["--default", "shared", "--policy", file],
        vec!["--default", "shared", "--no-policy"],
        vec!["--default", "sometimes"],
    ] {
        let (server, out) = created_with(&extra, None);
        assert_eq!(out.status.code(), Some(129), "{extra:?}");
        let cwd = server.dir("work");
        let list = server.ok(&cwd, "alice", &["repo", "list"]);
        assert!(!list.contains("plain"), "nothing was created: {list}");
    }
}
