//! Repositories from the command line: create, list, delete, clone and remembering which one a workspace is.

mod common;

use common::{start, start_empty};

#[test]
fn repositories_are_created_listed_and_deleted() {
    let env = start_empty();
    let home = env.dir("home");

    assert!(env.ok(&home, "alice", &["repo", "list"]).trim().is_empty());
    let out = env.ok(&home, "alice", &["repo", "create", "game"]);
    assert!(out.contains("created alice/game"), "{out}");
    env.ok(
        &home,
        "alice",
        &[
            "repo",
            "create",
            "alice/tools",
            "--visibility",
            "public",
            "--lease-hours",
            "2",
        ],
    );
    env.ok(&home, "bob", &["repo", "create", "bob/notes"]);

    let listed = env.ok(&home, "alice", &["repo", "list", "--owner", "alice"]);
    assert_eq!(
        listed.lines().collect::<Vec<_>>(),
        ["alice/game\tprivate\tadmin", "alice/tools\tpublic\tadmin"]
    );
    assert_eq!(
        env.ok(&home, "bob", &["repo", "list", "--owner", "bob"])
            .trim(),
        "bob/notes\tprivate\tadmin"
    );

    let taken = env.fails(&home, "alice", &["repo", "create", "game"]);
    assert!(taken.contains("repo_exists"), "{taken}");
    let elsewhere = env.fails(&home, "alice", &["repo", "create", "bob/game"]);
    assert!(elsewhere.contains("not_namespace_owner"), "{elsewhere}");
    let bad = env.fails(&home, "alice", &["repo", "create", "Not Ok"]);
    assert!(bad.contains("invalid_repo_name"), "{bad}");
    let vis = env.fails(
        &home,
        "alice",
        &["repo", "create", "x", "--visibility", "odd"],
    );
    assert!(vis.contains("unknown visibility"), "{vis}");

    let foreign = env.fails(&home, "bob", &["repo", "delete", "alice/game", "--yes"]);
    assert!(foreign.contains("not_namespace_owner"), "{foreign}");
    let out = env.ok(&home, "alice", &["repo", "delete", "alice/game", "--yes"]);
    assert!(out.contains("deleted alice/game"), "{out}");
    assert_eq!(
        env.ok(&home, "alice", &["repo", "list", "--owner", "alice"])
            .trim(),
        "alice/tools\tpublic\tadmin"
    );
}

#[test]
fn a_workspace_remembers_its_repository() {
    let env = start_empty();
    let home = env.dir("home");
    for name in ["one", "two"] {
        env.ok(&home, "alice", &["repo", "create", name]);
        let seed = env.dir(&format!("seed-{name}"));
        std::fs::write(seed.join("f"), format!("from {name}")).unwrap();
        env.ok(
            &seed,
            "alice",
            &[
                "--repo",
                &format!("alice/{name}"),
                "checkin",
                &format!("{name}.txt"),
                "f",
                "-m",
                "seed",
            ],
        );
    }

    let ws = env.dir("ws-one");
    let out = env.ok(
        &home,
        "alice",
        &["clone", &env.source("alice/one"), ws.to_str().unwrap()],
    );
    assert!(out.contains("cloned 1 files from alice/one"), "{out}");
    let ws = ws.canonicalize().unwrap();
    let config = std::fs::read_to_string(ws.join(".pyn/local_only/config.toml")).unwrap();
    assert!(config.contains("repo = \"alice/one\""), "{config}");
    assert!(ws.join("one.txt").is_file() && !ws.join("two.txt").exists());

    let files = env.ok(&ws, "alice", &["files"]);
    assert!(
        files.contains("one.txt") && !files.contains("two.txt"),
        "{files}"
    );
    assert!(
        env.ok(&ws, "alice", &["status"])
            .contains("everything is up to date")
    );
    let who = env.ok(&ws, "alice", &["whoami"]);
    assert!(
        who.starts_with("alice\t") && who.contains("checkin"),
        "{who}"
    );

    let flagged = env.ok(&ws, "alice", &["--repo", "alice/two", "files"]);
    assert!(
        flagged.contains("two.txt") && !flagged.contains("one.txt"),
        "{flagged}"
    );

    let outside = env.fails(&home, "alice", &["files"]);
    assert!(outside.contains("no repository"), "{outside}");
    assert!(env.ok(&home, "alice", &["whoami"]).trim() == "alice");
}

#[test]
fn clone_takes_a_server_address_or_a_bare_name_and_names_the_folder_after_the_repo() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "int a;", None);
    let home = env.dir("home");

    let out = env.ok(&home, "alice", &["clone", &env.source("alice/game")]);
    assert!(out.contains("cloned 1 files from alice/game"), "{out}");
    assert!(home.join("game/Source/a.cpp").is_file());

    let out = env.ok(&home, "alice", &["clone", "alice/game", "second"]);
    assert!(out.contains("from alice/game"), "{out}");
    assert!(home.join("second/Source/a.cpp").is_file());

    let missing = env.fails(
        &home,
        "alice",
        &["clone", &env.source("alice/nope"), "third"],
    );
    assert!(missing.contains("repo_not_found"), "{missing}");
    let malformed = env.fails(&home, "alice", &["clone", "just-a-name", "fourth"]);
    assert!(malformed.contains("owner/name"), "{malformed}");
}

#[test]
fn tokens_are_made_for_the_current_or_named_repositories() {
    let env = start();
    let home = env.dir("home");
    let token = env.ok(
        &home,
        "alice",
        &["token", "create", "ci", "--permissions", "read"],
    );
    assert!(token.trim().starts_with("pyn_"), "{token}");

    let listed = env.ok(&home, "alice", &["token", "list"]);
    assert!(listed.contains("ci"), "{listed}");

    let bare = start_empty();
    let none = bare.fails(
        &bare.dir("home"),
        "alice",
        &["token", "create", "ci", "--permissions", "read"],
    );
    assert!(none.contains("--repos"), "{none}");
}
