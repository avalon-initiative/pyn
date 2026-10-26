//! End to end, second part: branches and merge, `pyn verify`, the staging flow, policy preview and the `local_store`
//! commit flow, against the real `pyn` and `pyn-server` binaries.

use std::path::{Path, PathBuf};

mod common;

use common::real::{POLICY, Server};

fn clone_ws(server: &Server, name: &str, extra: &[&str]) -> PathBuf {
    let ws = server.dir(name).join("game");
    let source = server.source();
    let mut args = vec!["clone"];
    args.extend(extra);
    args.extend([source.as_str(), ws.to_str().unwrap()]);
    server.ok(&server.dir("home"), "alice", &args);
    ws
}

fn read(ws: &Path, path: &str) -> String {
    std::fs::read_to_string(ws.join(path)).unwrap()
}

fn write(ws: &Path, path: &str, text: &str) {
    let target = ws.join(path);
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    if target.exists() {
        let mut perms = std::fs::metadata(&target).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        std::fs::set_permissions(&target, perms).unwrap();
    }
    std::fs::write(target, text).unwrap();
}

#[test]
fn branch_commit_and_merge_back_shows_in_the_log() {
    let server = Server::with_game();
    server.seed("Source/a.cpp", "a\n");
    server.seed("Source/b.cpp", "b\n");
    let ws = clone_ws(&server, "ws", &["--no-local-store"]);

    server.ok(&ws, "alice", &["branch", "feature"]);
    assert_eq!(server.ok(&ws, "alice", &["branch"]), "  feature\n* main\n");
    server.ok(&ws, "alice", &["switch", "feature"]);
    assert_eq!(
        server.ok(&ws, "alice", &["branch", "--show-current"]),
        "feature\n"
    );
    write(&ws, "Source/a.cpp", "a from feature\n");
    server.ok(
        &ws,
        "alice",
        &["commit", "Source/a.cpp", "-m", "feature work"],
    );

    server.ok(&ws, "alice", &["switch", "main"]);
    assert_eq!(read(&ws, "Source/a.cpp"), "a\n");
    write(&ws, "Source/b.cpp", "b from main\n");
    server.ok(&ws, "alice", &["commit", "Source/b.cpp", "-m", "main work"]);
    let log = server.ok(&ws, "alice", &["log", "--commits"]);
    assert!(!log.contains("feature work"), "{log}");

    let preview = server.ok(&ws, "alice", &["merge", "--preview", "feature"]);
    assert!(preview.contains("Source/a.cpp"), "{preview}");
    assert_eq!(
        read(&ws, "Source/a.cpp"),
        "a\n",
        "a preview changes nothing"
    );

    let out = server.ok(
        &ws,
        "alice",
        &["merge", "feature", "-m", "bring in feature"],
    );
    assert!(out.starts_with("Merge made;"), "{out}");
    assert_eq!(read(&ws, "Source/a.cpp"), "a from feature\n");
    assert_eq!(read(&ws, "Source/b.cpp"), "b from main\n");
    let log = server.ok(&ws, "alice", &["log", "--commits"]);
    for message in ["bring in feature", "feature work", "main work"] {
        assert!(log.contains(message), "{message} missing: {log}");
    }

    let other = clone_ws(&server, "other", &[]);
    assert_eq!(read(&other, "Source/a.cpp"), "a from feature\n");
    assert_eq!(server.ok(&other, "alice", &["verify"]), "");
    assert!(
        server
            .ok(&ws, "alice", &["status"])
            .contains("everything is up to date")
    );
}

