//! Staging in a workspace of a pyn server: `add`, `commit` of the staged set as one commit, `restore --staged`,
//! `diff --cached`, the staged column of `status`, and the exclusive-lock check at `add`.

#![cfg(unix)]

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use common::{REPO, cell_rows, start};

fn clone(env: &common::Env) -> PathBuf {
    let ws = env.dir("ws");
    env.ok(
        &env.dir("home"),
        "alice",
        &[
            "clone",
            "--no-local-store",
            &env.source(REPO),
            ws.to_str().unwrap(),
        ],
    );
    ws.canonicalize().unwrap()
}

fn make_writable(file: &std::path::Path) {
    if std::fs::metadata(file).is_ok() {
        std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
}

fn write(ws: &std::path::Path, path: &str, text: &str) {
    let file = ws.join(path);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    make_writable(&file);
    std::fs::write(file, text).unwrap();
}

/// The `STAGED` cell of `path`'s row in `pyn status`.
fn staged_cell(env: &common::Env, ws: &std::path::Path, path: &str) -> String {
    let out = env.ok(ws, "alice", &["status"]);
    let rows = cell_rows(&out);
    let header = rows
        .iter()
        .find(|r| r.iter().any(|c| c == "STAGED"))
        .expect(&out);
    let col = header.iter().position(|c| c == "STAGED").unwrap();
    rows.iter()
        .find(|r| r.last().is_some_and(|c| c == path))
        .unwrap_or_else(|| panic!("no row for {path}: {out}"))[col]
        .clone()
}

#[test]
fn staged_files_are_committed_together_as_one_commit() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "a one", None);
    env.seed("alice", "Source/b.cpp", "b one", None);
    let ws = clone(&env);
    write(&ws, "Source/a.cpp", "a two");
    write(&ws, "Source/b.cpp", "b two");
    write(&ws, "Source/new.cpp", "new");
    write(&ws, "notes.txt", "not staged");

    env.ok(&ws, "alice", &["add", "Source"]);
    assert_eq!(staged_cell(&env, &ws, "Source/a.cpp"), "modified");
    assert_eq!(staged_cell(&env, &ws, "Source/new.cpp"), "added");
    assert_eq!(staged_cell(&env, &ws, "notes.txt"), "-");

    let before = env.ok(&ws, "alice", &["log", "--commits"]).lines().count();
    let out = env.ok(&ws, "alice", &["commit", "-m", "three files"]);
    for path in ["Source/a.cpp", "Source/b.cpp", "Source/new.cpp"] {
        assert!(out.contains(&format!("{path} is now at revision")), "{out}");
    }
    let commits = env.ok(&ws, "alice", &["log", "--commits"]);
    assert_eq!(
        commits.lines().count(),
        before + 1,
        "one new commit: {commits}"
    );
    let newest = cell_rows(&commits)[1].clone();
    assert_eq!(newest[3], "3", "{commits}");
    assert!(newest.contains(&"three files".to_string()), "{commits}");

    let status = env.ok(&ws, "alice", &["status"]);
    assert!(
        status.contains("notes.txt") && status.contains("1 untracked"),
        "{status}"
    );
    assert!(status.contains("0 staged"), "{status}");
    assert_eq!(env.ok(&ws, "alice", &["get", "Source/a.cpp"]), "a two");
}

#[test]
fn a_file_edited_after_it_was_staged_commits_as_staged() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "one", None);
    let ws = clone(&env);
    write(&ws, "Source/a.cpp", "two");
    env.ok(&ws, "alice", &["add", "Source/a.cpp"]);
    write(&ws, "Source/a.cpp", "three");

    let cached = env.ok(&ws, "alice", &["diff", "--cached"]);
    assert!(
        cached.contains("-one") && cached.contains("+two"),
        "{cached}"
    );
    let plain = env.ok(&ws, "alice", &["diff"]);
    assert!(
        plain.contains("-two") && plain.contains("+three"),
        "{plain}"
    );
    let status = env.ok(&ws, "alice", &["status"]);
    let row = cell_rows(&status)
        .into_iter()
        .find(|r| r.last().is_some_and(|c| c == "Source/a.cpp"))
        .unwrap();
    assert_eq!(
        &row[2..4],
        ["modified", "modified"],
        "staged and unstaged: {status}"
    );

    env.ok(&ws, "alice", &["commit", "-m", "the staged one"]);
    assert_eq!(env.ok(&ws, "alice", &["get", "Source/a.cpp"]), "two");
    let status = env.ok(&ws, "alice", &["status"]);
    assert!(
        status.contains("1 modified"),
        "the later edit is still unstaged: {status}"
    );
    assert_eq!(
        std::fs::read_to_string(ws.join("Source/a.cpp")).unwrap(),
        "three"
    );
}

