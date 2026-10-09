//! Every listing command prints a header row and aligned columns with the longest-growing column last.

mod common;

use common::{cell_rows, start, start_empty};

#[test]
fn locks_files_and_history_have_stable_columns() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "a", None);
    let home = env.dir("home");
    env.ok(&home, "alice", &["lock", "Content/m.umap"]);

    let locks = cell_rows(&env.ok(&home, "alice", &["locks"]));
    assert_eq!(locks[0], ["OWNER", "EXPIRES", "PATH"]);
    assert_eq!(locks[1][0], "alice");
    assert_eq!(locks[1][2], "Content/m.umap");

    let files = cell_rows(&env.ok(&home, "alice", &["files"]));
    assert_eq!(files[0], ["MODE", "REV", "LOCKED BY", "PATH"]);
    assert!(
        files
            .iter()
            .any(|r| r == &["shared", "r1", "-", "Source/a.cpp"])
    );
    assert!(
        files
            .iter()
            .any(|r| r == &["exclusive", "-", "alice", "Content/m.umap"])
    );

    let history = cell_rows(&env.ok(&home, "alice", &["log", "Source/a.cpp"]));
    assert_eq!(history[0], ["REV", "AUTHOR", "WHEN", "MESSAGE"]);
    assert_eq!(history[1], ["r1", "alice", history[1][2].as_str(), "seed"]);
}

#[test]
fn locks_mine_lists_every_repository_with_the_path_last() {
    let env = start();
    let home = env.dir("home");
    env.ok(&home, "alice", &["repo", "create", "other", "--no-policy"]);
    env.ok(&home, "alice", &["lock", "Content/m.umap"]);
    env.ok(
        &home,
        "alice",
        &["--repo", "alice/other", "lock", "Content/o.umap"],
    );
    env.ok(
        &home,
        "bob",
        &["--repo", "alice/other", "lock", "Content/x.umap"],
    );

    let mine = cell_rows(&env.ok(&home, "alice", &["locks", "--mine"]));
    assert_eq!(mine[0], ["REPOSITORY", "ACQUIRED", "EXPIRES", "PATH"]);
    let tail: Vec<_> = mine[1..]
        .iter()
        .map(|r| (r[0].as_str(), r[r.len() - 1].as_str()))
        .collect();
    assert_eq!(
        tail,
        [
            ("alice/game", "Content/m.umap"),
            ("alice/other", "Content/o.umap")
        ]
    );

    env.ok(&home, "alice", &["unlock", "Content/m.umap"]);
    env.ok(
        &home,
        "alice",
        &["--repo", "alice/other", "unlock", "Content/o.umap"],
    );
    assert_eq!(
        env.ok(&home, "alice", &["locks", "--mine"]).trim(),
        "no locks"
    );
}

#[test]
fn repository_history_lists_newest_first_with_the_path_last() {
    let env = start();
    env.seed("alice", "Source/a.ts", "a", None);
    env.seed("alice", "Source/b.cpp", "b", None);
    env.seed("alice", "Source/a.ts", "a2", Some(1));
    let home = env.dir("home");

    let all = cell_rows(&env.ok(&home, "alice", &["log"]));
    assert_eq!(all[0], ["REV", "AUTHOR", "WHEN", "MESSAGE", "PATH"]);
    let tail: Vec<_> = all[1..]
        .iter()
        .map(|r| (r[0].as_str(), r[4].as_str()))
        .collect();
    assert_eq!(
        tail,
        [
            ("r2", "Source/a.ts"),
            ("r1", "Source/b.cpp"),
            ("r1", "Source/a.ts")
        ]
    );

    let ts = cell_rows(&env.ok(&home, "alice", &["log", "--filter", "*.ts", "--limit", "1"]));
    assert_eq!(ts.len(), 2, "{ts:?}");
    assert_eq!(
        (ts[1][0].as_str(), ts[1][4].as_str()),
        ("r2", "Source/a.ts")
    );
}

#[test]
fn empty_listings_say_so_instead_of_printing_a_bare_header() {
    let env = start();
    let home = env.dir("home");
    for (args, text) in [
        (vec!["locks"], "no locks"),
        (vec!["files"], "no files"),
        (vec!["log", "nope.txt"], "no history"),
        (vec!["token", "list"], "no tokens"),
        (vec!["invite", "list"], "no invitations"),
        (vec!["config", "list"], "no settings"),
    ] {
        assert_eq!(env.ok(&home, "alice", &args).trim(), text, "{args:?}");
    }
}