#[test]
fn verify_reports_tampered_history_and_audit_log() {
    // The spawned server keeps its stores in its own memory with no outside seam, so tampering uses the in-process
    // harness (real `pyn` binary, store handles exposed) as verify.rs does.
    use pyn_core::{RepoId, RepoPath, RevisionId};
    let env = common::start();
    let home = env.dir("home");
    for (i, text) in ["one", "two", "three"].into_iter().enumerate() {
        env.seed("alice", "Source/a.ts", text, (i > 0).then_some(i as u64));
    }
    assert_eq!(env.ok(&home, "alice", &["verify"]), "");

    env.meta.tamper(
        &RepoId::new("t"),
        &RepoPath::new("Source/a.ts").unwrap(),
        RevisionId(2),
        |r| r.message = "rewritten".into(),
    );
    let out = env.run(&home, "alice", &["verify"]);
    assert_eq!(out.status.code(), Some(1));
    let rows = common::cell_rows(&String::from_utf8_lossy(&out.stdout));
    assert_eq!(rows[0], ["PROBLEM", "WHAT", "PATH"]);
    assert_eq!(rows[1], ["hash_mismatch", "r2", "Source/a.ts"]);

    env.audit
        .tamper(1, |e| e.actor = pyn_core::UserId::new("mallory"));
    let out = env.run(&home, "alice", &["verify"]);
    assert_eq!(out.status.code(), Some(1));
    let rows = common::cell_rows(&String::from_utf8_lossy(&out.stdout));
    assert!(
        rows.iter()
            .any(|r| r == &["hash_mismatch", "event 1", "(audit log)"]),
        "{rows:?}"
    );
}

#[test]
fn staging_flow_with_an_exclusive_path_refused_then_accepted_with_a_lock() {
    let server = Server::with_game();
    server.seed("Content/m.umap", "map one");
    server.seed("Source/a.cpp", "one");
    let ws = clone_ws(&server, "ws", &["--no-local-store"]);

    write(&ws, "Content/m.umap", "map two");
    write(&ws, "Source/a.cpp", "two");
    write(&ws, "Source/new.cpp", "new");
    let status = server.ok(&ws, "alice", &["status"]);
    assert!(status.contains("1 untracked"), "{status}");
    assert!(status.contains("0 staged"), "{status}");

    let (_, err) = server.fails(&ws, "alice", &["add", "."]);
    assert!(
        err.contains("Content/m.umap is exclusive and you do not hold its lock")
            && err.contains("pyn lock Content/m.umap"),
        "{err}"
    );
    let (_, err) = server.fails(&ws, "alice", &["commit", "-a", "-m", "x"]);
    assert!(err.contains("pyn lock Content/m.umap"), "{err}");
    let status = server.ok(&ws, "alice", &["status"]);
    assert!(
        status.contains("0 staged"),
        "a refusal stages nothing: {status}"
    );

    let (_, err) = {
        server.ok(&ws, "bob", &["lock", "Content/m.umap"]);
        server.fails(&ws, "alice", &["add", "Content/m.umap"])
    };
    assert!(err.contains("locked by bob"), "{err}");
    server.ok(&ws, "bob", &["unlock", "Content/m.umap"]);

    server.ok(&ws, "alice", &["lock", "Content/m.umap"]);
    write(&ws, "Content/m.umap", "map two");
    server.ok(&ws, "alice", &["add", "Source/new.cpp"]);
    let status = server.ok(&ws, "alice", &["status"]);
    assert!(status.contains("1 staged"), "{status}");

    server.ok(&ws, "alice", &["commit", "-a", "-m", "map and code"]);
    assert!(
        server
            .ok(&ws, "alice", &["status"])
            .contains("everything is up to date")
    );
    assert!(
        !server
            .ok(&ws, "alice", &["locks", "--mine"])
            .contains("Content/m.umap"),
        "commit released the lock"
    );
    let other = clone_ws(&server, "other", &["--no-local-store"]);
    assert_eq!(read(&other, "Content/m.umap"), "map two");
    assert_eq!(read(&other, "Source/a.cpp"), "two");
    assert_eq!(read(&other, "Source/new.cpp"), "new");
}

