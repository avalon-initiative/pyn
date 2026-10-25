//! `pyn status -s`, `--porcelain` and `-b` on a pyn server workspace: STAGED maps to the first column and the work
//! tree state to the second, with git's letters (docs/concepts/git-compatibility.md).

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{Duration, Utc};
use common::{Env, REPO, cell_rows, start, start_with_clock};
use pyn_core::ManualClock;

fn clone(env: &Env, extra: &[&str]) -> PathBuf {
    let ws = env.dir("ws");
    let source = env.source(REPO);
    let mut args = vec!["clone"];
    args.extend_from_slice(extra);
    args.extend_from_slice(&[&source, ws.to_str().unwrap()]);
    env.ok(&env.dir("home"), "alice", &args);
    ws.canonicalize().unwrap()
}

fn write(ws: &Path, path: &str, text: &str) {
    let file = ws.join(path);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(file, text).unwrap();
}

fn lines(out: &str) -> Vec<&str> {
    out.lines().collect()
}

fn seeded() -> (Env, PathBuf) {
    let env = start();
    for p in [
        "Source/a.cpp",
        "Source/b.cpp",
        "Source/c.cpp",
        "Source/d.cpp",
        "Source/gone.cpp",
        "Source/staged-gone.cpp",
        "Source/behind.cpp",
    ] {
        env.seed("alice", p, "one\n", None);
    }
    let ws = clone(&env, &[]);
    (env, ws)
}

#[test]
fn each_state_has_its_letters() {
    let (env, ws) = seeded();
    write(&ws, "Source/a.cpp", "edited\n");
    write(&ws, "Source/b.cpp", "staged\n");
    env.ok(&ws, "alice", &["add", "Source/b.cpp"]);
    write(&ws, "Source/c.cpp", "staged\n");
    env.ok(&ws, "alice", &["add", "Source/c.cpp"]);
    write(&ws, "Source/c.cpp", "and edited again\n");
    std::fs::remove_file(ws.join("Source/gone.cpp")).unwrap();
    std::fs::remove_file(ws.join("Source/staged-gone.cpp")).unwrap();
    env.ok(&ws, "alice", &["add", "Source/staged-gone.cpp"]);
    write(&ws, "Source/new.cpp", "new\n");
    env.ok(&ws, "alice", &["add", "Source/new.cpp"]);
    write(&ws, "notes.txt", "x\n");
    env.seed("bob", "Source/behind.cpp", "two\n", Some(1));
    env.seed("bob", "Source/fresh.cpp", "fresh\n", None);

    let expected = [
        "A  Source/new.cpp",
        " M Source/a.cpp",
        "M  Source/b.cpp",
        "MM Source/c.cpp",
        " D Source/gone.cpp",
        "D  Source/staged-gone.cpp",
        "?? notes.txt",
    ];
    for flags in [
        &["-s"][..],
        &["--short"],
        &["--porcelain"],
        &["--porcelain=v1"],
    ] {
        let mut args = vec!["status"];
        args.extend_from_slice(flags);
        let out = env.ok(&ws, "alice", &args);
        let mut got = lines(&out);
        let untracked = got.pop().unwrap();
        got.sort();
        let mut want = expected[..expected.len() - 1].to_vec();
        want.sort();
        assert_eq!(got, want, "{flags:?}: {out}");
        assert_eq!(untracked, "?? notes.txt", "untracked last: {out}");
    }
}

