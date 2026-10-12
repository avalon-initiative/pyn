//! Commits, whole-tree snapshots (clone, status and update at a commit) and `pyn diff` / `pyn show`.

mod common;

use common::{REPO, start};

/// Four commits: c1 a, c2 Content/m, c3 a edited, c4 b added.
fn history(env: &common::Env) {
    env.seed("alice", "Source/a.txt", "one\ntwo\nthree\n", None);
    env.seed("alice", "Content/m.umap", "map one", None);
    env.seed("alice", "Source/a.txt", "one\n2\nthree\n", Some(1));
    env.seed("alice", "Source/b.txt", "bee\n", None);
}

fn clone_at(env: &common::Env, name: &str, args: &[&str]) -> std::path::PathBuf {
    let ws = env.dir(name);
    let mut all = vec!["clone", "--revision"];
    all.extend(args);
    let src = env.source(REPO);
    all.push(&src);
    all.push(ws.to_str().unwrap());
    env.ok(&env.dir("home"), "alice", &all);
    ws.canonicalize().unwrap()
}

#[test]
fn log_commits_lists_every_write_newest_first() {
    let env = start();
    history(&env);
    let out = env.ok(&env.dir("home"), "alice", &["log", "--commits"]);
    let rows = common::cell_rows(&out);
    assert_eq!(rows[0], ["COMMIT", "AUTHOR", "WHEN", "FILES", "MESSAGE"]);
    let ids: Vec<&str> = rows[1..].iter().map(|r| r[0].as_str()).collect();
    assert_eq!(ids, ["c4", "c3", "c2", "c1"], "{out}");
}

#[test]
fn clone_at_a_commit_has_the_files_as_they_were() {
    let env = start();
    history(&env);
    let ws = clone_at(&env, "old", &["c2"]);
    assert_eq!(
        std::fs::read_to_string(ws.join("Source/a.txt")).unwrap(),
        "one\ntwo\nthree\n"
    );
    assert!(ws.join("Content/m.umap").is_file());
    assert!(!ws.join("Source/b.txt").exists(), "b came after c2");

    let head = clone_at(&env, "head~1", &["HEAD~1"]);
    assert_eq!(
        std::fs::read_to_string(head.join("Source/a.txt")).unwrap(),
        "one\n2\nthree\n"
    );
    assert!(!head.join("Source/b.txt").exists());

    let none = env.fails(
        &env.dir("home"),
        "alice",
        &["clone", "--revision", "c9", &env.source(REPO), "x"],
    );
    assert!(none.contains("commit_not_found"), "{none}");
}

#[test]
fn status_and_update_at_a_commit_move_the_workspace_both_ways() {
    let env = start();
    history(&env);
    let ws = env.dir("ws");
    env.ok(
        &env.dir("home"),
        "alice",
        &["clone", &env.source(REPO), ws.to_str().unwrap()],
    );
    let ws = ws.canonicalize().unwrap();

    let out = env.ok(&ws, "alice", &["status", "--revision", "c1"]);
    assert!(out.contains("ahead (c1 has r1)"), "{out}");
    assert!(out.contains("ahead (not in c1)"), "{out}");
    assert!(out.contains("3 ahead"), "{out}");

    let out = env.ok(&ws, "alice", &["update", "--revision", "c1"]);
    assert!(out.contains("updated 1 file(s), removed 2"), "{out}");
    assert_eq!(
        std::fs::read_to_string(ws.join("Source/a.txt")).unwrap(),
        "one\ntwo\nthree\n"
    );
    assert!(!ws.join("Source/b.txt").exists());
    let out = env.ok(&ws, "alice", &["status", "--revision", "c1"]);
    assert_eq!(out.trim(), "everything is up to date");

    std::fs::write(ws.join("Source/a.txt"), "mine").unwrap();
    let out = env.ok(&ws, "alice", &["update", "--revision", "c4"]);
    assert!(out.contains("skipped Source/a.txt: local changes"), "{out}");
    assert_eq!(
        std::fs::read_to_string(ws.join("Source/a.txt")).unwrap(),
        "mine"
    );
    assert!(ws.join("Source/b.txt").is_file());
}

