//! Watching a repository from the command line: set a level, list the watched repositories, stop.

mod common;

use common::{cell_rows, start_empty};

#[test]
fn a_repository_is_watched_at_a_level_listed_and_unwatched() {
    let env = start_empty();
    let home = env.dir("home");
    env.ok(
        &home,
        "alice",
        &["repo", "create", "alice/tools", "--visibility", "public"],
    );
    assert_eq!(
        env.ok(&home, "bob", &["watching"]).trim(),
        "not watching any repository"
    );

    let out = env.ok(&home, "bob", &["watch", "alice/tools", "--level", "locks"]);
    assert!(
        out.contains("watching alice/tools: locks (2 watchers)"),
        "{out}"
    );
    let out = env.ok(&home, "bob", &["watch", "alice/tools"]);
    assert!(out.contains("watching alice/tools: all"), "{out}");
    let missing = env.fails(&home, "bob", &["watch", "alice/nothing"]);
    assert!(missing.contains("repo_not_found"), "{missing}");
    let bad = env.fails(&home, "bob", &["watch", "alice/tools", "--level", "never"]);
    assert!(!bad.is_empty());

    let listed = env.ok(&home, "bob", &["watching"]);
    let rows = cell_rows(&listed);
    assert_eq!(rows[0], ["LEVEL", "SINCE", "WATCHERS", "REPOSITORY"]);
    assert_eq!(rows[1][0], "all");
    assert_eq!(rows[1][2..], ["2", "alice/tools"]);

    let out = env.ok(&home, "bob", &["unwatch", "alice/tools"]);
    assert!(
        out.contains("no longer watching alice/tools (1 watchers)"),
        "{out}"
    );
    assert_eq!(
        env.ok(&home, "bob", &["watching"]).trim(),
        "not watching any repository"
    );

    env.ok(&home, "bob", &["watch", "alice/tools", "--level", "ignore"]);
    let listed = env.ok(&home, "bob", &["watching"]);
    assert_eq!(cell_rows(&listed)[1][0], "ignore");
}
