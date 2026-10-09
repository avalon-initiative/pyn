//! The `pyn status` table: stable columns, a summary line and the wording for lock-only paths.

mod common;

use common::{REPO, start};

fn cells(line: &str) -> Vec<&str> {
    line.split("  ")
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .collect()
}

#[test]
fn status_prints_an_aligned_table_with_a_summary() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "a one", None);
    env.seed("alice", "Source/behind.cpp", "one", None);
    env.seed("alice", "Content/m.umap", "map one", None);
    let ws = env.dir("ws");
    env.ok(
        &env.dir("home"),
        "alice",
        &["clone", &env.source(REPO), ws.to_str().unwrap()],
    );
    let ws = ws.canonicalize().unwrap();

    std::fs::write(ws.join("Source/a.cpp"), "edited").unwrap();
    std::fs::write(ws.join("notes.txt"), "x").unwrap();
    env.ok(&ws, "alice", &["checkout", "Content/m.umap"]);
    env.seed("bob", "Source/behind.cpp", "two", Some(1));
    env.seed("bob", "Source/fresh.cpp", "fresh", None);

    let out = env.ok(&ws, "alice", &["status"]);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        cells(lines[0]),
        ["PATH", "MODE", "REV", "STATE", "LOCK"],
        "{out}"
    );
    let col = lines[0].find("MODE").unwrap();
    for line in &lines[1..lines.len() - 1] {
        assert!(
            !line.is_empty() && line[..col].ends_with("  "),
            "misaligned: {out}"
        );
    }
    let row = |p: &str| {
        lines
            .iter()
            .find(|l| l.starts_with(p))
            .unwrap_or_else(|| panic!("no row for {p}: {out}"))
    };
    assert_eq!(
        &cells(row("Source/a.cpp"))[1..4],
        ["shared", "r1", "modified"]
    );
    assert_eq!(&cells(row("Source/behind.cpp"))[3..4], ["behind (head r2)"]);
    assert_eq!(
        &cells(row("Source/fresh.cpp"))[1..4],
        ["shared", "-", "new"]
    );
    assert_eq!(&cells(row("notes.txt"))[1..4], ["-", "-", "untracked"]);
    let m = cells(row("Content/m.umap"));
    assert_eq!(&m[1..4], ["exclusive", "r1", "clean"]);
    assert!(
        m[4].starts_with("you \u{b7} ") && m[4].ends_with('m'),
        "{m:?}"
    );

    let summary = lines.last().unwrap();
    assert_eq!(
        *summary, "1 modified, 1 behind, 1 new, 1 untracked, 1 locked by you, 0 locked by others",
        "{out}"
    );
}

#[test]
fn a_clean_workspace_says_so() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "a one", None);
    let ws = env.dir("ws");
    env.ok(
        &env.dir("home"),
        "alice",
        &["clone", &env.source(REPO), ws.to_str().unwrap()],
    );
    let out = env.ok(&ws.canonicalize().unwrap(), "alice", &["status"]);
    assert_eq!(out.trim(), "everything is up to date");
}

#[test]
fn a_locked_path_without_a_revision_is_not_reported_as_new() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "a one", None);
    let home = env.dir("home");
    env.ok(&home, "alice", &["checkout", "Content/boss.uasset"]);
    env.ok(&home, "bob", &["checkout", "Content/map.umap"]);
    let ws = env.dir("ws");
    let out = env.ok(
        &home,
        "alice",
        &["clone", &env.source(REPO), ws.to_str().unwrap()],
    );
    assert!(out.contains("cloned 1 files"), "{out}");
    let ws = ws.canonicalize().unwrap();

    let out = env.ok(&ws, "alice", &["status"]);
    assert!(
        !out.contains("on the server") && !out.contains("pyn update"),
        "{out}"
    );
    let boss = out
        .lines()
        .find(|l| l.starts_with("Content/boss.uasset"))
        .expect(&out);
    assert_eq!(
        &cells(boss)[1..4],
        ["exclusive", "-", "locked, not checked in yet"],
        "{out}"
    );
    assert!(cells(boss)[4].starts_with("you \u{b7} "), "{boss}");
    let map = out
        .lines()
        .find(|l| l.starts_with("Content/map.umap"))
        .expect(&out);
    assert!(cells(map)[4].starts_with("bob \u{b7} "), "{map}");
    assert!(out.contains("1 locked by you, 1 locked by others"), "{out}");
    assert!(
        env.ok(&ws, "alice", &["update"])
            .contains("updated 0 file(s)")
    );
}