#[test]
fn commit_dash_a_stages_tracked_changes_and_leaves_new_files_out() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "one", None);
    env.seed("alice", "Source/b.cpp", "one", None);
    let ws = clone(&env);
    write(&ws, "Source/a.cpp", "two");
    write(&ws, "Source/b.cpp", "two");
    write(&ws, "Source/new.cpp", "new");

    let out = env.ok(&ws, "alice", &["commit", "-am", "all tracked"]);
    assert!(
        out.contains("Source/a.cpp") && out.contains("Source/b.cpp"),
        "{out}"
    );
    assert!(!out.contains("new.cpp"), "{out}");
    let status = env.ok(&ws, "alice", &["status"]);
    assert!(
        status.contains("Source/new.cpp") && status.contains("1 untracked"),
        "{status}"
    );
}

#[test]
fn restore_staged_takes_a_path_back_out_and_leaves_the_file_alone() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "one", None);
    let ws = clone(&env);
    write(&ws, "Source/a.cpp", "two");
    env.ok(&ws, "alice", &["add", "."]);
    assert_eq!(staged_cell(&env, &ws, "Source/a.cpp"), "modified");
    env.ok(&ws, "alice", &["restore", "--staged", "Source/a.cpp"]);
    assert_eq!(staged_cell(&env, &ws, "Source/a.cpp"), "-");
    assert_eq!(
        std::fs::read_to_string(ws.join("Source/a.cpp")).unwrap(),
        "two"
    );
    let err = env.fails(&ws, "alice", &["restore", "--staged", "nothing"]);
    assert!(err.contains("did not match"), "{err}");
    let err = env.fails(&ws, "alice", &["commit", "-m", "x"]);
    assert_eq!(
        err.trim(),
        "nothing to commit; stage changes with `pyn add`, or use `pyn commit -a`"
    );
}

#[test]
fn an_exclusive_path_cannot_be_staged_without_its_lock() {
    let env = start();
    env.seed("alice", "Content/m.umap", "map one", None);
    env.seed("alice", "Source/a.cpp", "one", None);
    let ws = clone(&env);
    let map = ws.join("Content/m.umap");
    make_writable(&map);
    write(&ws, "Content/m.umap", "map two");
    write(&ws, "Source/a.cpp", "two");

    let err = env.fails(&ws, "alice", &["add", "."]);
    assert!(
        err.contains("Content/m.umap is exclusive and you do not hold its lock")
            && err.contains("pyn lock Content/m.umap"),
        "{err}"
    );
    assert_eq!(
        staged_cell(&env, &ws, "Source/a.cpp"),
        "-",
        "nothing is staged when one path is refused"
    );
    let err = env.fails(&ws, "alice", &["commit", "-am", "x"]);
    assert!(err.contains("pyn lock Content/m.umap"), "{err}");

    env.ok(&env.dir("home"), "bob", &["lock", "Content/other.umap"]);
    env.ok(&ws, "alice", &["lock", "Content/m.umap"]);
    write(&ws, "Content/m.umap", "map two");
    env.ok(&ws, "alice", &["add", "."]);
    assert_eq!(staged_cell(&env, &ws, "Content/m.umap"), "modified");
    assert_eq!(staged_cell(&env, &ws, "Source/a.cpp"), "modified");

    env.ok(&ws, "alice", &["commit", "-m", "map and code"]);
    let locks = env.ok(&ws, "alice", &["locks", "--mine"]);
    assert!(
        !locks.contains("Content/m.umap"),
        "commit released the lock: {locks}"
    );
    assert_eq!(env.ok(&ws, "alice", &["get", "Content/m.umap"]), "map two");
}

#[test]
fn another_users_lock_is_named_when_add_refuses() {
    let env = start();
    env.seed("alice", "Content/m.umap", "one", None);
    let ws = clone(&env);
    env.ok(
        &env.dir("home"),
        "bob",
        &["lock", "Content/m.umap", "--base", "1"],
    );
    write(&ws, "Content/m.umap", "two");
    let err = env.fails(&ws, "alice", &["add", "Content/m.umap"]);
    assert!(err.contains("locked by bob"), "{err}");
}

