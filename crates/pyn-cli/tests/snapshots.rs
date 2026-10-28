//! Commits, whole-tree snapshots (clone, status and update at a commit) and `pyn diff` / `pyn show`.

mod common;

use common::{REPO, start};

/// Four commits, oldest first: a, Content/m, a edited, b added (HEAD~3 to HEAD).
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
    assert_eq!(ids.len(), 4, "{out}");
    for id in &ids {
        assert!(
            id.len() == 10 && id.bytes().all(|b| b.is_ascii_hexdigit()),
            "an abbreviated hash, not a number: {id}"
        );
    }
    assert_eq!(rows[1][1], "alice");
}

#[test]
fn commits_are_named_by_abbreviated_hash_head_steps_and_caret() {
    let env = start();
    history(&env);
    let home = env.dir("home");
    let log = common::cell_rows(&env.ok(&home, "alice", &["log", "--commits"]));
    let ids: Vec<String> = log[1..].iter().map(|r| r[0].clone()).collect();

    let show = |rev: &str| env.ok(&home, "alice", &["show", "--name-only", rev]);
    for (i, id) in ids.iter().enumerate() {
        let by_hash = show(id);
        assert!(by_hash.starts_with("commit "), "{by_hash}");
        assert_eq!(show(&format!("HEAD~{i}")), by_hash);
        assert_eq!(
            show(&id[..6]),
            by_hash,
            "a shorter prefix names the same commit"
        );
        if i + 1 < ids.len() {
            assert_eq!(show(&format!("{id}~1")), show(&ids[i + 1]));
            assert_eq!(show(&format!("{id}^")), show(&ids[i + 1]));
        }
    }
    assert_eq!(show("HEAD^"), show(&ids[1]));
    assert_eq!(show("HEAD^^"), show(&ids[2]));

    let full = env.ok(&home, "alice", &["show", "--name-only", &ids[3]]);
    let long = full.lines().next().unwrap().trim_start_matches("commit ");
    assert_eq!(long.len(), 64, "{full}");
    assert!(long.starts_with(&ids[3]));

    let err = env.fails(&home, "alice", &["show", "c3"]);
    assert!(err.contains("not a commit"), "{err}");
    let err = env.fails(&home, "alice", &["show", "HEAD~9"]);
    assert!(err.contains("before the first commit"), "{err}");
    let err = env.fails(&home, "alice", &["show", "HEAD^2"]);
    assert!(err.contains("no parent 2"), "{err}");
}

