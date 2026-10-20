//! `log` with `--stat`, `--shortstat`, `--name-only`, `--name-status`, `-p` and `-U<n>` on a pyn server.

mod common;

use std::path::{Path, PathBuf};

use common::{Env, REPO, start};

/// A clone with a published seed commit and one more commit that edits `a.txt` and adds `b.txt`.
fn setup() -> (Env, PathBuf) {
    let env = start();
    env.seed("alice", "Source/a.txt", "one\ntwo\nthree\n", None);
    let ws = env.dir("ws");
    env.ok(
        &env.dir("home"),
        "alice",
        &["clone", &env.source(REPO), ws.to_str().unwrap()],
    );
    let ws = ws.canonicalize().unwrap();
    std::fs::write(ws.join("Source/a.txt"), "one\n2\nthree\n").unwrap();
    std::fs::write(ws.join("Source/b.txt"), "new\n").unwrap();
    env.ok(&ws, "alice", &["add", "Source/b.txt"]);
    env.ok(
        &ws,
        "alice",
        &["commit", "Source/a.txt", "Source/b.txt", "-m", "change"],
    );
    (env, ws)
}

fn run(env: &Env, ws: &Path, args: &[&str]) -> String {
    env.ok(ws, "alice", args)
}

#[test]
fn stat_and_shortstat_follow_each_commit() {
    let (env, ws) = setup();
    assert_eq!(
        run(&env, &ws, &["log", "--format=%s", "--stat"]),
        "change\n\n Source/a.txt | 2 +-\n Source/b.txt | 1 +\n 2 files changed, 2 insertions(+), 1 deletion(-)\n\
         seed\n\n Source/a.txt | 3 +++\n 1 file changed, 3 insertions(+)\n"
    );
    assert_eq!(
        run(&env, &ws, &["log", "--format=%s", "--shortstat", "-n", "1"]),
        "change\n\n 2 files changed, 2 insertions(+), 1 deletion(-)\n"
    );
}

#[test]
fn name_flags_list_the_changed_paths() {
    let (env, ws) = setup();
    assert_eq!(
        run(&env, &ws, &["log", "--format=%s", "--name-only"]),
        "change\n\nSource/a.txt\nSource/b.txt\nseed\n\nSource/a.txt\n"
    );
    assert_eq!(
        run(&env, &ws, &["log", "--oneline", "--name-status", "-n", "1"])
            .split_once(' ')
            .unwrap()
            .1,
        "change\nM\tSource/a.txt\nA\tSource/b.txt\n"
    );
}

#[test]
fn patch_and_context_follow_each_commit() {
    let (env, ws) = setup();
    let out = run(&env, &ws, &["log", "-p", "-n", "1"]);
    assert!(out.starts_with("commit "), "{out}");
    assert!(
        out.contains("\n\n    change\n\ndiff --git a/Source/a.txt b/Source/a.txt\n"),
        "{out}"
    );
    assert!(
        out.contains("@@ -1,3 +1,3 @@\n one\n-two\n+2\n three\n"),
        "{out}"
    );
    assert!(
        out.contains("+++ b/Source/b.txt\n@@ -0,0 +1 @@\n+new\n"),
        "{out}"
    );
    let tight = run(&env, &ws, &["log", "-U0", "--format=%s", "-n", "1"]);
    assert!(tight.contains("@@ -2 +2 @@ one\n-two\n+2\n"), "{tight}");
    let wide = run(
        &env,
        &ws,
        &["log", "--stat", "-p", "--format=%s", "-n", "1"],
    );
    assert!(
        wide.starts_with("change\n---\n Source/a.txt | 2 +-\n"),
        "{wide}"
    );
    assert!(wide.contains("\n\ndiff --git a/Source/a.txt"), "{wide}");
}

#[test]
fn the_first_commit_is_diffed_against_nothing_and_diff_flags_reject_paths() {
    let (env, ws) = setup();
    let out = run(&env, &ws, &["log", "--oneline", "--name-status", "-n", "2"]);
    assert!(out.ends_with("seed\nA\tSource/a.txt\n"), "{out}");
    let out = env.run(&ws, "alice", &["log", "--stat", "Source/a.txt"]);
    assert_eq!(out.status.code(), Some(129));
    let out = env.run(&ws, "alice", &["log", "--commits", "--stat", "--name-only"]);
    assert_eq!(out.status.code(), Some(129));
}

#[test]
fn unpublished_local_commits_are_diffed_from_the_local_store() {
    let env = start();
    env.seed("alice", "Source/a.txt", "one\n", None);
    let ws = env.dir("ws");
    env.ok(
        &env.dir("home"),
        "alice",
        &[
            "clone",
            "--local-store",
            &env.source(REPO),
            ws.to_str().unwrap(),
        ],
    );
    let ws = ws.canonicalize().unwrap();
    std::fs::write(ws.join("Source/a.txt"), "one\ntwo\n").unwrap();
    std::fs::write(ws.join("Source/c.txt"), "x\n").unwrap();
    env.ok(&ws, "alice", &["add", "Source"]);
    env.ok(&ws, "alice", &["commit", "-m", "local one"]);
    assert_eq!(
        run(&env, &ws, &["log", "--format=%s", "--stat"]),
        "local one\n\n Source/a.txt | 1 +\n Source/c.txt | 1 +\n 2 files changed, 2 insertions(+)\n\
         seed\n\n Source/a.txt | 1 +\n 1 file changed, 1 insertion(+)\n"
    );
}