#[test]
fn globs_and_magic_pathspecs_stage_unstage_and_remove_files() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "a one", None);
    env.seed("alice", "Source/b.cpp", "b one", None);
    env.seed("alice", "Source/c.h", "c one", None);
    let ws = clone(&env);
    for p in ["Source/a.cpp", "Source/b.cpp", "Source/c.h"] {
        write(&ws, p, "two");
    }
    write(&ws, "Source/new.h", "new");
    write(&ws, "notes.txt", "x");

    env.ok(&ws, "alice", &["add", "Source/*.cpp"]);
    assert_eq!(staged_cell(&env, &ws, "Source/a.cpp"), "modified");
    assert_eq!(staged_cell(&env, &ws, "Source/b.cpp"), "modified");
    assert_eq!(staged_cell(&env, &ws, "Source/c.h"), "-");
    env.ok(
        &ws,
        "alice",
        &["restore", "--staged", ":(glob)Source/*.cpp"],
    );
    assert_eq!(staged_cell(&env, &ws, "Source/a.cpp"), "-");

    env.ok(&ws, "alice", &["add", ":(glob)**/*.h", ":!Source/c.h"]);
    assert_eq!(staged_cell(&env, &ws, "Source/new.h"), "added");
    assert_eq!(staged_cell(&env, &ws, "Source/c.h"), "-");
    env.ok(
        &ws,
        "alice",
        &["restore", "--staged", ":(icase)SOURCE/NEW.H"],
    );
    assert_eq!(staged_cell(&env, &ws, "Source/new.h"), "-");

    let dry = env.ok(&ws, "alice", &["add", "-n", ":(top)Source/?.cpp"]);
    assert!(
        dry.contains("add 'Source/a.cpp'") && dry.contains("add 'Source/b.cpp'"),
        "{dry}"
    );
    let err = env.fails(&ws, "alice", &["add", "nomatch*"]);
    assert!(
        err.contains("pathspec 'nomatch*' did not match any files"),
        "{err}"
    );
    let err = env.fails(&ws, "alice", &["add", ":(bogus)x"]);
    assert!(
        err.contains("Invalid pathspec magic 'bogus' in ':(bogus)x'"),
        "{err}"
    );

    let out = env.ok(&ws, "alice", &["rm", "-n", "-f", "Source/*.h"]);
    assert_eq!(out.trim(), "rm 'Source/c.h'");
    env.ok(&ws, "alice", &["rm", "-f", "Source/*.h"]);
    assert!(!ws.join("Source/c.h").exists() && ws.join("Source/new.h").exists());
    assert_eq!(staged_cell(&env, &ws, "Source/c.h"), "deleted");
}

#[test]
fn diff_takes_globs_on_a_server_workspace() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "one\n", None);
    env.seed("alice", "Source/b.txt", "one\n", None);
    let ws = clone(&env);
    write(&ws, "Source/a.cpp", "two\n");
    write(&ws, "Source/b.txt", "two\n");
    let names = |args: &[&str]| -> String {
        let mut full = vec!["diff", "--name-only"];
        full.extend_from_slice(args);
        env.ok(&ws, "alice", &full)
    };
    assert_eq!(names(&["Source/*.cpp"]), "Source/a.cpp\n");
    assert_eq!(names(&[":!Source/*.cpp"]), "Source/b.txt\n");
    assert_eq!(names(&[":(glob)**/b.*"]), "Source/b.txt\n");
}

#[test]
fn ignored_files_need_force_and_dry_run_stages_nothing() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "one", None);
    let ws = clone(&env);
    write(&ws, ".pyn/ignore", "*.log\n");
    write(&ws, "build.log", "log");
    write(&ws, "Source/new.cpp", "new");

    let out = env.ok(&ws, "alice", &["add", "-n", "."]);
    assert!(
        out.contains("add 'Source/new.cpp'") && !out.contains("build.log"),
        "{out}"
    );
    assert_eq!(
        staged_cell(&env, &ws, "Source/new.cpp"),
        "-",
        "a dry run stages nothing"
    );

    let out = env.run(&ws, "alice", &["add", "build.log"]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("ignored") && err.contains("-f"), "{err}");
    env.ok(&ws, "alice", &["add", "-f", "build.log"]);
    assert_eq!(staged_cell(&env, &ws, "build.log"), "added");
}

