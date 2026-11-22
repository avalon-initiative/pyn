//! A push and a clone through the real server with its content in an S3 bucket (the in-process S3 server).

mod common;

use common::real::Server;
use pyn_s3::testing::{ACCESS_KEY, BUCKET, SECRET_KEY, Server as Bucket};

fn objects_in(bucket: &Bucket) -> usize {
    fn count(dir: &std::path::Path) -> usize {
        std::fs::read_dir(dir).map_or(0, |entries| {
            entries
                .flatten()
                .map(|e| {
                    if e.path().is_dir() {
                        count(&e.path())
                    } else {
                        1
                    }
                })
                .sum()
        })
    }
    count(&bucket.root.join(BUCKET))
}

#[test]
fn a_push_and_a_clone_go_through_the_bucket() {
    let bucket = Bucket::start();
    let endpoint = format!("http://{}", bucket.addr);
    let server = Server::with_game_env(&[
        ("PYN_S3_ENDPOINT", &endpoint),
        ("PYN_S3_BUCKET", BUCKET),
        ("PYN_S3_PATH_STYLE", "true"),
        ("PYN_S3_PREFIX", "pyn"),
        ("PYN_S3_ACCESS_KEY_ID", ACCESS_KEY),
        ("PYN_S3_SECRET_ACCESS_KEY", SECRET_KEY),
    ]);
    server.seed("Source/a.cpp", "int a;\n");
    assert!(
        objects_in(&bucket) > 0,
        "the seeded file reached the bucket"
    );
    assert!(
        bucket.root.join(BUCKET).join("pyn").is_dir(),
        "under the prefix"
    );

    let ws = server.dir("ws").join("game");
    let source = server.source();
    server.ok(
        &server.dir("home"),
        "alice",
        &["clone", "--local-store", &source, ws.to_str().unwrap()],
    );
    std::fs::write(ws.join("Source/b.cpp"), "int b;\n").unwrap();
    server.ok(&ws, "alice", &["add", "Source/b.cpp"]);
    server.ok(&ws, "alice", &["commit", "-m", "second file"]);
    let before = objects_in(&bucket);
    let out = server.ok(&ws, "alice", &["push"]);
    assert!(out.contains("main -> main"), "{out}");
    assert!(
        objects_in(&bucket) > before,
        "the pushed file reached the bucket"
    );

    let other = server.dir("other").join("game");
    server.ok(
        &server.dir("home"),
        "alice",
        &["clone", &source, other.to_str().unwrap()],
    );
    assert_eq!(
        std::fs::read_to_string(other.join("Source/a.cpp")).unwrap(),
        "int a;\n"
    );
    assert_eq!(
        std::fs::read_to_string(other.join("Source/b.cpp")).unwrap(),
        "int b;\n"
    );
}