#[test]
fn diff_of_the_workspace_is_a_git_shaped_unified_diff() {
    let env = start();
    history(&env);
    let ws = env.dir("ws");
    env.ok(
        &env.dir("home"),
        "alice",
        &["clone", &env.source(REPO), ws.to_str().unwrap()],
    );
    let ws = ws.canonicalize().unwrap();
    assert_eq!(env.ok(&ws, "alice", &["diff"]), "");

    std::fs::write(ws.join("Source/a.txt"), "one\nTWO\nthree\nfour\n").unwrap();
    std::fs::remove_file(ws.join("Source/b.txt")).unwrap();
    let out = env.ok(&ws, "alice", &["diff"]);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "diff --git a/Source/a.txt b/Source/a.txt");
    assert!(
        lines[1].starts_with("index ") && lines[1].ends_with(" 100644"),
        "{out}"
    );
    assert_eq!(&lines[2..4], ["--- a/Source/a.txt", "+++ b/Source/a.txt"]);
    assert_eq!(
        &lines[4..10],
        ["@@ -1,3 +1,4 @@", " one", "-2", "+TWO", " three", "+four"]
    );
    assert_eq!(lines[10], "diff --git a/Source/b.txt b/Source/b.txt");
    assert_eq!(lines[11], "deleted file mode 100644");
    assert!(lines[12].ends_with("..0000000"), "{out}");
    assert_eq!(
        &lines[13..],
        [
            "--- a/Source/b.txt",
            "+++ /dev/null",
            "@@ -1 +0,0 @@",
            "-bee"
        ]
    );

    let only = env.ok(&ws, "alice", &["diff", "--", "Source/b.txt"]);
    assert!(only.starts_with("diff --git a/Source/b.txt"), "{only}");
    assert!(!only.contains("a.txt"));
    let dot = env.ok(&ws.join("Source"), "alice", &["diff", "a.txt"]);
    assert!(dot.contains("+TWO") && !dot.contains("b.txt"), "{dot}");
}

#[test]
fn diff_exit_codes_follow_git() {
    let env = start();
    history(&env);
    let ws = env.dir("ws");
    env.ok(
        &env.dir("home"),
        "alice",
        &["clone", &env.source(REPO), ws.to_str().unwrap()],
    );
    let ws = ws.canonicalize().unwrap();

    let same = env.run(&ws, "alice", &["diff", "--exit-code"]);
    assert_eq!(same.status.code(), Some(0));
    std::fs::write(ws.join("Source/a.txt"), "changed\n").unwrap();
    let differs = env.run(&ws, "alice", &["diff", "--exit-code"]);
    assert_eq!(differs.status.code(), Some(1));
    assert!(!differs.stdout.is_empty());
    let plain = env.run(&ws, "alice", &["diff"]);
    assert_eq!(
        plain.status.code(),
        Some(0),
        "without --exit-code a diff exits 0"
    );
    let quiet = env.run(&ws, "alice", &["diff", "--quiet"]);
    assert_eq!(quiet.status.code(), Some(1));
    assert!(quiet.stdout.is_empty());

    let staged = env.fails(&ws, "alice", &["diff", "--cached"]);
    assert!(staged.contains("no staging area"), "{staged}");
}

