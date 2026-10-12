//! Git file modes in a workspace: the executable bit and symbolic links are committed, shown, and restored on clone.
#![cfg(unix)]

mod common;

use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;

use common::{REPO, start};

fn mode(path: &Path) -> u32 {
    std::fs::symlink_metadata(path)
        .unwrap()
        .permissions()
        .mode()
        & 0o111
}

fn clone(env: &common::Env, name: &str) -> std::path::PathBuf {
    let ws = env.dir(name);
    env.ok(
        &env.dir("home"),
        "alice",
        &["clone", &env.source(REPO), ws.to_str().unwrap()],
    );
    ws.canonicalize().unwrap()
}

#[test]
fn an_executable_file_keeps_its_bit_through_commit_show_and_clone() {
    let env = start();
    env.seed("alice", "Source/run.sh", "#!/bin/sh\n", None);
    let ws = clone(&env, "ws");
    let script = ws.join("Source/run.sh");
    assert_eq!(mode(&script), 0, "an ordinary file is not executable");
    assert!(
        env.ok(&ws, "alice", &["status"])
            .contains("everything is up to date")
    );

    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let status = env.ok(&ws, "alice", &["status"]);
    assert!(
        status.contains("modified"),
        "a mode change alone is a change: {status}"
    );
    let diff = env.ok(&ws, "alice", &["diff"]);
    assert!(
        diff.contains("old mode 100644\nnew mode 100755\n"),
        "{diff}"
    );

    env.ok(
        &ws,
        "alice",
        &["commit", "Source/run.sh", "-m", "make executable"],
    );
    assert!(
        env.ok(&ws, "alice", &["status"])
            .contains("everything is up to date")
    );
    let shown = env.ok(&ws, "alice", &["show", "--name-status"]);
    assert!(shown.ends_with("M\tSource/run.sh\n"), "{shown}");

    let other = clone(&env, "other");
    assert_eq!(mode(&other.join("Source/run.sh")) & 0o100, 0o100);
}

#[test]
fn a_new_executable_file_is_created_with_its_mode() {
    let env = start();
    let ws = clone(&env, "ws");
    let tool = ws.join("tool.sh");
    std::fs::write(&tool, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
    env.ok(&ws, "alice", &["commit", "tool.sh", "-m", "add tool"]);
    let shown = env.ok(&ws, "alice", &["show"]);
    assert!(
        shown.contains("new file mode 100755"),
        "the commit records the executable bit: {shown}"
    );
}

#[test]
fn a_symlink_is_committed_as_its_target_and_recreated_on_clone() {
    let env = start();
    env.seed("alice", "Source/a.txt", "one\n", None);
    let ws = clone(&env, "ws");
    symlink("a.txt", ws.join("Source/link")).unwrap();
    let status = env.ok(&ws, "alice", &["status"]);
    assert!(status.contains("untracked"), "{status}");

    env.ok(&ws, "alice", &["commit", "Source/link", "-m", "add link"]);
    let shown = env.ok(&ws, "alice", &["show"]);
    assert!(shown.contains("new file mode 120000"), "{shown}");
    assert!(
        shown.contains("+a.txt"),
        "the target is the content: {shown}"
    );
    assert!(
        env.ok(&ws, "alice", &["status"])
            .contains("everything is up to date")
    );

    let other = clone(&env, "other");
    let link = other.join("Source/link");
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(std::fs::read_link(&link).unwrap(), Path::new("a.txt"));
    assert_eq!(std::fs::read_to_string(&link).unwrap(), "one\n");

    std::fs::remove_file(&link).unwrap();
    symlink("b.txt", &link).unwrap();
    let status = env.ok(&other, "alice", &["status"]);
    assert!(
        status.contains("Source/link") && status.contains("modified"),
        "{status}"
    );
}
