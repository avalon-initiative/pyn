//! How a conflicted text merge is written under each `merge.conflictStyle`, compared with real git on identical
//! repositories: the default style splits a conflict where the sides agree for more than three lines and joins conflicts
//! at most three lines apart; `diff3` and `zdiff3` only differ in how the edges are trimmed. Real git is the oracle.

#[path = "../../pyn-gittransport/tests/common/gitsrv.rs"]
mod gitsrv;

use std::fs;
use std::path::Path;
use std::process::Command;

use gitsrv::*;
use tempfile::tempdir;

fn lines(prefix: &str, n: usize) -> String {
    (1..=n).map(|i| format!("{prefix}{i}\n")).collect()
}

fn setup(repo: &Path, format: &str, style: &str, versions: [&str; 3]) {
    init(repo, format, false);
    run(repo, &["config", "merge.conflictStyle", style]);
    commit(repo, "f", versions[0].as_bytes(), "base");
    run(repo, &["checkout", "-q", "-b", "side"]);
    commit(repo, "f", versions[2].as_bytes(), "theirs");
    run(repo, &["checkout", "-q", "main"]);
    commit(repo, "f", versions[1].as_bytes(), "ours");
}

fn merged(repo: &Path, pyn: bool) -> (bool, String) {
    let code = if pyn {
        Command::new(env!("CARGO_BIN_EXE_pyn"))
            .current_dir(repo)
            .args(["merge", "side"])
            .env_clear()
            .env("PATH", path_env())
            .env("HOME", repo.join(".git"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .output()
            .unwrap()
            .status
    } else {
        git(repo)
            .args(["merge", "-q", "--no-edit", "side"])
            .output()
            .unwrap()
            .status
    };
    (
        code.success(),
        String::from_utf8_lossy(&fs::read(repo.join("f")).unwrap()).into_owned(),
    )
}

fn check(versions: [&str; 3], what: &str) {
    for format in FORMATS {
        for style in ["merge", "diff3", "zdiff3"] {
            let tmp = tempdir().unwrap();
            let root = tmp.path().canonicalize().unwrap();
            let (oracle, ours) = (root.join("oracle"), root.join("ours"));
            setup(&oracle, format, style, versions);
            setup(&ours, format, style, versions);
            let (want, got) = (merged(&oracle, false), merged(&ours, true));
            assert!(!want.0, "{what}: the merge must conflict");
            assert_eq!(got.1, want.1, "{what} ({style}, {format})");
            assert_eq!(got.0, want.0, "{what} ({style}, {format}): status");
        }
    }
}

fn spread(middle: usize) -> [String; 3] {
    let m = lines("m", middle);
    [
        "a\nB1\nB2\nB3\nz\n".to_string(),
        format!("a\nP\n{m}Q\nz\n"),
        format!("a\nR\n{m}S\nz\n"),
    ]
}

#[test]
fn a_conflict_whose_sides_agree_for_more_than_three_lines_splits() {
    let v = spread(4);
    check([&v[0], &v[1], &v[2]], "middle of four");
}

#[test]
fn a_conflict_whose_sides_agree_for_three_lines_stays_whole() {
    let v = spread(3);
    check([&v[0], &v[1], &v[2]], "middle of three");
}

#[test]
fn conflicts_a_line_apart_join_in_the_default_style() {
    check(
        [
            "a\nb\nc\nd\ne\nf\ng\n",
            "a\nb\nP\nd\nQ\nf\ng\n",
            "a\nb\nX\nd\nY\nf\ng\n",
        ],
        "one line apart",
    );
}

#[test]
fn conflicts_four_lines_apart_stay_apart() {
    let base = format!("a\nc\n{}e\nz\n", lines("m", 4));
    let ours = format!("a\nP\n{}Q\nz\n", lines("m", 4));
    let theirs = format!("a\nR\n{}S\nz\n", lines("m", 4));
    check([&base, &ours, &theirs], "four lines apart");
}

#[test]
fn a_change_only_one_side_made_between_conflicts_keeps_each_sides_text() {
    let base = "a\nc\nm1\nm2\ne\nz\n";
    let ours = "a\nP\nm1\nm2\nQ\nz\n";
    let theirs = "a\nR\nm1\nT\nS\nz\n";
    check([base, ours, theirs], "one-sided change in the gap");
}

#[test]
fn sides_of_different_lengths_split_around_the_shared_lines() {
    let m = lines("m", 5);
    let base = "a\nB1\nB2\nz\n".to_string();
    let ours = format!("a\nP1\nP2\n{m}Q\nz\n");
    let theirs = format!("a\nR\n{m}z2\nz\n");
    check([&base, &ours, &theirs], "different lengths");
}
