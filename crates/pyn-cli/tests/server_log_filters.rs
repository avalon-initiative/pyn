//! `log --author`, `--since`, `--until`, `--grep`, `<rev>..<rev>` and `<rev> -- <path>` on a pyn server.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Duration};
use common::{Env, REPO, start_with_clock};
use pyn_core::ManualClock;

const DAY1: &str = "2026-03-01T12:00:00Z";

/// Four commits a day apart: alice seeds `Source/a.txt`, bob adds `Source/b.txt`, alice edits `a.txt` and adds `Docs/n.md`.
fn setup() -> (Env, PathBuf) {
    let clock = Arc::new(ManualClock::new(
        DateTime::parse_from_rfc3339(DAY1).unwrap().into(),
    ));
    let env = start_with_clock(clock.clone());
    let ws = env.dir("ws");
    let steps: [(&str, &str, &str, &str, &[&str]); 4] = [
        ("alice", "Source/a.txt", "one\n", "seed a", &[]),
        ("bob", "Source/b.txt", "bee\n", "Add B for the build", &[]),
        (
            "alice",
            "Source/a.txt",
            "two\n",
            "tweak a",
            &["--base", "1"],
        ),
        ("alice", "Docs/n.md", "notes\n", "write notes", &[]),
    ];
    for (i, (user, path, text, message, extra)) in steps.iter().enumerate() {
        if i > 0 {
            clock.advance(Duration::days(1));
        }
        let file = env.dir("seed").join("upload");
        std::fs::write(&file, text).unwrap();
        let mut args = vec![
            "commit",
            path,
            "--file",
            file.to_str().unwrap(),
            "-m",
            message,
        ];
        args.extend(*extra);
        env.ok(&env.dir("seed"), user, &args);
    }
    env.ok(
        &env.dir("home"),
        "alice",
        &["clone", &env.source(REPO), ws.to_str().unwrap()],
    );
    (env, ws.canonicalize().unwrap())
}

fn subjects(env: &Env, ws: &Path, args: &[&str]) -> String {
    let mut all = vec!["log", "--format=%s"];
    all.extend(args);
    env.ok(ws, "alice", &all)
}

fn refused(env: &Env, ws: &Path, args: &[&str]) -> (Option<i32>, String) {
    let out = env.run(ws, "alice", args);
    assert!(!out.status.success(), "{args:?} should fail");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stderr).into(),
    )
}

#[test]
fn author_matches_name_or_email_ignoring_case() {
    let (env, ws) = setup();
    assert_eq!(
        subjects(&env, &ws, &["--author", "BOB"]),
        "Add B for the build\n"
    );
    assert_eq!(
        subjects(&env, &ws, &["--author=alice"]),
        "write notes\ntweak a\nseed a\n"
    );
    assert_eq!(subjects(&env, &ws, &["--author", "carol"]), "");
}

#[test]
fn grep_matches_the_message_ignoring_case() {
    let (env, ws) = setup();
    assert_eq!(
        subjects(&env, &ws, &["--grep", "BUILD"]),
        "Add B for the build\n"
    );
    assert_eq!(
        subjects(&env, &ws, &["--grep", "a"]),
        "tweak a\nAdd B for the build\nseed a\n"
    );
}

#[test]
fn since_and_until_bound_the_author_date_inclusively() {
    let (env, ws) = setup();
    assert_eq!(
        subjects(&env, &ws, &["--since", "2026-03-02T12:00:00Z"]),
        "write notes\ntweak a\nAdd B for the build\n"
    );
    assert_eq!(
        subjects(&env, &ws, &["--until=2026-03-02T12:00:00Z"]),
        "Add B for the build\nseed a\n"
    );
    assert_eq!(
        subjects(
            &env,
            &ws,
            &[
                "--since=2026-03-02T12:00:00Z",
                "--until=2026-03-03T12:00:00Z"
            ]
        ),
        "tweak a\nAdd B for the build\n"
    );
    let (code, err) = refused(&env, &ws, &["log", "--since", "not a date"]);
    assert_eq!(code, Some(128));
    assert!(err.contains("unsupported date 'not a date'"), "{err}");
}

#[test]
fn filters_combine_and_honour_the_limit() {
    let (env, ws) = setup();
    assert_eq!(
        subjects(&env, &ws, &["--author", "alice", "--grep", "e", "-n", "2"]),
        "write notes\ntweak a\n"
    );
    assert_eq!(
        subjects(
            &env,
            &ws,
            &["--author", "alice", "--until", "2026-03-02T12:00:00Z"]
        ),
        "seed a\n"
    );
}

#[test]
fn filters_without_a_format_list_the_commits_table() {
    let (env, ws) = setup();
    let out = env.ok(&ws, "alice", &["log", "--author", "bob"]);
    let rows = common::cell_rows(&out);
    assert_eq!(rows[0], ["COMMIT", "AUTHOR", "WHEN", "FILES", "MESSAGE"]);
    assert_eq!(rows.len(), 2, "{out}");
    assert_eq!(rows[1][1], "bob");
    assert_eq!(rows[1][3], "1");
    assert_eq!(rows[1][4], "Add B for the build");
}