#[test]
fn update_leaves_a_staged_file_alone() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "one", None);
    let ws = clone(&env);
    write(&ws, "Source/a.cpp", "two");
    env.ok(&ws, "alice", &["add", "Source/a.cpp"]);

    env.seed("bob", "Source/a.cpp", "bob", Some(1));
    let out = env.ok(&ws, "alice", &["update"]);
    assert!(out.contains("Source/a.cpp: staged"), "{out}");
    assert_eq!(
        std::fs::read_to_string(ws.join("Source/a.cpp")).unwrap(),
        "two"
    );
    let err = env.fails(&ws, "alice", &["commit", "-m", "late"]);
    assert!(err.contains("behind") || err.contains("head"), "{err}");
}

#[test]
fn a_deleted_file_is_staged_and_committed_as_a_deletion() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "one", None);
    env.seed("alice", "Source/gone.cpp", "one", None);
    let ws = clone(&env);
    std::fs::remove_file(ws.join("Source/gone.cpp")).unwrap();
    assert!(
        env.ok(&ws, "alice", &["status"])
            .contains("deleted locally")
    );

    let out = env.ok(&ws, "alice", &["add", "-n", "-u"]);
    assert!(out.contains("remove 'Source/gone.cpp'"), "{out}");
    env.ok(&ws, "alice", &["add", "Source/gone.cpp"]);
    assert_eq!(staged_cell(&env, &ws, "Source/gone.cpp"), "deleted");
    let status = env.ok(&ws, "alice", &["status"]);
    assert!(!status.contains("deleted locally"), "{status}");
    assert_eq!(env.ok(&ws, "alice", &["diff"]), "", "nothing unstaged");
    let cached = env.ok(&ws, "alice", &["diff", "--cached"]);
    assert!(
        cached.contains("deleted file mode 100644") && cached.contains("-one"),
        "{cached}"
    );

    let out = env.ok(&ws, "alice", &["commit", "-m", "drop gone"]);
    assert!(
        out.contains("Source/gone.cpp was deleted at revision 2"),
        "{out}"
    );
    assert!(
        env.ok(&ws, "alice", &["status"])
            .contains("everything is up to date")
    );
    assert!(!env.ok(&ws, "alice", &["files"]).contains("Source/gone.cpp"));
    let log = env.ok(&ws, "alice", &["log", "Source/gone.cpp"]);
    assert!(log.contains("drop gone (deleted)"), "{log}");
    assert_eq!(env.ok(&ws, "alice", &["verify"]), "");
    let audit = env.ok(&ws, "alice", &["audit", "--action", "delete"]);
    assert!(audit.contains("Source/gone.cpp"), "{audit}");
    let err = env.fails(&ws, "alice", &["commit", "-m", "again"]);
    assert!(err.contains("nothing to commit"), "{err}");
}

#[test]
fn commit_all_stages_a_deletion_with_the_edits() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "one", None);
    env.seed("alice", "Source/b.cpp", "one", None);
    let ws = clone(&env);
    write(&ws, "Source/a.cpp", "two");
    std::fs::remove_file(ws.join("Source/b.cpp")).unwrap();
    let out = env.ok(&ws, "alice", &["commit", "-am", "edit and drop"]);
    assert!(
        out.contains("Source/a.cpp is now at revision 2")
            && out.contains("Source/b.cpp was deleted at revision 2"),
        "{out}"
    );
    let files = env.ok(&ws, "alice", &["files"]);
    assert!(
        files.contains("Source/a.cpp") && !files.contains("Source/b.cpp"),
        "{files}"
    );
}

#[test]
fn a_deleted_exclusive_file_needs_its_lock_and_can_be_restored() {
    let env = start();
    env.seed("alice", "Content/m.umap", "map one", None);
    let ws = clone(&env);
    let map = ws.join("Content/m.umap");
    make_writable(&map);
    std::fs::remove_file(&map).unwrap();

    let err = env.fails(&ws, "alice", &["add", "-u"]);
    assert!(
        err.contains("Content/m.umap is exclusive and you do not hold its lock")
            && err.contains("pyn lock Content/m.umap"),
        "{err}"
    );
    env.ok(&ws, "alice", &["lock", "Content/m.umap"]);
    std::fs::remove_file(&map).ok();
    env.ok(&ws, "alice", &["add", "-u"]);
    assert_eq!(staged_cell(&env, &ws, "Content/m.umap"), "deleted");
    env.ok(&ws, "alice", &["commit", "-m", "remove the map"]);
    let locks = env.ok(&ws, "alice", &["locks", "--mine"]);
    assert!(locks.contains("no locks"), "{locks}");

    env.ok(&ws, "alice", &["lock", "Content/m.umap"]);
    env.ok(
        &ws,
        "alice",
        &["restore", "--source", "1", "--yes", "Content/m.umap"],
    );
    env.ok(&ws, "alice", &["update"]);
    assert_eq!(std::fs::read_to_string(&map).unwrap(), "map one");
    assert!(
        env.ok(&ws, "alice", &["status"])
            .contains("everything is up to date")
    );
}