#[test]
fn lock_state_stays_out_of_short_and_porcelain_output() {
    let clock = Arc::new(ManualClock::new(Utc::now()));
    let env = start_with_clock(clock.clone());
    for path in [
        "Content/expired.umap",
        "Content/mine.umap",
        "Content/theirs.umap",
    ] {
        env.seed("alice", path, "one\n", None);
    }
    let ws = clone(&env, &[]);
    env.ok(&ws, "alice", &["lock", "Content/expired.umap"]);
    clock.advance(Duration::hours(9));
    env.ok(&ws, "alice", &["lock", "Content/mine.umap"]);
    env.ok(&ws, "bob", &["lock", "Content/theirs.umap"]);

    for flag in ["-s", "--porcelain"] {
        assert_eq!(env.ok(&ws, "alice", &["status", flag]), "", "{flag}");
    }
    for path in [
        "Content/expired.umap",
        "Content/mine.umap",
        "Content/theirs.umap",
    ] {
        write(&ws, path, "edited\n");
    }
    env.ok(&ws, "alice", &["add", "Content/mine.umap"]);
    let expected = " M Content/expired.umap\nM  Content/mine.umap\n M Content/theirs.umap\n";
    for flag in ["-s", "--porcelain"] {
        assert_eq!(env.ok(&ws, "alice", &["status", flag]), expected, "{flag}");
        assert_eq!(
            env.ok(&ws, "alice", &["status", flag, "-b"]),
            format!("## main...origin/main\n{expected}"),
            "{flag} -b"
        );
    }

    let out = env.ok(&ws, "alice", &["status"]);
    let mut rows = cell_rows(&out);
    // Lease countdowns use wall time rather than the server's manual clock.
    for row in &mut rows {
        if row.len() == 6 && row[0] == "exclusive" && row[4] != "-" {
            row[4] = row[4].split_once(" · ").unwrap().0.to_string();
        }
    }
    assert_eq!(
        rows,
        vec![
            vec!["On branch main"],
            vec!["MODE", "REV", "STAGED", "STATE", "LOCK", "PATH"],
            vec![
                "exclusive",
                "r1",
                "-",
                "modified",
                "-",
                "Content/expired.umap"
            ],
            vec![
                "exclusive",
                "r1",
                "modified",
                "clean",
                "you",
                "Content/mine.umap"
            ],
            vec![
                "exclusive",
                "r1",
                "-",
                "modified",
                "bob",
                "Content/theirs.umap"
            ],
            vec![
                "1 staged, 2 modified, 0 behind, 0 new, 0 untracked, 1 locked by you, 1 locked by others"
            ],
        ],
        "{out}"
    );
}

#[test]
fn a_clean_workspace_prints_nothing_and_the_branch_line_only_with_b() {
    let (env, ws) = seeded();
    assert_eq!(env.ok(&ws, "alice", &["status", "-s"]), "");
    assert_eq!(env.ok(&ws, "alice", &["status", "--porcelain"]), "");
    assert_eq!(
        env.ok(&ws, "alice", &["status", "-sb"]),
        "## main...origin/main\n"
    );
    assert_eq!(
        env.ok(&ws, "alice", &["status", "--porcelain", "--branch"]),
        "## main...origin/main\n"
    );
    env.ok(&ws, "alice", &["switch", "-c", "feature"]);
    assert_eq!(
        env.ok(&ws, "alice", &["status", "-s", "-b"]),
        "## feature...origin/feature\n"
    );
}

#[test]
fn local_commits_not_yet_published_count_as_ahead() {
    let (env, ws) = seeded();
    write(&ws, "Source/a.cpp", "local\n");
    env.ok(&ws, "alice", &["add", "Source/a.cpp"]);
    env.ok(&ws, "alice", &["commit", "-m", "local"]);
    assert_eq!(
        env.ok(&ws, "alice", &["status", "-sb"]),
        "## main...origin/main [ahead 1]\n"
    );
}

#[test]
fn a_workspace_without_a_local_store_names_just_the_branch() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "one\n", None);
    let ws = clone(&env, &["--no-local-store"]);
    assert_eq!(env.ok(&ws, "alice", &["status", "-sb"]), "## main\n");
}

#[test]
fn pathspecs_limit_the_listing_and_are_taken_from_where_you_stand() {
    let (env, ws) = seeded();
    write(&ws, "Source/a.cpp", "edited\n");
    write(&ws, "Source/b.cpp", "edited\n");
    write(&ws, "notes.txt", "x\n");

    let out = env.ok(&ws, "alice", &["status", "-s", "Source/a.cpp"]);
    assert_eq!(out, " M Source/a.cpp\n");
    let out = env.ok(&ws, "alice", &["status", "--porcelain", "Source"]);
    assert_eq!(lines(&out), [" M Source/a.cpp", " M Source/b.cpp"]);
    assert_eq!(env.ok(&ws, "alice", &["status", "-s", "Sour"]), "");
    assert_eq!(env.ok(&ws, "alice", &["status", "-s", "nothing"]), "");

    let sub = ws.join("Source");
    let out = env.ok(&sub, "alice", &["status", "-s"]);
    assert_eq!(lines(&out), [" M a.cpp", " M b.cpp", "?? ../notes.txt"]);
    let out = env.ok(
        &sub,
        "alice",
        &["status", "--porcelain", "a.cpp", "../notes.txt"],
    );
    assert_eq!(lines(&out), [" M Source/a.cpp", "?? notes.txt"]);

    let table = env.ok(&ws, "alice", &["status", "Source/b.cpp"]);
    assert!(
        table.contains("Source/b.cpp") && !table.contains("a.cpp"),
        "{table}"
    );
}

