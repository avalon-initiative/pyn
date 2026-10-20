//! `rm` and `mv` in a workspace of a pyn server: they stage a deletion, or a deletion and an addition, and an
//! exclusive path is refused without its lock exactly as `add` refuses it.

#![cfg(unix)]

mod common;

use std::path::PathBuf;

use common::{REPO, cell_rows, start};

fn clone_as(env: &common::Env, name: &str, extra: &[&str]) -> PathBuf {
    let ws = env.dir(name);
    let source = env.source(REPO);
    let mut args = vec!["clone"];
    args.extend(extra);
    args.extend([source.as_str(), ws.to_str().unwrap()]);
    env.ok(&env.dir("home"), "alice", &args);
    ws.canonicalize().unwrap()
}

fn clone(env: &common::Env) -> PathBuf {
    clone_as(env, "ws", &["--no-local-store"])
}

fn write(ws: &std::path::Path, path: &str, text: &str) {
    let file = ws.join(path);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    if file.exists() {
        std::fs::set_permissions(&file, std::os::unix::fs::PermissionsExt::from_mode(0o644))
            .unwrap();
    }
    std::fs::write(file, text).unwrap();
}

/// The `STAGED` cell of `path`'s row in `pyn status`, or `-` when the path has no row.
fn staged_cell(env: &common::Env, ws: &std::path::Path, path: &str) -> String {
    let out = env.ok(ws, "alice", &["status"]);
    let rows = cell_rows(&out);
    let Some(header) = rows.iter().find(|r| r.iter().any(|c| c == "STAGED")) else {
        return "-".into();
    };
    let col = header.iter().position(|c| c == "STAGED").unwrap();
    rows.iter()
        .find(|r| r.last().is_some_and(|c| c == path))
        .map_or("-".to_string(), |r| r[col].clone())
}