#[test]
fn policy_preview_lists_changes_without_applying_them() {
    let server = Server::with_game();
    server.seed("Source/a.cpp", "code");
    server.seed("Content/m.umap", "map");
    let ws = clone_ws(&server, "ws", &[]);

    let unchanged = ws.join("same.toml");
    std::fs::write(&unchanged, POLICY).unwrap();
    let out = server.ok(&ws, "alice", &["policy", "preview", "same.toml"]);
    assert!(out.contains("no path changes mode"), "{out}");

    let stricter = POLICY.replace(
        "paths = [\"Content/\"]",
        "paths = [\"Content/\", \"Source/\"]",
    );
    std::fs::write(ws.join("stricter.toml"), &stricter).unwrap();
    let out = server.ok(&ws, "alice", &["policy", "preview", "stricter.toml"]);
    let rows = common::cell_rows(&out);
    assert_eq!(rows[0], ["CHANGE", "PATH"]);
    assert_eq!(rows[1], ["shared -> exclusive", "Source/a.cpp"], "{out}");
    assert_eq!(rows.len(), 2, "{out}");

    let (_, err) = server.fails(&ws, "alice", &["policy", "preview", "missing.toml"]);
    assert!(err.contains("missing.toml"), "{err}");

    let fresh = clone_ws(&server, "fresh", &[]);
    assert_eq!(
        read(&fresh, ".pyn/pyn.toml"),
        POLICY,
        "preview applied nothing"
    );
}

#[test]
fn local_commits_are_pushed_fetched_and_pulled_between_clones() {
    let server = Server::with_game();
    server.seed("Source/a.cpp", "one\n");
    server.seed("Content/m.umap", "map");
    let ws = clone_ws(&server, "ws", &["--local-store"]);
    let other = clone_ws(&server, "other", &["--local-store"]);

    write(&ws, "Source/a.cpp", "one\ntwo\n");
    server.ok(&ws, "alice", &["add", "Source/a.cpp"]);
    server.ok(&ws, "alice", &["commit", "-m", "local edit"]);
    assert!(
        server
            .ok(&ws, "alice", &["status"])
            .contains("local commit"),
        "the commit is local until pushed"
    );
    let log = server.ok(&other, "alice", &["log", "--commits"]);
    assert!(!log.contains("local edit"), "{log}");

    let out = server.ok(&ws, "alice", &["push"]);
    assert!(out.contains("main -> main"), "{out}");
    assert_eq!(
        server.ok(&ws, "alice", &["push"]),
        "Everything up-to-date\n"
    );

    server.ok(&other, "alice", &["fetch"]);
    assert_eq!(
        read(&other, "Source/a.cpp"),
        "one\n",
        "fetch leaves files alone"
    );
    server.ok(&other, "alice", &["pull"]);
    assert_eq!(read(&other, "Source/a.cpp"), "one\ntwo\n");
    let log = server.ok(&other, "alice", &["log", "--commits"]);
    assert!(log.contains("local edit"), "{log}");
    assert!(
        server
            .ok(&other, "alice", &["status"])
            .contains("everything is up to date")
    );
}

#[test]
fn a_push_of_an_exclusive_path_needs_a_lock() {
    let server = Server::with_game();
    server.seed("Content/m.umap", "map");
    let ws = clone_ws(&server, "ws", &["--local-store"]);

    server.ok(&ws, "alice", &["lock", "Content/m.umap"]);
    write(&ws, "Content/m.umap", "map2");
    server.ok(&ws, "alice", &["add", "Content/m.umap"]);
    server.ok(&ws, "alice", &["commit", "-m", "map"]);
    server.ok(&ws, "alice", &["unlock", "Content/m.umap"]);

    let (_, err) = server.fails(&ws, "alice", &["push"]);
    assert!(
        err.contains("lock required") && err.contains("pyn lock"),
        "{err}"
    );
    assert!(err.contains("nothing was pushed"), "{err}");
    let other = clone_ws(&server, "other", &[]);
    assert_eq!(
        read(&other, "Content/m.umap"),
        "map",
        "nothing reached the server"
    );

    server.ok(&ws, "alice", &["lock", "Content/m.umap"]);
    server.ok(&ws, "alice", &["push"]);
    let third = clone_ws(&server, "third", &[]);
    assert_eq!(read(&third, "Content/m.umap"), "map2");
    assert!(
        !server
            .ok(&ws, "alice", &["status"])
            .contains("local commit")
    );
}
