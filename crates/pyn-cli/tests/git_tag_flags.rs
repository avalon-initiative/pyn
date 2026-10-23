//! `pyn tag` flag mixes on a git repository: create-only flags while listing or deleting, and `-l` with `-d`, are a
//! usage error (129, nothing on stdout) as in git; the valid forms keep working.

mod common;

use common::gitfix::Repo;

fn refused(repo: &Repo, args: &[&str]) {
    let before = repo.ok(&["tag"]);
    let out = repo.run_in(&repo.main, args);
    assert_eq!(out.status.code(), Some(129), "pyn {args:?}");
    assert!(out.stdout.is_empty(), "pyn {args:?} wrote to stdout");
    assert!(!out.stderr.is_empty(), "pyn {args:?} gave no message");
    assert_eq!(repo.ok(&["tag"]), before, "pyn {args:?} changed the tags");
}

#[test]
fn create_flags_without_a_name_are_a_usage_error() {
    let repo = Repo::new("sha1");
    for args in [
        &["tag", "-f"][..],
        &["tag", "-m", "msg"],
        &["tag", "-a"],
        &["tag", "-s"],
        &["tag", "-u", "key"],
        &["tag", "-l", "v*", "-m", "x"],
        &["tag", "-l", "-f"],
        &["tag", "-l", "-a"],
        &["tag", "-l", "-s"],
    ] {
        refused(&repo, args);
    }
}

#[test]
fn delete_with_list_or_create_flags_is_a_usage_error() {
    let repo = Repo::new("sha1");
    for args in [
        &["tag", "-l", "-d", "v1"][..],
        &["tag", "-d", "-l"],
        &["tag", "-d", "-f", "v1"],
        &["tag", "-d", "-a", "v1"],
        &["tag", "-d", "-m", "x", "v1"],
        &["tag", "-d", "-s", "v1"],
        &["tag", "-d", "-u", "key", "v1"],
    ] {
        refused(&repo, args);
    }
    assert!(repo.ok(&["tag"]).contains("v1"));
}

#[test]
fn valid_forms_still_work() {
    let repo = Repo::new("sha1");
    let base = repo.ok(&["tag"]);
    repo.ok(&["tag", "-f", "fresh"]);
    repo.ok(&["tag", "-f", "fresh"]);
    repo.ok(&["tag", "-m", "msg", "noted"]);
    repo.ok(&["tag", "-a", "-m", "msg", "-f", "noted"]);
    assert_eq!(repo.ok(&["tag", "-l", "n*"]), "noted\n");
    assert_eq!(
        repo.ok(&["tag", "--no-sign"]).lines().count(),
        base.lines().count() + 2
    );
    assert_eq!(repo.ok(&["tag", "-l", "--no-sign", "f*"]), "fresh\n");
    assert!(
        repo.ok(&["tag", "-d", "fresh", "noted"])
            .contains("Deleted tag 'fresh'")
    );
    assert_eq!(repo.ok(&["tag"]), base);
    assert_eq!(repo.ok(&["tag", "-d"]), "");
}
