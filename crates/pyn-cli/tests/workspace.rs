//! Runs the real `pyn` binary against a real server started in this process.

mod common;

use std::path::Path;

use common::{REPO, start};

fn read_only(path: &Path) -> bool {
    std::fs::metadata(path).unwrap().permissions().readonly()
}

#[test]
fn a_workspace_tracks_what_you_cloned_and_handles_locks_and_checkins() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "int a;", None);
    env.seed("alice", "Content/m.umap", "map one", None);

    let ws = env.dir("ws");
    let out = env.ok(
        &env.dir("home"),
        "alice",
        &["clone", &env.source(REPO), ws.to_str().unwrap()],
    );
    assert!(out.contains("cloned 2 files"), "{out}");
    let ws = ws.canonicalize().unwrap();
    assert_eq!(
        std::fs::read_to_string(ws.join("Source/a.cpp")).unwrap(),
        "int a;"
    );
    assert!(
        !read_only(&ws.join("Source/a.cpp")),
        "shared files are writable"
    );
    assert!(
        read_only(&ws.join("Content/m.umap")),
        "exclusive files are read-only until locked"
    );
    let config = std::fs::read_to_string(ws.join(".pyn/local_only/config.toml")).unwrap();
    assert!(config.contains(&env.url), "{config}");
    assert!(ws.join(".pyn/local_only/state/files.toml").is_file());

    assert!(
        env.ok(&ws, "alice", &["status"])
            .contains("everything is up to date")
    );

    std::fs::write(ws.join("Source/a.cpp"), "int a = 1;").unwrap();
    let status = env.ok(&ws.join("Source"), "alice", &["status"]);
    assert!(
        status.contains("Source/a.cpp") && status.contains("modified"),
        "{status}"
    );

    let content = ws.join("Content");
    env.ok(&content, "alice", &["checkout", "m.umap"]);
    assert!(
        !read_only(&ws.join("Content/m.umap")),
        "checkout makes it writable"
    );
    assert!(
        env.ok(&ws, "alice", &["status"])
            .contains("1 locked by you")
    );
    let blocked = env.fails(&ws, "bob", &["checkout", "Content/m.umap"]);
    assert!(
        blocked.contains("locked by alice") && blocked.contains("Ask alice"),
        "{blocked}"
    );

    std::fs::write(ws.join("Content/m.umap"), "map two").unwrap();
    let out = env.ok(&content, "alice", &["checkin", "m.umap", "-m", "second"]);
    assert!(out.contains("revision 2"), "{out}");
    assert!(
        read_only(&ws.join("Content/m.umap")),
        "checkin locks it down again"
    );

    let out = env.ok(&ws, "alice", &["checkin", "Source/a.cpp", "-m", "edit"]);
    assert!(
        out.contains("revision 2"),
        "the base revision came from the workspace: {out}"
    );
    assert!(
        env.ok(&ws, "alice", &["status"])
            .contains("everything is up to date")
    );
    let history = env.ok(&ws, "alice", &["history", "Content/m.umap"]);
    assert_eq!(history.lines().count(), 2, "{history}");

    env.ok(&content, "alice", &["checkout", "m.umap"]);
    env.ok(&content, "alice", &["release", "m.umap"]);
    assert!(
        read_only(&ws.join("Content/m.umap")),
        "release locks it down too"
    );
}

#[test]
fn update_brings_in_newer_files_but_never_overwrites_local_edits() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "a one", None);
    env.seed("alice", "Source/b.cpp", "b one", None);
    env.seed("alice", "Content/m.umap", "map one", None);

    let ws = env.dir("ws");
    env.ok(
        &env.dir("home"),
        "alice",
        &["clone", &env.source(REPO), ws.to_str().unwrap()],
    );
    let ws = ws.canonicalize().unwrap();

    env.seed("bob", "Source/a.cpp", "a two", Some(1));
    env.seed("bob", "Source/b.cpp", "b two", Some(1));
    env.seed("bob", "Source/new.cpp", "brand new", None);
    env.seed("bob", "Content/m.umap", "map two", Some(1));
    std::fs::write(ws.join("Source/b.cpp"), "b local edit").unwrap();

    let status = env.ok(&ws, "alice", &["status"]);
    assert!(
        status.contains("Source/a.cpp") && status.contains("behind (head r2)"),
        "{status}"
    );
    assert!(
        status.contains("Source/new.cpp") && status.contains("new"),
        "{status}"
    );
    assert!(
        status.contains("Source/b.cpp") && status.contains("modified") && status.contains("behind"),
        "{status}"
    );

    let stale = env.fails(&ws, "alice", &["checkout", "Content/m.umap"]);
    assert!(stale.contains("pyn update"), "{stale}");

    let out = env.ok(&ws, "alice", &["update"]);
    assert!(out.contains("updated 3 file(s)"), "{out}");
    assert!(
        out.contains("skipped Source/b.cpp") && out.contains("local changes"),
        "{out}"
    );
    assert_eq!(
        std::fs::read_to_string(ws.join("Source/a.cpp")).unwrap(),
        "a two"
    );
    assert_eq!(
        std::fs::read_to_string(ws.join("Source/new.cpp")).unwrap(),
        "brand new"
    );
    assert_eq!(
        std::fs::read_to_string(ws.join("Source/b.cpp")).unwrap(),
        "b local edit",
        "local edits survive"
    );
    assert_eq!(
        std::fs::read_to_string(ws.join("Content/m.umap")).unwrap(),
        "map two"
    );
    assert!(read_only(&ws.join("Content/m.umap")));

    env.ok(&ws, "alice", &["checkout", "Content/m.umap"]);
}

#[test]
fn ignore_rules_and_settings_work_per_workspace_and_per_user() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "a one", None);
    let ws = env.dir("ws");
    env.ok(
        &env.dir("home"),
        "alice",
        &["clone", &env.source(REPO), ws.to_str().unwrap()],
    );
    let ws = ws.canonicalize().unwrap();

    std::fs::write(ws.join(".pyn/ignore"), "# build output\n*.log\nbuild/\n").unwrap();
    std::fs::create_dir_all(ws.join("build")).unwrap();
    std::fs::write(ws.join("build/out.o"), "x").unwrap();
    std::fs::write(ws.join("debug.log"), "x").unwrap();
    std::fs::write(ws.join("notes.txt"), "x").unwrap();
    let status = env.ok(&ws, "alice", &["status"]);
    assert!(
        status.contains("notes.txt") && status.contains("untracked"),
        "{status}"
    );
    assert!(
        !status.contains("debug.log") && !status.contains("out.o"),
        "{status}"
    );

    assert_eq!(
        env.ok(&ws, "alice", &["config", "get", "server"]).trim(),
        env.url
    );
    env.ok(&ws, "alice", &["config", "set", "user", "alice"]);
    assert_eq!(
        env.ok(&ws, "alice", &["config", "get", "user", "--local"])
            .trim(),
        "alice"
    );
    let apart = env.dir("apart");
    let err = env.fails(&apart, "alice", &["config", "set", "user", "bob"]);
    assert!(err.contains("not in a workspace"), "{err}");
    env.ok(
        &apart,
        "alice",
        &["config", "set", "user", "bob", "--global"],
    );
    assert_eq!(
        env.ok(&apart, "alice", &["config", "get", "user", "--global"])
            .trim(),
        "bob"
    );
    assert!(
        env.fails(&ws, "alice", &["config", "set", "colour", "red"])
            .contains("unknown setting")
    );

    let outside = env.fails(&ws, "alice", &["checkout", "../elsewhere.txt"]);
    assert!(outside.contains("outside the workspace"), "{outside}");
}