#[test]
fn ranges_follow_git_semantics() {
    let (env, ws) = setup();
    assert_eq!(
        subjects(&env, &ws, &["HEAD~2..HEAD"]),
        "write notes\ntweak a\n"
    );
    assert_eq!(subjects(&env, &ws, &["HEAD~2.."]), "write notes\ntweak a\n");
    assert_eq!(
        subjects(&env, &ws, &["HEAD~1"]),
        "tweak a\nAdd B for the build\nseed a\n"
    );
    assert_eq!(subjects(&env, &ws, &["^HEAD~1", "HEAD"]), "write notes\n");
    assert_eq!(subjects(&env, &ws, &["HEAD..HEAD"]), "");
    let ids = subjects_ids(&env, &ws);
    let range = format!("{}..{}", &ids[3][..8], &ids[1][..8]);
    assert_eq!(
        subjects(&env, &ws, &[&range]),
        "tweak a\nAdd B for the build\n"
    );
    assert_eq!(
        subjects(&env, &ws, &["HEAD~3..HEAD", "--author", "alice"]),
        "write notes\ntweak a\n"
    );
}

fn subjects_ids(env: &Env, ws: &Path) -> Vec<String> {
    env.ok(ws, "alice", &["log", "--format=%H"])
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn a_path_after_dashes_lists_the_commits_that_touched_it() {
    let (env, ws) = setup();
    assert_eq!(
        subjects(&env, &ws, &["--author", "alice", "--", "Source/a.txt"]),
        "tweak a\nseed a\n"
    );
    assert_eq!(
        subjects(&env, &ws, &["HEAD", "--", "Source"]),
        "tweak a\nAdd B for the build\nseed a\n"
    );
    assert_eq!(subjects(&env, &ws, &["HEAD~1", "--", "Docs"]), "");
    assert_eq!(
        subjects(&env, &ws, &["--author", "bob", "Source/b.txt"]),
        "Add B for the build\n"
    );
}

#[test]
fn a_format_with_a_path_lists_the_commits_that_touched_it() {
    let (env, ws) = setup();
    assert_eq!(subjects(&env, &ws, &["Docs/n.md"]), "write notes\n");
    assert_eq!(
        subjects(&env, &ws, &["--", "Source/b.txt"]),
        "Add B for the build\n"
    );
}

#[test]
fn a_path_in_a_subdirectory_is_resolved_from_the_workspace() {
    let (env, ws) = setup();
    assert_eq!(
        subjects(&env, &ws.join("Docs"), &["HEAD", "--", "n.md"]),
        "write notes\n"
    );
}

#[test]
fn oneline_uses_the_shared_renderer_with_filters() {
    let (env, ws) = setup();
    let id = &subjects_ids(&env, &ws)[2];
    assert_eq!(
        env.ok(&ws, "alice", &["log", "--oneline", "--author", "bob"]),
        format!("{} Add B for the build\n", &id[..7])
    );
}

#[test]
fn what_the_server_cannot_filter_is_a_usage_error() {
    let (env, ws) = setup();
    for (args, flag) in [
        (&["log", "--committer", "x"][..], "--committer"),
        (&["log", "--no-merges"], "--no-merges"),
        (&["log", "--skip", "1"], "--skip"),
        (&["log", "--invert-grep", "--grep", "x"], "--invert-grep"),
        (
            &["log", "--author", "a", "--author", "b"],
            "--author given more than once",
        ),
        (
            &["log", "--author", "^al"],
            "--author with a regular expression",
        ),
        (
            &["log", "--grep", "a.*b"],
            "--grep with a regular expression",
        ),
    ] {
        let (code, err) = refused(&env, &ws, args);
        assert_eq!(code, Some(129), "{args:?}: {err}");
        assert!(err.contains(&format!("`pyn log {flag}`")), "{err}");
    }
    let (code, err) = refused(&env, &ws, &["log", "HEAD...HEAD~1"]);
    assert_eq!(code, Some(128));
    assert!(err.contains("symmetric difference"), "{err}");
    let (code, err) = refused(&env, &ws, &["log", "--author", "bob", "--filter", "*.txt"]);
    assert_eq!(code, Some(129));
    assert!(err.contains("--filter"), "{err}");
    let (code, err) = refused(&env, &ws, &["log", "HEAD~99..HEAD"]);
    assert_eq!(code, Some(128));
    assert!(err.contains("before the first commit"), "{err}");
}

#[test]
fn plain_log_is_unchanged() {
    let (env, ws) = setup();
    let out = env.ok(&ws, "alice", &["log", "Source/a.txt"]);
    assert!(out.starts_with("REV"), "{out}");
}