#[test]
fn diff_between_commits_works_outside_a_workspace() {
    let env = start();
    history(&env);
    let home = env.dir("home");
    let out = env.ok(&home, "alice", &["diff", "c1", "c3"]);
    assert!(out.contains("-two\n+2\n"), "{out}");
    assert!(!out.contains("b.txt"), "b is not in c3: {out}");
    assert_eq!(env.ok(&home, "alice", &["diff", "c1..c3"]), out);

    let names = env.ok(&home, "alice", &["diff", "--name-status", "c1", "HEAD"]);
    assert_eq!(
        names,
        "A\tContent/m.umap\nM\tSource/a.txt\nA\tSource/b.txt\n"
    );
    let only = env.ok(
        &home,
        "alice",
        &["diff", "--name-only", "c1", "HEAD", "Source/b.txt"],
    );
    assert_eq!(only, "Source/b.txt\n");

    let stat = env.ok(&home, "alice", &["diff", "--stat", "c1", "HEAD", "Source"]);
    let lines: Vec<&str> = stat.lines().collect();
    assert_eq!(lines[0], " Source/a.txt | 2 +-");
    assert_eq!(lines[1], " Source/b.txt | 1 +");
    assert_eq!(lines[2], " 2 files changed, 2 insertions(+), 1 deletion(-)");

    let wide = env.ok(&home, "alice", &["diff", "-U0", "c1", "c3"]);
    assert!(wide.contains("@@ -2 +2 @@\n-two\n+2\n"), "{wide}");

    let err = env.fails(&home, "alice", &["diff", "c1", "c7"]);
    assert!(err.contains("commit_not_found"), "{err}");
}

#[test]
fn diff_against_a_commit_compares_the_workspace_and_reports_binary_files() {
    let env = start();
    history(&env);
    let bin = env.dir("bin").join("blob.bin");
    std::fs::write(&bin, b"\x00\x01\x02first").unwrap();
    let home = env.dir("home");
    env.ok(
        &home,
        "alice",
        &[
            "commit",
            "blob.bin",
            "--file",
            bin.to_str().unwrap(),
            "-m",
            "bin",
        ],
    );
    std::fs::write(&bin, b"\x00\x01\x02second").unwrap();
    env.ok(
        &home,
        "alice",
        &[
            "commit",
            "blob.bin",
            "--file",
            bin.to_str().unwrap(),
            "-m",
            "bin 2",
            "--base",
            "1",
        ],
    );

    let out = env.ok(&home, "alice", &["diff", "c5", "c6"]);
    assert!(
        out.contains("Binary files a/blob.bin and b/blob.bin differ"),
        "{out}"
    );
    assert!(!out.contains("@@"), "{out}");
    let added = env.ok(&home, "alice", &["diff", "c4", "c5"]);
    assert!(added.contains("new file mode 100644"), "{added}");
    assert!(
        added.contains("Binary files /dev/null and b/blob.bin differ"),
        "{added}"
    );
    let stat = env.ok(&home, "alice", &["diff", "--stat", "c5", "c6"]);
    assert!(stat.contains("blob.bin | Bin"), "{stat}");

    let ws = env.dir("ws");
    env.ok(
        &home,
        "alice",
        &[
            "clone",
            "--revision",
            "c2",
            &env.source(REPO),
            ws.to_str().unwrap(),
        ],
    );
    let ws = ws.canonicalize().unwrap();
    let out = env.ok(&ws, "alice", &["diff", "c4"]);
    assert!(
        out.contains("-2\n+two\n") || out.contains("-one\n"),
        "{out}"
    );
}

#[test]
fn show_prints_the_commit_and_what_it_changed() {
    let env = start();
    history(&env);
    let home = env.dir("home");
    let out = env.ok(&home, "alice", &["show", "c3"]);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "commit c3");
    assert_eq!(lines[1], "Author: alice");
    assert!(lines[2].starts_with("Date:   "), "{out}");
    assert_eq!(lines[3], "");
    assert_eq!(lines[4], "    seed");
    assert!(
        out.contains("diff --git a/Source/a.txt b/Source/a.txt"),
        "{out}"
    );
    assert!(out.contains("-two\n+2\n"), "{out}");
    assert!(!out.contains("b.txt"));

    let head = env.ok(&home, "alice", &["show", "--name-status"]);
    assert!(head.starts_with("commit c4\n"), "{head}");
    assert!(head.ends_with("A\tSource/b.txt\n"), "{head}");

    let first = env.ok(&home, "alice", &["show", "c1", "--stat"]);
    assert!(first.contains("Source/a.txt | 3 +++"), "{first}");
}