fn refused(env: &common::Env, ws: &std::path::Path, args: &[&str]) -> (i32, String) {
    let out = env.run(ws, "alice", args);
    assert!(!out.status.success(), "pyn {args:?} should have failed");
    (
        out.status.code().unwrap(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn seeded() -> (common::Env, PathBuf) {
    let env = start();
    env.seed("alice", "Source/a.cpp", "a one", None);
    env.seed("alice", "Source/b.cpp", "b one", None);
    env.seed("alice", "Source/deep/c.cpp", "c one", None);
    env.seed("alice", "Other/d.txt", "d one", None);
    let ws = clone(&env);
    (env, ws)
}

#[test]
fn rm_stages_a_deletion_removes_the_file_and_commit_sends_it() {
    let (env, ws) = seeded();
    let out = env.ok(&ws, "alice", &["rm", "Source/a.cpp"]);
    assert!(out.contains("rm 'Source/a.cpp'"), "{out}");
    assert!(!ws.join("Source/a.cpp").exists());
    assert_eq!(staged_cell(&env, &ws, "Source/a.cpp"), "deleted");
    let out = env.ok(&ws, "alice", &["commit", "-m", "drop a"]);
    assert!(
        out.contains("Source/a.cpp was deleted at revision"),
        "{out}"
    );
    let files = env.ok(&ws, "alice", &["files"]);
    assert!(
        !files.contains("Source/a.cpp") && files.contains("Source/b.cpp"),
        "{files}"
    );
}

#[test]
fn rm_cached_keeps_the_file_and_commit_dash_a_keeps_the_deletion() {
    let (env, ws) = seeded();
    env.ok(&ws, "alice", &["rm", "--cached", "Source/a.cpp"]);
    assert!(ws.join("Source/a.cpp").exists());
    assert_eq!(staged_cell(&env, &ws, "Source/a.cpp"), "deleted");
    write(&ws, "Source/b.cpp", "b two");
    let out = env.ok(&ws, "alice", &["commit", "-am", "untrack a"]);
    assert!(out.contains("Source/a.cpp was deleted"), "{out}");
    assert!(ws.join("Source/a.cpp").exists());
    assert!(!env.ok(&ws, "alice", &["files"]).contains("Source/a.cpp"));
}

#[test]
fn rm_refuses_changes_unless_forced_and_stages_nothing() {
    let (env, ws) = seeded();
    write(&ws, "Source/a.cpp", "a edited");
    let (code, err) = refused(&env, &ws, &["rm", "Source/a.cpp", "Source/b.cpp"]);
    assert_eq!(code, 1);
    assert!(
        err.contains("local modifications") && err.contains("Source/a.cpp"),
        "{err}"
    );
    assert!(ws.join("Source/b.cpp").exists());
    assert_eq!(staged_cell(&env, &ws, "Source/b.cpp"), "-");

    env.ok(&ws, "alice", &["rm", "--cached", "Source/a.cpp"]);
    assert_eq!(staged_cell(&env, &ws, "Source/a.cpp"), "deleted");
    env.ok(&ws, "alice", &["restore", "--staged", "Source/a.cpp"]);

    env.ok(&ws, "alice", &["add", "Source/a.cpp"]);
    let (code, err) = refused(&env, &ws, &["rm", "Source/a.cpp"]);
    assert_eq!(code, 1);
    assert!(err.contains("changes staged"), "{err}");
    write(&ws, "Source/a.cpp", "a edited again");
    let (code, err) = refused(&env, &ws, &["rm", "--cached", "Source/a.cpp"]);
    assert_eq!(code, 1);
    assert!(err.contains("different from both"), "{err}");

    env.ok(&ws, "alice", &["rm", "-f", "Source/a.cpp"]);
    assert!(!ws.join("Source/a.cpp").exists());
    assert_eq!(staged_cell(&env, &ws, "Source/a.cpp"), "deleted");
}

#[test]
fn rm_of_a_file_that_was_only_staged_forgets_it() {
    let (env, ws) = seeded();
    write(&ws, "Source/new.cpp", "new");
    env.ok(&ws, "alice", &["add", "Source/new.cpp"]);
    assert_eq!(staged_cell(&env, &ws, "Source/new.cpp"), "added");
    let (code, _) = refused(&env, &ws, &["rm", "Source/new.cpp"]);
    assert_eq!(code, 1);
    env.ok(&ws, "alice", &["rm", "--cached", "Source/new.cpp"]);
    assert!(ws.join("Source/new.cpp").exists());
    assert_eq!(staged_cell(&env, &ws, "Source/new.cpp"), "-");
    env.ok(&ws, "alice", &["add", "Source/new.cpp"]);
    env.ok(&ws, "alice", &["rm", "-f", "Source/new.cpp"]);
    assert!(!ws.join("Source/new.cpp").exists());
    assert_eq!(staged_cell(&env, &ws, "Source/new.cpp"), "-");
}

#[test]
fn rm_of_a_folder_needs_dash_r_and_unmatched_paths_are_errors() {
    let (env, ws) = seeded();
    let (code, err) = refused(&env, &ws, &["rm", "Source"]);
    assert_eq!(code, 128);
    assert!(
        err.contains("not removing 'Source' recursively without -r"),
        "{err}"
    );
    let (code, err) = refused(&env, &ws, &["rm", "nothing"]);
    assert_eq!(code, 128);
    assert!(err.contains("did not match any files"), "{err}");
    env.ok(&ws, "alice", &["rm", "--ignore-unmatch", "nothing"]);

    let out = env.ok(&ws, "alice", &["rm", "-r", "-n", "Source"]);
    assert!(out.contains("rm 'Source/deep/c.cpp'"), "{out}");
    assert!(ws.join("Source/a.cpp").exists());

    let out = env.ok(&ws, "alice", &["rm", "-r", "-q", "Source"]);
    assert!(out.is_empty(), "{out}");
    assert!(!ws.join("Source").exists());
    assert!(ws.join("Other/d.txt").exists());
    assert_eq!(staged_cell(&env, &ws, "Source/deep/c.cpp"), "deleted");
}

#[test]
fn rm_of_an_exclusive_path_needs_the_lock_like_add() {
    let env = start();
    env.seed("alice", "Content/m.umap", "map one", None);
    env.seed("alice", "Source/a.cpp", "one", None);
    let ws = clone(&env);
    let (code, err) = refused(&env, &ws, &["rm", "Content/m.umap", "Source/a.cpp"]);
    let (add_code, add_err) = {
        write(&ws, "Content/m.umap", "map two");
        refused(&env, &ws, &["add", "Content/m.umap"])
    };
    assert_eq!(code, add_code);
    assert_eq!(err, add_err);
    assert!(
        err.contains("Content/m.umap is exclusive and you do not hold its lock")
            && err.contains("pyn lock Content/m.umap"),
        "{err}"
    );
    assert!(ws.join("Source/a.cpp").exists(), "nothing is removed");
    assert_eq!(staged_cell(&env, &ws, "Source/a.cpp"), "-");

    env.ok(&env.dir("home"), "bob", &["lock", "Content/other.umap"]);
    env.ok(&ws, "alice", &["lock", "Content/m.umap"]);
    env.ok(&ws, "alice", &["rm", "-f", "Content/m.umap"]);
    assert_eq!(staged_cell(&env, &ws, "Content/m.umap"), "deleted");
    env.ok(&ws, "alice", &["commit", "-m", "remove the map"]);
    assert!(
        env.ok(&ws, "alice", &["locks", "--mine"])
            .contains("no locks")
    );
}

#[test]
fn rm_of_a_path_locked_by_someone_else_names_the_holder() {
    let env = start();
    env.seed("alice", "Content/m.umap", "map one", None);
    let ws = clone(&env);
    env.ok(&ws, "bob", &["lock", "Content/m.umap"]);
    let (code, err) = refused(&env, &ws, &["rm", "--cached", "Content/m.umap"]);
    assert_eq!(code, 128);
    assert!(err.contains("locked by bob"), "{err}");
}

#[test]
fn mv_stages_a_deletion_and_an_addition_and_commit_sends_both() {
    let (env, ws) = seeded();
    env.ok(&ws, "alice", &["mv", "Source/a.cpp", "Source/renamed.cpp"]);
    assert!(!ws.join("Source/a.cpp").exists());
    assert_eq!(
        std::fs::read_to_string(ws.join("Source/renamed.cpp")).unwrap(),
        "a one"
    );
    assert_eq!(staged_cell(&env, &ws, "Source/a.cpp"), "deleted");
    assert_eq!(staged_cell(&env, &ws, "Source/renamed.cpp"), "added");
    let out = env.ok(&ws, "alice", &["commit", "-m", "rename"]);
    assert!(
        out.contains("Source/renamed.cpp is now at revision")
            && out.contains("Source/a.cpp was deleted at revision"),
        "{out}"
    );
    assert_eq!(
        env.ok(&ws, "alice", &["get", "Source/renamed.cpp"]),
        "a one"
    );
}

#[test]
fn mv_into_a_folder_of_a_folder_and_with_the_flags() {
    let (env, ws) = seeded();
    env.ok(
        &ws,
        "alice",
        &["mv", "Source/a.cpp", "Source/b.cpp", "Other"],
    );
    assert!(ws.join("Other/a.cpp").exists() && ws.join("Other/b.cpp").exists());
    env.ok(&ws, "alice", &["mv", "Source", "Moved"]);
    assert_eq!(
        std::fs::read_to_string(ws.join("Moved/deep/c.cpp")).unwrap(),
        "c one"
    );
    assert_eq!(staged_cell(&env, &ws, "Source/deep/c.cpp"), "deleted");
    assert_eq!(staged_cell(&env, &ws, "Moved/deep/c.cpp"), "added");

    let out = env.ok(&ws, "alice", &["mv", "-n", "Other/a.cpp", "Other/z.cpp"]);
    assert!(out.contains("Renaming"), "{out}");
    assert!(ws.join("Other/a.cpp").exists());

    let (code, err) = refused(&env, &ws, &["mv", "Other/a.cpp", "Other/b.cpp"]);
    assert_eq!(code, 128);
    assert!(err.contains("destination exists"), "{err}");
    env.ok(&ws, "alice", &["mv", "-f", "Other/a.cpp", "Other/b.cpp"]);
    assert_eq!(
        std::fs::read_to_string(ws.join("Other/b.cpp")).unwrap(),
        "a one"
    );

    let (code, err) = refused(&env, &ws, &["mv", "nothing", "Other"]);
    assert_eq!(code, 128);
    assert!(err.contains("bad source"), "{err}");
    write(&ws, "loose.txt", "x");
    let (code, err) = refused(&env, &ws, &["mv", "loose.txt", "Other"]);
    assert_eq!(code, 128);
    assert!(err.contains("not under version control"), "{err}");
    env.ok(
        &ws,
        "alice",
        &["mv", "-k", "loose.txt", "Other/d.txt", "Moved"],
    );
    assert!(ws.join("Moved/d.txt").exists() && ws.join("loose.txt").exists());

    env.ok(&ws, "alice", &["commit", "-m", "all of it"]);
    let files = env.ok(&ws, "alice", &["files"]);
    assert!(
        files.contains("Moved/deep/c.cpp") && !files.contains("Source/"),
        "{files}"
    );
}

#[test]
fn mv_back_to_the_old_name_leaves_nothing_staged() {
    let (env, ws) = seeded();
    env.ok(&ws, "alice", &["mv", "Source/a.cpp", "tmp.cpp"]);
    env.ok(&ws, "alice", &["mv", "tmp.cpp", "Source/a.cpp"]);
    for path in ["Source/a.cpp", "tmp.cpp"] {
        assert_eq!(staged_cell(&env, &ws, path), "-", "{path}");
    }
}

#[test]
fn mv_of_an_exclusive_path_needs_the_lock_like_add() {
    let env = start();
    env.seed("alice", "Content/m.umap", "map one", None);
    let ws = clone(&env);
    let (code, err) = refused(&env, &ws, &["mv", "Content/m.umap", "Content/n.umap"]);
    assert_eq!(code, 128);
    assert_eq!(
        err.trim(),
        "fatal: Content/m.umap is exclusive and you do not hold its lock; run `pyn lock Content/m.umap` first"
    );
    assert!(ws.join("Content/m.umap").exists() && !ws.join("Content/n.umap").exists());
    assert_eq!(staged_cell(&env, &ws, "Content/m.umap"), "-");

    env.ok(&ws, "alice", &["lock", "Content/m.umap"]);
    env.ok(&ws, "alice", &["mv", "Content/m.umap", "Content/n.umap"]);
    env.ok(&ws, "alice", &["lock", "Content/n.umap"]);
    env.ok(&ws, "alice", &["commit", "-m", "rename the map"]);
    assert_eq!(env.ok(&ws, "alice", &["get", "Content/n.umap"]), "map one");
}

#[test]
fn rm_and_mv_commit_into_the_local_store_too() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "a one", None);
    env.seed("alice", "Source/b.cpp", "b one", None);
    let ws = clone_as(&env, "local", &[]);
    env.ok(&ws, "alice", &["rm", "Source/a.cpp"]);
    env.ok(&ws, "alice", &["mv", "Source/b.cpp", "Source/c.cpp"]);
    let out = env.ok(&ws, "alice", &["commit", "-m", "both"]);
    assert!(
        out.contains("2 file(s) changed") || out.contains("3 file(s) changed"),
        "{out}"
    );
    env.ok(&ws, "alice", &["push"]);
    let files = env.ok(&ws, "alice", &["files"]);
    assert!(
        files.contains("Source/c.cpp")
            && !files.contains("Source/a.cpp")
            && !files.contains("Source/b.cpp"),
        "{files}"
    );
}
