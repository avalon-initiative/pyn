//! Repositories from the command line: create, list, delete, clone and remembering which one a workspace is.

mod common;

use common::{cell_rows, start, start_empty};

#[test]
fn repositories_are_created_listed_and_deleted() {
    let env = start_empty();
    let home = env.dir("home");

    assert_eq!(
        env.ok(&home, "alice", &["repo", "list"]).trim(),
        "no repositories"
    );
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
        cell_rows(&listed),
        [
            ["VISIBILITY", "ROLE", "REPOSITORY"],
            ["private", "admin", "alice/game"],
            ["public", "admin", "alice/tools"]
        ]
    );
    assert_eq!(
        cell_rows(&env.ok(&home, "bob", &["repo", "list", "--owner", "bob"]))[1],
        ["private", "admin", "bob/notes"]
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
        cell_rows(&env.ok(&home, "alice", &["repo", "list", "--owner", "alice"]))[1],
        ["public", "admin", "alice/tools"]
    );
}

#[test]
fn a_workspace_remembers_its_repository() {
    let env = start_empty();
    let home = env.dir("home");
    for name in ["one", "two"] {
        env.ok(&home, "alice", &["repo", "create", name, "--no-policy"]);
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

#[test]
fn ls_and_summary_show_the_landing_page_data() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "int a;", None);
    let home = env.dir("home");
    env.ok(&home, "alice", &["checkout", "Content/m.umap"]);

    let root = env.ok(&home, "alice", &["ls"]);
    let rows = cell_rows(&root);
    assert_eq!(
        rows[0],
        ["MODE", "LOCKED BY", "NAME", "LAST CHANGE"],
        "{root}"
    );
    assert!(
        rows.iter()
            .any(|r| r == &["shared", "-", "Source/", "seed (alice, r1)"]),
        "{root}"
    );
    assert!(
        rows.iter()
            .any(|r| r[..3] == ["exclusive", "-", "Content/"]),
        "{root}"
    );
    let content = cell_rows(&env.ok(&home, "alice", &["ls", "Content"]));
    assert!(
        content
            .iter()
            .any(|r| r[..3] == ["exclusive", "alice", "m.umap"]),
        "{content:?}"
    );
    let missing = env.fails(&home, "alice", &["ls", "Nope"]);
    assert!(missing.contains("path_not_found"), "{missing}");

    let summary = env.ok(&home, "alice", &["summary"]);
    assert!(summary.contains("main (1 branch), 1 files"), "{summary}");
    let rows = cell_rows(&summary);
    assert!(
        rows.iter().any(|r| r == &["LOCKED BY", "PATH"]),
        "{summary}"
    );
    assert!(
        rows.iter().any(|r| r == &["alice", "Content/m.umap"]),
        "{summary}"
    );
    assert!(
        rows.iter()
            .any(|r| r == &["WHEN", "ACTOR", "ACTION", "PATH"]),
        "{summary}"
    );
    assert!(summary.contains("checkout"), "{summary}");
}

#[test]
fn a_new_repository_starts_with_a_policy_file_the_owner_can_replace() {
    let env = start_empty();
    let home = env.dir("home");

    let out = env.ok(&home, "alice", &["repo", "create", "plain"]);
    assert!(out.contains("added .pyn/pyn.toml"), "{out}");
    let files = env.ok(&home, "alice", &["--repo", "alice/plain", "files"]);
    assert!(
        cell_rows(&files)
            .iter()
            .any(|r| r.contains(&".pyn/pyn.toml".to_string())
                && r.contains(&"exclusive".to_string())),
        "{files}"
    );
    let shown = env.ok(
        &home,
        "alice",
        &["--repo", "alice/plain", "get", ".pyn/pyn.toml"],
    );
    assert!(shown.contains("default = \"exclusive\""), "{shown}");

    let ws = env.dir("ws-plain");
    env.ok(
        &home,
        "alice",
        &["clone", &env.source("alice/plain"), ws.to_str().unwrap()],
    );
    assert!(ws.join(".pyn/pyn.toml").is_file());

    let custom = env.dir("custom");
    std::fs::write(
        custom.join("p.toml"),
        "[meta]\ndefault = \"shared\"\n[shared]\npaths = [\".pyn/pyn.toml\"]\n",
    )
    .unwrap();
    env.ok(
        &custom,
        "alice",
        &["repo", "create", "open", "--policy", "p.toml"],
    );
    std::fs::write(custom.join("a.txt"), "a").unwrap();
    env.ok(
        &custom,
        "alice",
        &[
            "--repo",
            "alice/open",
            "checkin",
            "a.txt",
            "a.txt",
            "-m",
            "a",
        ],
    );

    let bad = custom.join("bad.toml");
    std::fs::write(&bad, "[exlusive]\n").unwrap();
    let err = env.fails(
        &custom,
        "alice",
        &[
            "repo",
            "create",
            "broken",
            "--policy",
            bad.to_str().unwrap(),
        ],
    );
    assert!(err.contains("invalid_rules"), "{err}");

    env.ok(&home, "alice", &["repo", "create", "bare", "--no-policy"]);
    let files = env.ok(&home, "alice", &["--repo", "alice/bare", "files"]);
    assert!(!files.contains("pyn.toml"), "{files}");

    let clash = env.fails(
        &home,
        "alice",
        &["repo", "create", "x", "--no-policy", "--policy", "p.toml"],
    );
    assert!(clash.contains("cannot be used with"), "{clash}");
}

#[test]
fn repo_create_takes_a_lock_limit_that_checkout_enforces() {
    let env = start_empty();
    let home = env.dir("home");
    env.ok(
        &home,
        "alice",
        &["repo", "create", "game", "--max-locks", "1", "--no-policy"],
    );
    let run = |tail: &[&'static str]| -> Vec<&'static str> {
        ["--repo", "alice/game"]
            .into_iter()
            .chain(tail.iter().copied())
            .collect()
    };
    env.ok(&home, "alice", &run(&["checkout", "Content/a.umap"]));
    let err = env.fails(&home, "alice", &run(&["checkout", "Content/b.umap"]));
    assert!(err.contains("lock_limit_reached"), "{err}");
    assert!(err.contains("1 lock"), "{err}");
    env.ok(&home, "alice", &run(&["release", "Content/a.umap"]));
    env.ok(&home, "alice", &run(&["checkout", "Content/b.umap"]));

    let bad = env.fails(
        &home,
        "alice",
        &["repo", "create", "other", "--max-locks", "0", "--no-policy"],
    );
    assert!(bad.contains("invalid_request"), "{bad}");
}
