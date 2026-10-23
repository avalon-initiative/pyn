//! `pyn tag` in a workspace with `local_store` on, against a real server. Real git reads the store as an oracle
//! for semantics only.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::{Env, REPO, start};

fn clone_with(env: &Env, name: &str, extra: &[&str]) -> PathBuf {
    let ws = env.dir(name);
    let source = env.source(REPO);
    let mut args = vec!["clone"];
    args.extend(extra);
    args.extend([source.as_str(), ws.to_str().unwrap()]);
    env.ok(&env.dir("home"), "alice", &args);
    ws.canonicalize().unwrap()
}

fn git(ws: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("--git-dir")
        .arg(ws.join(".pyn/local_only/store"))
        .args(["-c", "gc.auto=0", "-c", "maintenance.auto=false"])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn commit_local(env: &Env, ws: &Path, path: &str, text: &str, message: &str) {
    std::fs::write(ws.join(path), text).unwrap();
    env.ok(ws, "alice", &["add", path]);
    env.ok(ws, "alice", &["commit", "-m", message]);
}

fn seeded() -> (Env, PathBuf) {
    let env = start();
    env.seed("alice", "Source/a.cpp", "one", None);
    let ws = clone_with(&env, "ws", &["--local-store"]);
    (env, ws)
}

#[test]
fn lightweight_and_annotated_tags_are_created_listed_and_deleted() {
    let (env, ws) = seeded();
    commit_local(&env, &ws, "Source/a.cpp", "two", "first local");
    let head = git(&ws, &["rev-parse", "refs/heads/main"]);

    env.ok(&ws, "alice", &["tag", "light"]);
    env.ok(&ws, "alice", &["tag", "-m", "release one", "note"]);
    assert_eq!(git(&ws, &["rev-parse", "refs/tags/light"]), head);
    assert_eq!(git(&ws, &["cat-file", "-t", "refs/tags/light"]), "commit");
    assert_eq!(git(&ws, &["cat-file", "-t", "refs/tags/note"]), "tag");
    assert_eq!(git(&ws, &["rev-parse", "refs/tags/note^{commit}"]), head);
    assert!(
        git(&ws, &["cat-file", "-p", "refs/tags/note"]).ends_with("release one"),
        "annotated message stored"
    );

    assert_eq!(env.ok(&ws, "alice", &["tag"]), "light\nnote\n");
    assert_eq!(env.ok(&ws, "alice", &["tag", "-l", "n*"]), "note\n");

    let gone = env.ok(&ws, "alice", &["tag", "-d", "light"]);
    assert!(gone.starts_with("Deleted tag 'light' (was "), "{gone}");
    assert_eq!(env.ok(&ws, "alice", &["tag"]), "note\n");
    env.fails(&ws, "alice", &["tag", "-d", "light"]);
}

#[test]
fn a_tag_can_name_an_older_commit_and_is_not_replaced_without_force() {
    let (env, ws) = seeded();
    commit_local(&env, &ws, "Source/a.cpp", "two", "first local");
    let published = git(&ws, &["rev-parse", "refs/heads/main~1"]);
    env.ok(&ws, "alice", &["tag", "base", "HEAD~1"]);
    assert_eq!(git(&ws, &["rev-parse", "refs/tags/base"]), published);

    let err = env.fails(&ws, "alice", &["tag", "base"]);
    assert!(err.contains("already exists"), "{err}");
    let updated = env.ok(&ws, "alice", &["tag", "-f", "base"]);
    assert!(updated.starts_with("Updated tag 'base'"), "{updated}");
    assert_ne!(git(&ws, &["rev-parse", "refs/tags/base"]), published);

    env.ok(&ws, "alice", &["tag", "again", "base"]);
    env.fails(&ws, "alice", &["tag", "bad name"]);
    env.fails(&ws, "alice", &["tag", "-s", "-m", "x", "signed"]);
}

#[test]
fn show_resolves_a_tag_and_an_annotated_tag_shows_its_message_first() {
    let (env, ws) = seeded();
    commit_local(&env, &ws, "Source/a.cpp", "two", "first local");
    env.ok(&ws, "alice", &["tag", "light"]);
    env.ok(&ws, "alice", &["tag", "-m", "release one", "note", "HEAD"]);
    env.ok(&ws, "alice", &["tag", "old", "HEAD~1"]);

    let plain = env.ok(&ws, "alice", &["show", "light"]);
    assert!(plain.starts_with("commit "), "{plain}");
    assert!(plain.contains("first local"), "{plain}");

    let shown = env.ok(&ws, "alice", &["show", "note"]);
    assert!(
        shown.starts_with("tag note\nTagger: alice <alice@pyn.invalid>\nDate:   "),
        "{shown}"
    );
    let (tag, commit) = shown.split_once("\n\nrelease one\n\n").expect(&shown);
    assert!(tag.starts_with("tag note"), "{tag}");
    assert!(commit.starts_with("commit "), "{commit}");
    assert!(commit.contains("first local"), "{commit}");

    let steps = env.ok(&ws, "alice", &["show", "note~1"]);
    assert!(steps.starts_with("commit "), "{steps}");
    let published = env.ok(&ws, "alice", &["show", "old"]);
    assert!(published.starts_with("commit "), "{published}");
    assert!(!published.contains("first local"), "{published}");
}

#[test]
fn diff_resolves_tag_names() {
    let (env, ws) = seeded();
    env.ok(&ws, "alice", &["tag", "v1", "HEAD"]);
    commit_local(&env, &ws, "Source/a.cpp", "two", "first local");
    env.ok(&ws, "alice", &["tag", "v2"]);
    let by_tag = env.ok(&ws, "alice", &["diff", "v1", "--", "Source/a.cpp"]);
    assert!(
        by_tag.contains("-one") && by_tag.contains("+two"),
        "{by_tag}"
    );
}

#[test]
fn tags_are_not_pushed_and_survive_a_push() {
    let (env, ws) = seeded();
    commit_local(&env, &ws, "Source/a.cpp", "two", "first local");
    env.ok(&ws, "alice", &["tag", "keep"]);
    env.ok(&ws, "alice", &["push"]);
    assert_eq!(env.ok(&ws, "alice", &["tag"]), "keep\n");
}

#[test]
fn without_a_local_store_tag_says_how_to_get_one() {
    let env = start();
    env.seed("alice", "Source/a.cpp", "one", None);
    let ws = clone_with(&env, "plain", &["--no-local-store"]);
    let err = env.fails(&ws, "alice", &["tag", "v1"]);
    assert!(err.contains("local store"), "{err}");
}

fn published(env: &Env, path: &str, text: &str, message: &str) {
    let dir = env.dir("seed");
    let file = dir.join("upload");
    std::fs::write(&file, text).unwrap();
    env.ok(
        &dir,
        "alice",
        &[
            "commit",
            path,
            "--file",
            file.to_str().unwrap(),
            "-m",
            message,
        ],
    );
}

fn subjects(env: &Env, ws: &Path, args: &[&str]) -> String {
    let mut all = vec!["log", "--format=%s"];
    all.extend(args);
    env.ok(ws, "alice", &all)
}

/// Three published commits (one, two, three) tagged `v1` (first) and `v2` (second), and a tag `Source` on the first.
fn three() -> (Env, PathBuf) {
    let env = start();
    published(&env, "Source/a.cpp", "a", "one");
    published(&env, "Source/b.cpp", "b", "two");
    published(&env, "Docs/c.md", "c", "three");
    let ws = clone_with(&env, "ws", &["--local-store"]);
    env.ok(&ws, "alice", &["tag", "v1", "HEAD~2"]);
    env.ok(&ws, "alice", &["tag", "-m", "second", "v2", "HEAD~1"]);
    env.ok(&ws, "alice", &["tag", "Source", "HEAD~2"]);
    (env, ws)
}

#[test]
fn log_takes_tag_names_as_revisions_and_in_ranges() {
    let (env, ws) = three();
    assert_eq!(subjects(&env, &ws, &["v2"]), "two\none\n");
    assert_eq!(subjects(&env, &ws, &["v2~1"]), "one\n");
    assert_eq!(subjects(&env, &ws, &["v2^"]), "one\n");
    assert_eq!(subjects(&env, &ws, &["v1..HEAD"]), "three\ntwo\n");
    assert_eq!(subjects(&env, &ws, &["v1..v2"]), "two\n");
    assert_eq!(subjects(&env, &ws, &["v1.."]), "three\ntwo\n");
    assert_eq!(subjects(&env, &ws, &["..v2"]), "");
    assert_eq!(subjects(&env, &ws, &["^v1", "HEAD"]), "three\ntwo\n");
    assert_eq!(subjects(&env, &ws, &["v2", "--", "Source/a.cpp"]), "one\n");
}

#[test]
fn a_path_named_like_a_tag_stays_a_path_unless_dashes_say_otherwise() {
    let (env, ws) = three();
    assert_eq!(subjects(&env, &ws, &["Source"]), "two\none\n");
    assert_eq!(subjects(&env, &ws, &["Source", "--", "Source"]), "one\n");
}

#[test]
fn a_tag_on_an_unpublished_commit_is_refused_by_log_with_a_reason() {
    let (env, ws) = three();
    commit_local(&env, &ws, "Source/a.cpp", "local", "local work");
    env.ok(&ws, "alice", &["tag", "mine"]);
    let err = env.fails(&ws, "alice", &["log", "mine"]);
    assert!(
        err.contains("'mine' names an unpublished local commit"),
        "{err}"
    );
    env.ok(&ws, "alice", &["push"]);
    assert_eq!(
        subjects(&env, &ws, &["mine"]),
        "local work\nthree\ntwo\none\n"
    );
}
