//! `log` and `show` with `--oneline`, `--format` and `-s` on a pyn server, whose commits convert to the git-home renderer's view.

mod common;

use std::path::{Path, PathBuf};

use common::{Env, REPO, start};

fn setup() -> (Env, PathBuf) {
    let env = start();
    env.seed("alice", "Source/a.txt", "one\n", None);
    let ws = env.dir("ws");
    env.ok(
        &env.dir("home"),
        "alice",
        &["clone", &env.source(REPO), ws.to_str().unwrap()],
    );
    let ws = ws.canonicalize().unwrap();
    std::fs::write(ws.join("Source/a.txt"), "two\n").unwrap();
    env.ok(
        &ws,
        "alice",
        &[
            "commit",
            "Source/a.txt",
            "-m",
            "change a\n\nlonger body\nsecond line",
        ],
    );
    (env, ws)
}

fn run(env: &Env, ws: &Path, args: &[&str]) -> String {
    env.ok(ws, "alice", args)
}

/// The ids of the two commits, newest first.
fn ids(env: &Env, ws: &Path) -> Vec<String> {
    run(env, ws, &["log", "--format=%H"])
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn log_oneline_lists_commits_with_short_ids() {
    let (env, ws) = setup();
    let ids = ids(&env, &ws);
    assert_eq!(ids.len(), 2);
    assert_eq!(
        run(&env, &ws, &["log", "--oneline"]),
        format!("{} change a\n{} seed\n", &ids[0][..7], &ids[1][..7])
    );
    assert_eq!(
        run(&env, &ws, &["log", "--oneline", "-n", "1"]),
        format!("{} change a\n", &ids[0][..7])
    );
    assert_eq!(
        run(&env, &ws, &["log", "--pretty=oneline", "-n", "1"]),
        format!("{} change a\n", ids[0])
    );
}

#[test]
fn log_format_expands_placeholders() {
    let (env, ws) = setup();
    let ids = ids(&env, &ws);
    assert_eq!(
        run(&env, &ws, &["log", "--format=%an <%ae>|%cn|%s|%p|%%"]),
        format!(
            "alice <alice@pyn.invalid>|alice|change a|{}|%\nalice <alice@pyn.invalid>|alice|seed||%\n",
            &ids[1][..7]
        )
    );
    assert_eq!(
        run(&env, &ws, &["log", "-n", "1", "--format=%b"]),
        "longer body\nsecond line\n"
    );
    assert_eq!(
        run(&env, &ws, &["log", "-n", "1", "--format=format:%s"]),
        "change a"
    );
    assert_eq!(
        run(&env, &ws, &["log", "--format=%s%d", "-n", "1"]),
        "change a\n"
    );
}

#[test]
fn log_named_formats_and_date_styles() {
    let (env, ws) = setup();
    let ids = ids(&env, &ws);
    let medium = run(&env, &ws, &["log", "-n", "1", "--pretty"]);
    let lines: Vec<&str> = medium.lines().collect();
    assert_eq!(lines[0], format!("commit {}", ids[0]));
    assert_eq!(lines[1], "Author: alice <alice@pyn.invalid>");
    assert!(lines[2].starts_with("Date:   ") && lines[2].ends_with(" +0000"));
    assert_eq!(
        &lines[3..],
        [
            "",
            "    change a",
            "    ",
            "    longer body",
            "    second line"
        ]
    );
    let short = run(&env, &ws, &["log", "-n", "1", "--pretty=short"]);
    assert_eq!(
        short,
        format!(
            "commit {}\nAuthor: alice <alice@pyn.invalid>\n\n    change a\n",
            ids[0]
        )
    );
    let day = run(
        &env,
        &ws,
        &["log", "-n", "1", "--format=%ad", "--date=short"],
    );
    assert!(day.len() == 11 && day.starts_with("20"), "{day}");
    let unix = run(
        &env,
        &ws,
        &["log", "-n", "1", "--format=%ct", "--date=unix"],
    );
    assert!(
        unix.trim().parse::<i64>().unwrap() > 1_700_000_000,
        "{unix}"
    );
}

#[test]
fn show_formats_print_the_commit_then_the_diff() {
    let (env, ws) = setup();
    let ids = ids(&env, &ws);
    let diff = "diff --git a/Source/a.txt b/Source/a.txt\nindex a4ed1f3..aa9e7dc 100644\n\
                --- a/Source/a.txt\n+++ b/Source/a.txt\n@@ -1 +1 @@\n-one\n+two\n";
    assert_eq!(
        run(&env, &ws, &["show", "--oneline"]),
        format!("{} change a\n{diff}", &ids[0][..7])
    );
    assert_eq!(
        run(&env, &ws, &["show", "--format=%s"]),
        format!("change a\n\n{diff}")
    );
    assert_eq!(
        run(&env, &ws, &["show", "--format=format:%h"]),
        format!("{}\n{diff}", &ids[0][..7])
    );
}

#[test]
fn show_no_patch_prints_only_the_commit() {
    let (env, ws) = setup();
    let ids = ids(&env, &ws);
    assert_eq!(
        run(&env, &ws, &["show", "-s", "--oneline"]),
        format!("{} change a\n", &ids[0][..7])
    );
    assert_eq!(
        run(
            &env,
            &ws,
            &["show", "--no-patch", "--format=%an: %s", &ids[1][..8]]
        ),
        "alice: seed\n"
    );
    let plain = run(&env, &ws, &["show", "-s"]);
    assert!(
        plain.starts_with(&format!("commit {}\n", ids[0])),
        "{plain}"
    );
    assert!(!plain.contains("diff --git"), "{plain}");
}

#[test]
fn a_formatted_log_takes_no_filter_glob_and_bad_formats_fail() {
    let (env, ws) = setup();
    for args in [
        &["log", "--oneline", "--filter", "*.txt"][..],
        &["log", "--date=bogus", "--oneline"],
        &["log", "--pretty=nonsense"],
    ] {
        let out = env.run(&ws, "alice", args);
        assert!(!out.status.success(), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}");
    }
    let out = env.run(&ws, "alice", &["log", "--oneline", "--filter", "*.txt"]);
    assert_eq!(out.status.code(), Some(129));
    let out = env.run(&ws, "alice", &["log", "--committer=x"]);
    assert_eq!(out.status.code(), Some(129));
}