#[test]
fn administration_listings_have_stable_columns() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "a", None);
    let home = env.dir("home");

    env.ok(&home, "alice", &["member", "set", "bob", "writer"]);
    let members = cell_rows(&env.ok(&home, "alice", &["member", "list"]));
    assert_eq!(members[0], ["ROLE", "SOURCE", "USER"]);
    assert!(members.iter().any(|r| r == &["writer", "direct", "bob"]));

    let roles = cell_rows(&env.ok(&home, "alice", &["role", "list"]));
    assert_eq!(roles[0], ["ROLE", "PERMISSIONS"]);

    env.ok(&home, "alice", &["invite", "create", "--role", "writer"]);
    let invites = cell_rows(&env.ok(&home, "alice", &["invite", "list"]));
    assert_eq!(invites[0], ["ID", "ROLE", "EXPIRES", "STATE"]);
    assert_eq!(
        (&invites[1][1][..], &invites[1][3][..]),
        ("writer", "unused")
    );

    env.ok(
        &home,
        "alice",
        &["token", "create", "ci", "--permissions", "read"],
    );
    let tokens = cell_rows(&env.ok(&home, "alice", &["token", "list"]));
    assert_eq!(tokens[0], ["ID", "STATE", "EXPIRES", "NAME", "PERMISSIONS"]);
    assert_eq!(tokens[1][1], "active");
    assert_eq!(tokens[1][3..], ["ci", "read"]);

    let audit = cell_rows(&env.ok(&home, "alice", &["audit"]));
    assert_eq!(
        audit[0],
        ["ID", "WHEN", "ACTOR", "ACTION", "PATH", "DETAIL"]
    );
    assert!(audit.len() > 1);
}

#[test]
fn config_list_is_a_table() {
    let env = start_empty();
    let home = env.dir("home");
    env.ok(
        &home,
        "alice",
        &["config", "set", "--global", "user", "alice"],
    );
    let rows = cell_rows(&env.ok(&home, "alice", &["config", "list"]));
    assert_eq!(rows[0], ["KEY", "SCOPE", "VALUE"]);
    assert!(rows.iter().any(|r| r == &["user", "user", "alice"]));
}

fn expires(env: &common::Env, home: &std::path::Path, tz: &str) -> chrono::NaiveDateTime {
    let out = env.run_in_tz(home, "alice", &["locks"], tz);
    let out = String::from_utf8_lossy(&out.stdout).into_owned();
    let cell = cell_rows(&out)[1][1].clone();
    chrono::NaiveDateTime::parse_from_str(&cell, "%b %d %Y %H:%M")
        .unwrap_or_else(|e| panic!("{cell:?} is not `Mon DD YYYY HH:MM`: {e}"))
}

#[test]
fn times_print_in_the_timezone_of_the_environment() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "a", None);
    let home = env.dir("home");
    env.ok(&home, "alice", &["lock", "Content/m.umap"]);

    let utc = expires(&env, &home, "UTC");
    let tokyo = expires(&env, &home, "Asia/Tokyo");
    let new_york = expires(&env, &home, "America/New_York");
    assert_eq!((tokyo - utc).num_hours(), 9);
    assert!([-4, -5].contains(&(new_york - utc).num_hours()));
}

#[test]
fn lock_errors_show_the_local_time_not_raw_utc() {
    let env = start();
    let home = env.dir("home");
    env.ok(&home, "alice", &["lock", "Content/m.umap"]);
    let err = env.fails(&home, "bob", &["lock", "Content/m.umap"]);
    assert!(!err.contains("UTC"), "{err}");
    let until = err.split(" until ").nth(1).expect(&err);
    chrono::NaiveDateTime::parse_and_remainder(until, "%b %d %Y %H:%M").expect(&err);
}

#[test]
fn unlock_gives_up_your_lock_and_force_removes_another_users() {
    let env = start();
    let home = env.dir("home");
    env.ok(&home, "bob", &["lock", "Content/m.umap"]);
    let err = env.fails(&home, "alice", &["unlock", "--force", "Content/m.umap"]);
    assert!(err.contains("--reason"), "{err}");
    let err = env.fails(
        &home,
        "alice",
        &["unlock", "Content/m.umap", "--reason", "x"],
    );
    assert!(err.contains("--force"), "{err}");
    env.ok(
        &home,
        "alice",
        &["unlock", "--force", "--reason", "stuck", "Content/m.umap"],
    );
    env.ok(&home, "bob", &["lock", "Content/m.umap"]);
    env.ok(&home, "bob", &["unlock", "Content/m.umap"]);
    assert!(env.ok(&home, "bob", &["locks"]).contains("no locks"));
}

#[test]
fn history_is_an_alias_of_log() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "a", None);
    let home = env.dir("home");
    assert_eq!(
        env.ok(&home, "alice", &["history", "Source/a.cpp"]),
        env.ok(&home, "alice", &["log", "Source/a.cpp"])
    );
}
