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
