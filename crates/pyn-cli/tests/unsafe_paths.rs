//! A workspace never materializes a path that is not safe to write, and never writes through a link: checked against
//! a server that lists such paths, with the work tree and state left as they were. The server refuses to store them, so
//! a forwarding proxy swaps a stored placeholder path for the unsafe one in the file listing.
#![cfg(unix)]

mod common;

use std::collections::BTreeMap;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, Response};
use common::{Env, REPO, start};

const PLACEHOLDER: &str = "Source/zz.txt";

/// Forwards to `target`, replacing `PLACEHOLDER` with `bad` in file listings. Returns its base URL.
fn listing_proxy(target: &str, bad: &str) -> String {
    let target = target.to_string();
    let bad = serde_json::to_string(bad).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let client = Arc::new(reqwest::blocking::Client::new());
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            let handler = move |req: Request<Body>| {
                let (client, target, bad) = (client.clone(), target.clone(), bad.clone());
                async move {
                    let (parts, body) = req.into_parts();
                    let bytes = to_bytes(body, usize::MAX).await.unwrap();
                    tokio::task::spawn_blocking(move || {
                        let url = format!("{target}{}", parts.uri.path_and_query().unwrap());
                        let mut up = client.request(parts.method.clone(), url);
                        for (k, v) in &parts.headers {
                            if k != "host" && k != "content-length" {
                                up = up.header(k, v);
                            }
                        }
                        let resp = up.body(bytes.to_vec()).send().unwrap();
                        let status = resp.status().as_u16();
                        let ctype = resp.headers().get("content-type").cloned();
                        let mut data = resp.bytes().unwrap().to_vec();
                        if parts.uri.path().ends_with("/files") {
                            let text = String::from_utf8(data).unwrap();
                            data = text
                                .replace(&format!("\"{PLACEHOLDER}\""), &bad)
                                .into_bytes();
                        }
                        let mut out = Response::builder().status(status);
                        if let Some(ctype) = ctype {
                            out = out.header("content-type", ctype);
                        }
                        out.body(Body::from(data)).unwrap()
                    })
                    .await
                    .unwrap()
                }
            };
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send(listener.local_addr().unwrap()).unwrap();
            axum::serve(listener, Router::new().fallback(handler))
                .await
                .unwrap();
        });
    });
    format!("http://{}", rx.recv().unwrap())
}

fn run_via(env: &Env, server: &str, cwd: &Path, args: &[&str]) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_pyn"))
        .args(args)
        .current_dir(cwd)
        .env("PYN_SERVER", server)
        .env("PYN_USER", "alice")
        .env("PYN_CONFIG_DIR", env.dir("config"))
        .env("TZ", "UTC")
        .env("PYN_REPO", REPO)
        .env_remove("PYN_TOKEN")
        .env_remove("PYN_DIR")
        .output()
        .unwrap()
}

/// Whether the real server refuses to store `path`.
fn server_refuses(env: &Env, path: &str) -> bool {
    let resp = reqwest::blocking::Client::new()
        .post(format!("{}/v1/repos/{REPO}/checkin", env.url))
        .header("x-pyn-user", "alice")
        .json(&serde_json::json!({"path": path, "content": "0".repeat(40), "message": "m"}))
        .send()
        .unwrap();
    let body: serde_json::Value = resp.json().unwrap();
    body["code"] == "invalid_path"
}

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
        env.seed("alice", PLACEHOLDER, "payload", None);
        let proxy = listing_proxy(&env.url, bad);
        let ws = env.dir("clones").join("ws");
        let out = run_via(
            &env,
            &proxy,
            &env.dir("home"),
            &["clone", &format!("{proxy}/{REPO}"), ws.to_str().unwrap()],
        );
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{bad}");
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
        env.seed("alice", PLACEHOLDER, "payload", None);
        let before = snapshot(&ws);
        let state = std::fs::read_to_string(ws.join(".pyn/local_only/state/files.toml")).unwrap();
        let proxy = listing_proxy(&env.url, bad);
        let out = run_via(&env, &proxy, &ws, &["update"]);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{bad}");
        assert!(err.contains("refusing path"), "{bad}: {err}");
        assert_eq!(snapshot(&ws), before, "{bad}");
        assert_eq!(
            std::fs::read_to_string(ws.join(".pyn/local_only/state/files.toml")).unwrap(),
            state
        );
    }
}

#[test]
fn the_server_refuses_to_store_the_paths_the_client_refuses_to_write() {
    let env = start();
    for bad in UNSAFE {
        let expected = bad != "docs/git~1/x";
        assert_eq!(server_refuses(&env, bad), expected, "{bad}");
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