#[test]
fn a_path_with_a_space_is_quoted_as_git_does() {
    let (env, ws) = seeded();
    write(&ws, "with space.txt", "x\n");
    assert_eq!(
        env.ok(&ws, "alice", &["status", "-s"]),
        "?? \"with space.txt\"\n"
    );
}

#[test]
fn unsupported_variants_are_usage_errors() {
    let (env, ws) = seeded();
    for args in [&["-z"][..], &["--porcelain=v2"]] {
        let mut full = vec!["status"];
        full.extend_from_slice(args);
        let out = env.run(&ws, "alice", &full);
        assert_eq!(out.status.code(), Some(129), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}");
    }
}

#[test]
fn glob_and_magic_pathspecs_limit_the_listing() {
    let (env, ws) = seeded();
    write(&ws, "Source/a.cpp", "edited\n");
    write(&ws, "Source/b.cpp", "edited\n");
    write(&ws, "notes.txt", "x\n");
    write(&ws, "Other/n.txt", "x\n");
    for (args, want) in [
        (
            &["-s", "Source/*.cpp"][..],
            " M Source/a.cpp\n M Source/b.cpp\n",
        ),
        (
            &["-s", "Source/?.cpp"],
            " M Source/a.cpp\n M Source/b.cpp\n",
        ),
        (&["-s", ":(glob)*.txt"], "?? notes.txt\n"),
        (&["-s", "**/*.txt"], "?? Other/n.txt\n"),
        (&["-s", ":(glob)**/*.txt"], "?? Other/n.txt\n?? notes.txt\n"),
        (&["-s", ":!Source", ":!Other"], "?? notes.txt\n"),
        (&["-s", ":(icase)SOURCE/A.CPP"], " M Source/a.cpp\n"),
    ] {
        let mut full = vec!["status"];
        full.extend_from_slice(args);
        let got = env.ok(&ws, "alice", &full);
        assert_eq!(got.replace("\r", ""), want, "{args:?}");
    }
}

#[test]
fn an_unmerged_path_is_uu_and_a_both_added_path_is_aa() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "1\n2\n3\n", None);
    let ws = clone(&env, &[]);
    env.ok(&ws, "alice", &["switch", "-c", "feature"]);
    write(&ws, "Source/a.cpp", "1\ntheirs\n3\n");
    write(&ws, "Source/both.cpp", "theirs\n");
    env.ok(
        &ws,
        "alice",
        &["commit", "Source/a.cpp", "Source/both.cpp", "-m", "theirs"],
    );
    env.ok(&ws, "alice", &["switch", "main"]);
    write(&ws, "Source/a.cpp", "1\nours\n3\n");
    write(&ws, "Source/both.cpp", "ours\n");
    env.ok(
        &ws,
        "alice",
        &["commit", "Source/a.cpp", "Source/both.cpp", "-m", "ours"],
    );
    let merged = env.run(&ws, "alice", &["merge", "feature"]);
    assert_eq!(merged.status.code(), Some(1));

    let out = env.ok(&ws, "alice", &["status", "--porcelain"]);
    assert_eq!(
        lines(&out),
        ["UU Source/a.cpp", "AA Source/both.cpp"],
        "{out}"
    );
    write(&ws, "Source/a.cpp", "resolved\n");
    env.ok(&ws, "alice", &["add", "Source/a.cpp"]);
    let out = env.ok(&ws, "alice", &["status", "--porcelain"]);
    assert_eq!(
        lines(&out),
        ["M  Source/a.cpp", "AA Source/both.cpp"],
        "{out}"
    );
}
