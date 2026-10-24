//! Git alignment (decision #218): aliases, argument shapes, exit codes and the git-only command messages.

mod common;

use common::start;

#[test]
fn log_of_a_path_is_newest_first_and_takes_max_count() {
    let env = start();
    env.seed("alice", "Source/a.ts", "one", None);
    env.seed("alice", "Source/a.ts", "two", Some(1));
    env.seed("alice", "Source/a.ts", "three", Some(2));
    let home = env.dir("home");

    let out = env.ok(&home, "alice", &["log", "Source/a.ts"]);
    let revs: Vec<&str> = out
        .lines()
        .skip(1)
        .map(|l| l.split_whitespace().next().unwrap())
        .collect();
    assert_eq!(revs, ["r3", "r2", "r1"], "{out}");

    for flag in ["-n", "--max-count", "--limit"] {
        let out = env.ok(&home, "alice", &["log", flag, "2", "Source/a.ts"]);
        assert_eq!(out.lines().count(), 3, "{flag}: {out}");
        assert!(out.contains("r3") && !out.contains("r1"), "{flag}: {out}");
    }
}

#[test]
fn git_log_options_are_usage_errors_on_a_pyn_server() {
    let env = start();
    env.seed("alice", "Source/a.ts", "one", None);
    let home = env.dir("home");
    for args in [
        &["log", "--reverse"][..],
        &["log", "--committer=x"],
        &["log", "--skip=1"],
        &["log", "Source/a.ts", "Source/b.ts"],
        &["log", "--", "Source/a.ts", "Source/b.ts"],
    ] {
        let out = env.run(&home, "alice", args);
        assert_eq!(out.status.code(), Some(129), "{args:?}");
    }
    let out = env.ok(&home, "alice", &["log", "--", "Source/a.ts"]);
    assert!(out.contains("r1"), "{out}");
}

#[test]
fn commit_checks_several_paths_in_as_one_commit() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "one", None);
    let ws = env.dir("ws");
    let home = env.dir("home");
    env.ok(
        &home,
        "alice",
        &["clone", &env.source(common::REPO), ws.to_str().unwrap()],
    );
    let ws = ws.canonicalize().unwrap();
    std::fs::write(ws.join("Source/a.cpp"), "two").unwrap();
    std::fs::write(ws.join("Source/b.cpp"), "b").unwrap();
    env.ok(&ws, "alice", &["commit", "Source/a.cpp", "-m", "edit"]);
    let out = env.ok(&ws, "alice", &["log", "Source/a.cpp"]);
    assert!(out.contains("edit"), "{out}");
}

#[test]
fn restore_takes_the_revision_as_source() {
    let env = start();
    env.seed("alice", "Content/m.umap", "one", None);
    env.seed("alice", "Content/m.umap", "two", Some(1));
    let home = env.dir("home");
    env.ok(&home, "alice", &["lock", "Content/m.umap", "--base", "2"]);
    let out = env.ok(
        &home,
        "alice",
        &["restore", "--source", "1", "--yes", "Content/m.umap"],
    );
    assert!(out.contains("restored from r1"), "{out}");

    let err = env.fails(&home, "alice", &["restore", "Content/m.umap"]);
    assert!(
        err.contains("<REVISION>") || err.contains("--source"),
        "{err}"
    );
}

#[test]
fn git_only_commands_say_so_and_unknown_ones_are_not_commands() {
    let env = start();
    let home = env.dir("home");
    let run = |args: &[&str]| {
        let out = env.run(&home, "alice", args);
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };
    let (code, err) = run(&["rebase", "x"]);
    assert_eq!(code, Some(1));
    assert!(
        err.contains("'rebase' is a git command pyn does not support yet"),
        "{err}"
    );

    let (code, err) = run(&["grep", "x"]);
    assert_eq!(code, Some(1));
    assert!(err.contains("'grep' searches a git repository"), "{err}");

    for name in ["blame", "annotate"] {
        let (code, err) = run(&[name, "x"]);
        assert_eq!(code, Some(1));
        assert!(
            err.contains(&format!("'{name}' reads a git repository")),
            "{err}"
        );
    }

    let (code, err) = run(&["checkout", "Source/a.cpp"]);
    assert_eq!(code, Some(128));
    assert!(err.contains("pyn lock"), "{err}");

    let (code, err) = run(&["frobnicate"]);
    assert_eq!(code, Some(1));
    assert!(err.contains("'frobnicate' is not a pyn command"), "{err}");
}

#[test]
fn failures_exit_128_and_usage_errors_129() {
    let env = start();
    let home = env.dir("home");
    let out = env.run(&home, "alice", &["get", "nope.txt"]);
    assert_eq!(out.status.code(), Some(128));
    assert!(String::from_utf8_lossy(&out.stderr).starts_with("fatal: "));

    let out = env.run(&home, "alice", &["log", "--no-such-flag"]);
    assert_eq!(out.status.code(), Some(129));

    let out = env.run(&home, "alice", &["--version"]);
    assert_eq!(out.status.code(), Some(0));
}
