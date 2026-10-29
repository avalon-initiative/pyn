//! `pyn log` and the one-line views on a pyn server home split a message into a subject and a body (decision #809),
//! with and without the local store.

mod common;

use std::fs;
use std::path::PathBuf;

use common::{Env, REPO, start};

const MULTI: &str = "first line\nsecond line\n\nbody one\nbody two\n\nlast paragraph";

fn clone_with(env: &Env, extra: &[&str]) -> PathBuf {
    let ws = env.dir("ws");
    let source = env.source(REPO);
    let mut args = vec!["clone"];
    args.extend(extra);
    args.extend([source.as_str(), ws.to_str().unwrap()]);
    env.ok(&env.dir("home"), "alice", &args);
    ws.canonicalize().unwrap()
}

/// A workspace whose `Source/a.txt` has one revision per message, in order.
fn with_messages(extra: &[&str], messages: &[&str]) -> (Env, PathBuf) {
    let env = start();
    env.seed("alice", "Source/a.txt", "0\n", None);
    let ws = clone_with(&env, extra);
    for (i, m) in messages.iter().enumerate() {
        fs::write(ws.join("Source/a.txt"), format!("{}\n", i + 1)).unwrap();
        if extra.contains(&"--local-store") {
            env.ok(&ws, "alice", &["add", "Source/a.txt"]);
            env.ok(&ws, "alice", &["commit", "-m", m]);
        } else {
            env.ok(&ws, "alice", &["commit", "Source/a.txt", "-m", m]);
        }
    }
    (env, ws)
}

fn modes() -> [&'static [&'static str]; 2] {
    [&["--no-local-store"], &["--local-store"]]
}

#[test]
fn the_path_view_shows_the_subject_with_the_body_indented_below() {
    for extra in modes() {
        let (env, ws) = with_messages(extra, &[MULTI]);
        let out = env.ok(&ws, "alice", &["log", "Source/a.txt"]);
        let lines: Vec<&str> = out.lines().collect();
        let at = lines
            .iter()
            .position(|l| l.ends_with("first line second line"))
            .unwrap_or_else(|| panic!("{extra:?}: {out}"));
        assert_eq!(
            lines[at + 1..at + 5],
            ["    body one", "    body two", "", "    last paragraph"],
            "{extra:?}: {out}"
        );
    }
}

#[test]
fn the_repository_view_has_a_subject_cell_and_the_path_last() {
    for extra in modes() {
        let (env, ws) = with_messages(extra, &[MULTI]);
        let out = env.ok(&ws, "alice", &["log"]);
        let row = out
            .lines()
            .find(|l| l.contains("first line second line"))
            .unwrap_or_else(|| panic!("{extra:?}: {out}"));
        assert!(row.ends_with("Source/a.txt"), "{extra:?}: {row}");
        assert!(out.contains("\n    body one\n    body two\n"), "{out}");
    }
}

#[test]
fn a_message_without_a_body_prints_no_indented_lines() {
    for extra in modes() {
        let (env, ws) = with_messages(extra, &["just a subject\n"]);
        let out = env.ok(&ws, "alice", &["log", "Source/a.txt"]);
        assert!(out.contains("just a subject"), "{out}");
        assert!(out.lines().all(|l| !l.starts_with("    ")), "{out}");
    }
}

#[test]
fn leading_blank_lines_do_not_make_an_empty_subject() {
    for extra in modes() {
        let (env, ws) = with_messages(extra, &["\n\nonly a body"]);
        let out = env.ok(&ws, "alice", &["log", "Source/a.txt"]);
        assert!(out.contains("only a body"), "{extra:?}: {out}");
        assert!(
            out.lines().all(|l| !l.starts_with("    ")),
            "{extra:?}: {out}"
        );
    }
}

#[test]
fn commits_rows_hold_one_subject_each() {
    for extra in modes() {
        let (env, ws) = with_messages(extra, &[MULTI]);
        let out = env.ok(&ws, "alice", &["log", "--commits"]);
        assert!(
            out.lines().any(|l| l.ends_with("first line second line")),
            "{extra:?}: {out}"
        );
        assert!(!out.contains("body one"), "{extra:?}: {out}");
    }
}

#[test]
fn oneline_prints_the_abbreviated_id_and_the_subject_only() {
    for extra in modes() {
        let (env, ws) = with_messages(extra, &[MULTI, "plain\n"]);
        let out = env.ok(&ws, "alice", &["log", "--oneline"]);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 3, "{extra:?}: {out}");
        assert!(lines[0].ends_with(" plain"), "{out}");
        assert!(lines[1].ends_with(" first line second line"), "{out}");
        assert!(lines[2].ends_with(" seed"), "{out}");
        assert!(
            lines
                .iter()
                .all(|l| l.split(' ').next().unwrap().len() == 7)
        );
    }
}

#[test]
fn restore_and_ls_print_the_subject_only() {
    let env = start();
    env.seed("alice", "Content/m.umap", "0", None);
    let ws = clone_with(&env, &["--no-local-store"]);
    env.ok(&ws, "alice", &["lock", "Content/m.umap"]);
    fs::write(ws.join("Content/m.umap"), "1").unwrap();
    env.ok(&ws, "alice", &["commit", "Content/m.umap", "-m", MULTI]);
    env.ok(&ws, "alice", &["lock", "Content/m.umap"]);
    let ls = env.ok(&ws, "alice", &["ls", "Content"]);
    assert!(ls.contains("first line second line (alice, r2)"), "{ls}");
    let out = env.ok(&ws, "alice", &["restore", "Content/m.umap", "1", "--yes"]);
    assert!(
        out.contains("The current head r2 (alice, \"first line second line\")"),
        "{out}"
    );
    assert!(!out.contains("body one"), "{out}");
}
