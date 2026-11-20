//! `pyn locks --watch` against a real server: the stream's changes arrive as lines or JSON documents, and a resume
//! cursor skips what was already seen.

mod common;

use std::io::{BufRead, BufReader};
use std::process::Child;
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use common::start;
use serde_json::Value;

/// Standard output lines of `child`, delivered as they are printed.
fn lines(child: &mut Child) -> Receiver<String> {
    let out = child.stdout.take().unwrap();
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    rx
}

fn next(rx: &Receiver<String>) -> String {
    rx.recv_timeout(Duration::from_secs(20))
        .expect("the watch printed a line in time")
}

/// Gives the watch time to take its starting cursor, so changes made after this point are the ones it must show.
fn started() {
    std::thread::sleep(Duration::from_millis(1500));
}

#[test]
fn json_watch_prints_one_document_per_change_and_resumes_from_a_cursor() {
    let env = start();
    let home = env.dir("home");
    env.ok(&home, "alice", &["lock", "Content/before.umap"]);

    let mut watch = env.spawn(&home, "alice", &["locks", "--watch", "--json"]);
    let rx = lines(&mut watch);
    started();

    env.ok(&home, "alice", &["lock", "Content/a.umap"]);
    env.ok(&home, "alice", &["unlock", "Content/a.umap"]);
    env.ok(&home, "bob", &["lock", "Content/b.umap"]);
    env.ok(
        &home,
        "alice",
        &["unlock", "--force", "Content/b.umap", "--reason", "stuck"],
    );

    let docs: Vec<Value> = (0..4)
        .map(|_| serde_json::from_str(&next(&rx)).unwrap())
        .collect();
    watch.kill().unwrap();
    let _ = watch.wait();

    let shape: Vec<_> = docs
        .iter()
        .map(|d| {
            (
                d["type"].as_str().unwrap(),
                d["path"].as_str().unwrap(),
                d["holder"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        shape,
        [
            ("lock.acquired", "Content/a.umap", "alice"),
            ("lock.released", "Content/a.umap", "alice"),
            ("lock.acquired", "Content/b.umap", "bob"),
            ("lock.forced", "Content/b.umap", "bob"),
        ]
    );
    assert!(
        docs.iter()
            .all(|d| d["version"] == 1 && d["cursor"].is_string())
    );
    assert!(docs[0]["expires_at"].is_string());
    assert_eq!(docs[3]["actor"], "alice");
    assert_eq!(docs[3]["reason"], "stuck");
    assert!(docs[1]["expires_at"].is_null());

    let after = docs[1]["cursor"].as_str().unwrap();
    let mut resumed = env.spawn(
        &home,
        "alice",
        &["locks", "--watch", "--json", "--after", after],
    );
    let rx = lines(&mut resumed);
    let again: Vec<Value> = (0..2)
        .map(|_| serde_json::from_str(&next(&rx)).unwrap())
        .collect();
    resumed.kill().unwrap();
    let _ = resumed.wait();
    assert_eq!(again[0]["cursor"], docs[2]["cursor"]);
    assert_eq!(again[1]["cursor"], docs[3]["cursor"]);
}

#[test]
fn text_watch_prints_a_line_per_change_with_the_path_last() {
    let env = start();
    let home = env.dir("home");
    let mut watch = env.spawn(&home, "alice", &["locks", "--watch"]);
    let rx = lines(&mut watch);
    started();

    env.ok(&home, "bob", &["lock", "Content/m.umap"]);
    let line = next(&rx);
    watch.kill().unwrap();
    let _ = watch.wait();
    assert!(line.contains("bob") && line.contains("acquired"), "{line}");
    assert!(line.ends_with("Content/m.umap"), "{line}");
}

#[test]
fn a_credential_without_lock_read_access_cannot_watch() {
    let env = start();
    let home = env.dir("home");
    let secret = env
        .ok(
            &home,
            "alice",
            &[
                "token",
                "create",
                "files-only",
                "--permissions",
                "contents:read",
                "--repos",
                "alice/game",
            ],
        )
        .trim()
        .to_string();
    let list = env.run_with_token(&home, &secret, &["locks"]);
    let watch = env.run_with_token(&home, &secret, &["locks", "--watch"]);
    assert!(!list.status.success());
    assert_eq!(watch.status.code(), list.status.code());
    assert!(watch.stdout.is_empty());
}
