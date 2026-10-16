//! A workspace never materializes a path that is not safe to write, and never writes through a link: checked against
//! a real server holding such paths, with the work tree and state left as they were.
#![cfg(unix)]

mod common;

use std::collections::BTreeMap;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use common::{Env, REPO, start};

fn clone_to(env: &Env, name: &str) -> PathBuf {
    let ws = env.dir(name);
    env.ok(
        &env.dir("home"),
        "alice",
        &["clone", &env.source(REPO), ws.to_str().unwrap()],
    );
    ws.canonicalize().unwrap()
}

fn try_clone(env: &Env, name: &str) -> (PathBuf, String) {
    let ws = env.dir("clones").join(name);
    let err = env.fails(
        &env.dir("home"),
        "alice",
        &["clone", &env.source(REPO), ws.to_str().unwrap()],
    );
    (ws, err)
}

fn snapshot(root: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in read {
            let path = entry.unwrap().path();
            let rel = path.strip_prefix(root).unwrap().display().to_string();
            let meta = std::fs::symlink_metadata(&path).unwrap();
            if meta.is_dir() {
                stack.push(path);
            } else if meta.file_type().is_symlink() {
                out.insert(
                    rel,
                    format!("-> {}", std::fs::read_link(&path).unwrap().display()),
                );
            } else {
                out.insert(rel, std::fs::read_to_string(&path).unwrap_or_default());
            }
        }
    }
    out
}

const UNSAFE: [&str; 5] = [
    "sub/.git/config",
    ".GIT/hooks/x",
    ".pyn/local_only/config.toml",
    "docs/git~1/x",
    "notes/a\u{1b}[2Jb.txt",
];

#[test]
fn clone_refuses_a_listing_with_an_unsafe_path_before_creating_anything() {
    for bad in UNSAFE {
        let env = start();
        env.seed("alice", "Source/ok.txt", "fine", None);
        env.seed("alice", bad, "payload", None);
        let (ws, err) = try_clone(&env, "ws");
        assert!(err.contains("refusing path"), "{bad}: {err}");
        assert!(!ws.exists(), "{bad}: nothing is created");
    }
}

#[test]
fn update_refuses_the_whole_plan_when_one_path_is_unsafe() {
    for bad in UNSAFE {
        let env = start();
        env.seed("alice", "Source/ok.txt", "one", None);
        let ws = clone_to(&env, "ws");
        env.seed("alice", "Source/ok.txt", "two", Some(1));
        env.seed("alice", bad, "payload", None);
        let before = snapshot(&ws);
        let state = std::fs::read_to_string(ws.join(".pyn/local_only/state/files.toml")).unwrap();
        let err = env.fails(&ws, "alice", &["update"]);
        assert!(err.contains("refusing path"), "{bad}: {err}");
        assert_eq!(snapshot(&ws), before, "{bad}");
        assert_eq!(
            std::fs::read_to_string(ws.join(".pyn/local_only/state/files.toml")).unwrap(),
            state
        );
    }
}

#[test]
fn update_refuses_to_write_through_a_linked_folder() {
    let env = start();
    env.seed("alice", "Source/ok.txt", "one", None);
    let ws = clone_to(&env, "ws");
    let outside = env.dir("outside");
    std::fs::remove_file(ws.join("Source/ok.txt")).unwrap();
    std::fs::remove_dir(ws.join("Source")).unwrap();
    symlink(&outside, ws.join("Source")).unwrap();
    env.seed("alice", "Source/ok.txt", "two", Some(1));
    env.seed("alice", "Source/new.txt", "fresh", None);
    let err = env.fails(&ws, "alice", &["update"]);
    assert!(err.contains("refusing to write through"), "{err}");
    assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
    assert!(
        std::fs::symlink_metadata(ws.join("Source"))
            .unwrap()
            .is_symlink()
    );
}

#[test]
fn clone_refuses_a_link_that_leaves_the_workspace() {
    for target in ["../../outside", "/etc/passwd", "a/../../../b"] {
        let env = start();
        env.seed("alice", "Source/ok.txt", "one", None);
        let seeded = clone_to(&env, "seeded");
        symlink(target, seeded.join("Source/l")).unwrap();
        env.ok(&seeded, "alice", &["commit", "Source/l", "-m", "link"]);
        let (ws, err) = try_clone(&env, "ws");
        assert!(err.contains("symbolic link target"), "{target}: {err}");
        assert!(
            !ws.join("Source/l").exists() && !ws.join("Source/l").is_symlink(),
            "{target}"
        );
    }
}

#[test]
fn a_link_inside_the_workspace_is_still_made() {
    let env = start();
    env.seed("alice", "Source/ok.txt", "one", None);
    let seeded = clone_to(&env, "seeded");
    symlink("ok.txt", seeded.join("Source/l")).unwrap();
    env.ok(&seeded, "alice", &["commit", "Source/l", "-m", "link"]);
    let ws = clone_to(&env, "ws");
    assert_eq!(
        std::fs::read_link(ws.join("Source/l")).unwrap(),
        Path::new("ok.txt")
    );
}