#[test]
fn clone_at_a_commit_has_the_files_as_they_were() {
    let env = start();
    history(&env);
    let ws = clone_at(&env, "old", &["HEAD~2"]);
    assert_eq!(
        std::fs::read_to_string(ws.join("Source/a.txt")).unwrap(),
        "one\ntwo\nthree\n"
    );
    assert!(ws.join("Content/m.umap").is_file());
    assert!(!ws.join("Source/b.txt").exists(), "b came after HEAD~2");

    let head = clone_at(&env, "head~1", &["HEAD~1"]);
    assert_eq!(
        std::fs::read_to_string(head.join("Source/a.txt")).unwrap(),
        "one\n2\nthree\n"
    );
    assert!(!head.join("Source/b.txt").exists());

    let none = env.fails(
        &env.dir("home"),
        "alice",
        &["clone", "--revision", "deadbeef", &env.source(REPO), "x"],
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

    let first = common::cell_rows(&env.ok(&ws, "alice", &["log", "--commits"]))
        .pop()
        .unwrap()[0]
        .clone();
    let out = env.ok(&ws, "alice", &["status", "--revision", &first]);
    assert!(out.contains(&format!("ahead ({first} has r1)")), "{out}");
    assert!(out.contains(&format!("ahead (not in {first})")), "{out}");
    assert!(out.contains("3 ahead"), "{out}");

    let out = env.ok(&ws, "alice", &["update", "--revision", &first]);
    assert!(out.contains("updated 1 file(s), removed 2"), "{out}");
    assert_eq!(
        std::fs::read_to_string(ws.join("Source/a.txt")).unwrap(),
        "one\ntwo\nthree\n"
    );
    assert!(!ws.join("Source/b.txt").exists());
    let out = env.ok(&ws, "alice", &["status", "--revision", "HEAD~3"]);
    assert_eq!(out.trim(), "On branch main\neverything is up to date");

    std::fs::write(ws.join("Source/a.txt"), "mine").unwrap();
    let out = env.ok(&ws, "alice", &["update", "--revision", "HEAD"]);
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

    let staged = env.run(&ws, "alice", &["diff", "--cached", "--exit-code"]);
    assert_eq!(staged.status.code(), Some(0), "nothing is staged");
    assert!(staged.stdout.is_empty());
}

#[test]
fn diff_between_commits_works_outside_a_workspace() {
    let env = start();
    history(&env);
    let home = env.dir("home");
    let out = env.ok(&home, "alice", &["diff", "HEAD~3", "HEAD~1"]);
    assert!(out.contains("-two\n+2\n"), "{out}");
    assert!(!out.contains("b.txt"), "b is not in HEAD~1: {out}");
    assert_eq!(env.ok(&home, "alice", &["diff", "HEAD~3..HEAD~1"]), out);

    let names = env.ok(&home, "alice", &["diff", "--name-status", "HEAD~3", "HEAD"]);
    assert_eq!(
        names,
        "A\tContent/m.umap\nM\tSource/a.txt\nA\tSource/b.txt\n"
    );
    let only = env.ok(
        &home,
        "alice",
        &["diff", "--name-only", "HEAD~3", "HEAD", "Source/b.txt"],
    );
    assert_eq!(only, "Source/b.txt\n");

    let stat = env.ok(
        &home,
        "alice",
        &["diff", "--stat", "HEAD~3", "HEAD", "Source"],
    );
    let lines: Vec<&str> = stat.lines().collect();
    assert_eq!(lines[0], " Source/a.txt | 2 +-");
    assert_eq!(lines[1], " Source/b.txt | 1 +");
    assert_eq!(lines[2], " 2 files changed, 2 insertions(+), 1 deletion(-)");

    let wide = env.ok(&home, "alice", &["diff", "-U0", "HEAD~3", "HEAD~1"]);
    assert!(wide.contains("@@ -2 +2 @@ one\n-two\n+2\n"), "{wide}");

    let err = env.fails(&home, "alice", &["diff", "HEAD~3", "deadbeef"]);
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

    let out = env.ok(&home, "alice", &["diff", "HEAD~1", "HEAD"]);
    assert!(
        out.contains("Binary files a/blob.bin and b/blob.bin differ"),
        "{out}"
    );
    assert!(!out.contains("@@"), "{out}");
    let added = env.ok(&home, "alice", &["diff", "HEAD~2", "HEAD~1"]);
    assert!(added.contains("new file mode 100644"), "{added}");
    assert!(
        added.contains("Binary files /dev/null and b/blob.bin differ"),
        "{added}"
    );
    let stat = env.ok(&home, "alice", &["diff", "--stat", "HEAD~1", "HEAD"]);
    assert!(stat.contains("blob.bin | Bin"), "{stat}");

    let ws = env.dir("ws");
    env.ok(
        &home,
        "alice",
        &[
            "clone",
            "--revision",
            "HEAD~4",
            &env.source(REPO),
            ws.to_str().unwrap(),
        ],
    );
    let ws = ws.canonicalize().unwrap();
    let out = env.ok(&ws, "alice", &["diff", "HEAD~2"]);
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
    let out = env.ok(&home, "alice", &["show", "HEAD~1"]);
    let lines: Vec<&str> = out.lines().collect();
    assert!(
        lines[0].len() == "commit ".len() + 64 && lines[0].starts_with("commit "),
        "{out}"
    );
    assert_eq!(lines[1], "Author: alice <alice@pyn.invalid>");
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
    assert!(head.starts_with("commit "), "{head}");
    assert!(head.ends_with("A\tSource/b.txt\n"), "{head}");

    let first = env.ok(&home, "alice", &["show", "HEAD~3", "--stat"]);
    assert!(first.contains("Source/a.txt | 3 +++"), "{first}");
}

#[test]
fn diff_and_show_take_shortstat_ignore_whitespace_and_check() {
    let env = start();
    env.seed("alice", "Source/w.txt", "a b\nkeep\nz\n", None);
    env.seed("alice", "Source/w.txt", "ab\nkeep  \nZ \n", Some(1));
    env.seed("alice", "Source/s.txt", "x\n", None);
    env.seed("alice", "Source/s.txt", " x\n", Some(1));
    let home = env.dir("home");
    let diff = |args: &[&str]| {
        let mut all = vec!["diff"];
        all.extend(args);
        all.extend(["HEAD~3", "HEAD"]);
        env.run(&home, "alice", &all)
    };
    let text = |o: &std::process::Output| String::from_utf8_lossy(&o.stdout).into_owned();

    assert_eq!(
        text(&diff(&["--shortstat"])),
        " 2 files changed, 4 insertions(+), 3 deletions(-)\n"
    );
    assert_eq!(
        text(&diff(&["-w", "--shortstat"])),
        " 2 files changed, 2 insertions(+), 1 deletion(-)\n"
    );
    assert_eq!(
        text(&diff(&["-w", "--stat"])),
        " Source/s.txt | 1 +\n Source/w.txt | 2 +-\n 2 files changed, 2 insertions(+), 1 deletion(-)\n"
    );
    let patch = text(&diff(&["-w"]));
    assert!(
        patch.ends_with("@@ -1,3 +1,3 @@\n ab\n keep  \n-z\n+Z \n"),
        "{patch}"
    );
    let ws_only = env.run(
        &home,
        "alice",
        &["diff", "-w", "--exit-code", "HEAD~1", "HEAD"],
    );
    assert_eq!(ws_only.status.code(), Some(0));
    assert!(ws_only.stdout.is_empty());

    let checked = diff(&["--check"]);
    assert_eq!(checked.status.code(), Some(2));
    assert_eq!(
        text(&checked),
        "Source/w.txt:2: trailing whitespace.\n+keep  \nSource/w.txt:3: trailing whitespace.\n+Z \n"
    );
    let clean = env.run(&home, "alice", &["diff", "--check", "HEAD~1", "HEAD"]);
    assert_eq!(clean.status.code(), Some(0));
    assert!(clean.stdout.is_empty());

    let shown = env.run(&home, "alice", &["show", "--check", "HEAD~2"]);
    assert_eq!(shown.status.code(), Some(2));
    assert!(text(&shown).ends_with("\n\nSource/w.txt:2: trailing whitespace.\n+keep  \nSource/w.txt:3: trailing whitespace.\n+Z \n"));
    let short = env.ok(&home, "alice", &["show", "--shortstat", "HEAD~2"]);
    assert!(
        short.ends_with("\n\n 1 file changed, 3 insertions(+), 3 deletions(-)\n"),
        "{short}"
    );
    let ignore = env.ok(&home, "alice", &["show", "-w", "HEAD~2"]);
    assert!(ignore.contains("-z\n+Z \n"), "{ignore}");

    let err = env.fails(&home, "alice", &["diff", "--check", "--stat"]);
    assert!(err.contains("cannot be used with"), "{err}");
}