#[test]
fn update_removes_a_file_the_server_deleted_unless_it_has_local_changes() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "one", None);
    env.seed("alice", "Source/b.cpp", "one", None);
    let ws = clone(&env);
    let other = env.dir("other");
    env.ok(
        &env.dir("home"),
        "bob",
        &[
            "clone",
            "--no-local-store",
            &env.source(REPO),
            other.to_str().unwrap(),
        ],
    );
    let other = other.canonicalize().unwrap();
    for path in ["Source/a.cpp", "Source/b.cpp"] {
        std::fs::remove_file(ws.join(path)).unwrap();
    }
    env.ok(&ws, "alice", &["commit", "-am", "drop both"]);

    write(&other, "Source/b.cpp", "bob edit");
    let out = env.ok(&other, "bob", &["update"]);
    assert!(out.contains("removed 1"), "{out}");
    assert!(out.contains("Source/b.cpp: local changes"), "{out}");
    assert!(!other.join("Source/a.cpp").exists());
    assert!(other.join("Source/b.cpp").exists());
}

#[test]
fn committing_named_paths_still_works_and_unstages_them() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "one", None);
    let ws = clone(&env);
    write(&ws, "Source/a.cpp", "two");
    env.ok(&ws, "alice", &["add", "Source/a.cpp"]);
    env.ok(&ws, "alice", &["commit", "Source/a.cpp", "-m", "by path"]);
    assert!(
        env.ok(&ws, "alice", &["status"])
            .contains("everything is up to date")
    );
    let err = env.fails(&ws, "alice", &["commit", "-a", "Source/a.cpp", "-m", "x"]);
    assert!(err.contains("name no paths"), "{err}");
}

#[test]
fn the_index_is_kept_under_local_only() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "one", None);
    let ws = clone(&env);
    write(&ws, "Source/a.cpp", "two");
    env.ok(&ws, "alice", &["add", "Source/a.cpp"]);
    let index = std::fs::read_to_string(ws.join(".pyn/local_only/state/index.toml")).unwrap();
    assert!(
        index.contains("version = 1") && index.contains("[entries.\"Source/a.cpp\"]"),
        "{index}"
    );
    assert_eq!(
        std::fs::read_dir(ws.join(".pyn/local_only/staged"))
            .unwrap()
            .count(),
        1
    );
    env.ok(&ws, "alice", &["restore", "--staged", "."]);
    assert!(!ws.join(".pyn/local_only/state/index.toml").exists());
    assert_eq!(
        std::fs::read_dir(ws.join(".pyn/local_only/staged"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn a_symlink_and_an_executable_bit_are_staged_with_their_mode() {
    use std::os::unix::fs::symlink;
    let env = start();
    env.seed("alice", "Source/a.cpp", "one", None);
    let ws = clone(&env);
    symlink("a.cpp", ws.join("Source/link")).unwrap();
    write(&ws, "Source/run.sh", "#!/bin/sh\n");
    std::fs::set_permissions(
        ws.join("Source/run.sh"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    env.ok(&ws, "alice", &["add", "Source"]);
    env.ok(&ws, "alice", &["commit", "-m", "modes"]);
    let other = env.dir("other");
    env.ok(
        &env.dir("home"),
        "alice",
        &[
            "clone",
            "--no-local-store",
            &env.source(REPO),
            other.to_str().unwrap(),
        ],
    );
    let other = other.canonicalize().unwrap();
    assert_eq!(
        std::fs::read_link(other.join("Source/link")).unwrap(),
        PathBuf::from("a.cpp")
    );
    let mode = std::fs::metadata(other.join("Source/run.sh"))
        .unwrap()
        .permissions()
        .mode();
    assert_ne!(mode & 0o111, 0);
}
