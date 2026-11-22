//! A push through one server copy is cloned through another: two real servers on one PostgreSQL schema and one
//! bucket (`PYN_INSTANCES=2`). Needs `PYN_DATABASE_URL`.

mod common;

use common::real::{PASSWORD, Server};
use pyn_postgres::PgMetadataStore;
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

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs PYN_DATABASE_URL"]
async fn a_push_through_one_copy_is_cloned_through_the_other() {
    let url = std::env::var("PYN_DATABASE_URL").expect("PYN_DATABASE_URL must be set");
    let store = PgMetadataStore::connect_in_scratch_schema(&url)
        .await
        .unwrap();
    let joiner = if url.contains('?') { '&' } else { '?' };
    let scoped = format!(
        "{url}{joiner}options=-csearch_path%3D{}",
        store.scratch_schema().unwrap()
    );
    let bucket = Bucket::start();
    let endpoint = format!("http://{}", bucket.addr);
    let env = [
        ("PYN_DATABASE_URL", scoped.as_str()),
        ("PYN_INSTANCES", "2"),
        ("PYN_SECRET_KEY", &"ab".repeat(32)),
        ("PYN_S3_ENDPOINT", &endpoint),
        ("PYN_S3_BUCKET", BUCKET),
        ("PYN_S3_PATH_STYLE", "true"),
        ("PYN_S3_ACCESS_KEY_ID", ACCESS_KEY),
        ("PYN_S3_SECRET_ACCESS_KEY", SECRET_KEY),
    ];
    let a = Server::start_with_env(&env);
    let b = Server::start_with_env(&env);
    a.provision();
    a.seed("Source/a.cpp", "int a;\n");
    let after_seed = objects_in(&bucket);
    assert!(after_seed > 0, "the seeded file reached the bucket");

    let cwd = b.dir("login");
    let input = format!("{PASSWORD}\n");
    let out = b.run_with(
        &cwd,
        "alice",
        &["login", "alice", "--password-stdin"],
        Some(&input),
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let ws = b.dir("ws").join("game");
    b.ok(
        &b.dir("home"),
        "alice",
        &["clone", "--local-store", &b.source(), ws.to_str().unwrap()],
    );
    assert_eq!(
        std::fs::read_to_string(ws.join("Source/a.cpp")).unwrap(),
        "int a;\n"
    );
    std::fs::write(ws.join("Source/b.cpp"), "int b;\n").unwrap();
    b.ok(&ws, "alice", &["add", "Source/b.cpp"]);
    b.ok(&ws, "alice", &["commit", "-m", "second file"]);
    let out = b.ok(&ws, "alice", &["push"]);
    assert!(out.contains("main -> main"), "{out}");
    assert!(
        objects_in(&bucket) > after_seed,
        "the push reached the bucket"
    );

    let other = a.dir("other").join("game");
    a.ok(
        &a.dir("home"),
        "alice",
        &["clone", &a.source(), other.to_str().unwrap()],
    );
    for (file, text) in [("Source/a.cpp", "int a;\n"), ("Source/b.cpp", "int b;\n")] {
        assert_eq!(std::fs::read_to_string(other.join(file)).unwrap(), text);
    }
}
