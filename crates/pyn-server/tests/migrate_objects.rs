//! `pyn-server migrate-objects` between a data directory and the in-process S3 server: the real binary for the
//! command's behaviour, and the library over a pushed repository to show the data is intact on the other side.

#[path = "common/cred.rs"]
mod cred;
use cred::{Cred, WithCred};

use std::collections::BTreeMap;
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use http_body_util::BodyExt;
use pyn_core::commit_record::{TreeChange, tree_of};
use pyn_core::memory::{MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore};
use pyn_core::{
    AccessConfig, AccessService, AuthProvider, ChunkedStore, FileMode, ObjectId, ObjectStore,
    REPO_ALGORITHM, RegistrationMode, RepoPath, Repositories, Rules, StoredKind, SystemClock,
};
use pyn_fs::FsObjectStore;
use pyn_object::{Algorithm, Kind, Signature, hash_object};
use pyn_proto as api;
use pyn_s3::S3ObjectStore;
use pyn_s3::testing::{ACCESS_KEY, BUCKET, SECRET_KEY, Server};
use pyn_server::auth::{BearerAuth, DevHeaderAuth};
use pyn_server::migrate_objects::{Options, migrate};
use pyn_server::{AppState, router};
use serde_json::json;
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";

fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

fn free_addr() -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().to_string()
}

fn s3_env(s3: &Server) -> Vec<(String, String)> {
    [
        ("PYN_S3_ENDPOINT", format!("http://{}", s3.addr)),
        ("PYN_S3_BUCKET", BUCKET.into()),
        ("PYN_S3_PATH_STYLE", "true".into()),
        ("PYN_S3_ACCESS_KEY_ID", ACCESS_KEY.into()),
        ("PYN_S3_SECRET_ACCESS_KEY", SECRET_KEY.into()),
        ("PYN_S3_MAX_RETRIES", "0".into()),
        ("PYN_ADDR", free_addr()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect()
}

fn run(args: &[&str], env: &[(String, String)]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pyn-server"))
        .arg("migrate-objects")
        .args(args)
        .env_clear()
        .envs(env.iter().map(|(k, v)| (k, v)))
        .output()
        .unwrap()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// A populated filesystem store as the server lays it out: small objects, a chunked file over 1 MiB and its manifest.
async fn populated(dir: &Path) -> (ChunkedStore, Vec<ObjectId>, Vec<u8>) {
    let store = ChunkedStore::new(Arc::new(FsObjectStore::open(dir).await.unwrap()));
    let mut ids = Vec::new();
    for n in 0..12 {
        ids.push(store.put(noise(500 + n * 100, n as u64 + 1)).await.unwrap());
    }
    let big = noise(3 << 20, 99);
    ids.push(store.put(big.clone()).await.unwrap());
    (store, ids, big)
}

fn bucket_dir(s3: &Server) -> std::path::PathBuf {
    s3.root.join(BUCKET)
}

fn stored_files(s3: &Server) -> usize {
    walk(&bucket_dir(s3))
}

fn walk(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .map(|d| {
            d.flatten()
                .map(|e| {
                    if e.path().is_dir() {
                        walk(&e.path())
                    } else {
                        1
                    }
                })
                .sum()
        })
        .unwrap_or(0)
}

fn count(text: &str, label: &str) -> u64 {
    let line = text
        .lines()
        .find(|l| l.starts_with(label))
        .unwrap_or_else(|| panic!("no {label} line in {text}"));
    line[label.len()..]
        .trim_start_matches(':')
        .trim()
        .split(' ')
        .next()
        .unwrap()
        .parse()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_populated_store_is_copied_then_a_second_run_copies_nothing() {
    let (src, s3) = (tempfile::tempdir().unwrap(), Server::start());
    populated(src.path()).await;
    let env = s3_env(&s3);
    let from = src.path().to_str().unwrap();

    let first = run(&["--from-data-dir", from, "--to-s3"], &env);
    assert!(first.status.success(), "{}", err(&first));
    let copied = count(&out(&first), "copied");
    assert!(copied > 12, "{}", out(&first));
    assert_eq!(count(&out(&first), "failed"), 0);
    assert!(err(&first).contains("progress:"), "{}", err(&first));
    for secret in [SECRET_KEY, ACCESS_KEY] {
        assert!(!err(&first).contains(secret) && !out(&first).contains(secret));
    }
    let files = stored_files(&s3);
    assert!(files >= copied as usize);

    let again = run(&["--from-data-dir", from, "--to-s3"], &env);
    assert!(again.status.success(), "{}", err(&again));
    assert_eq!(count(&out(&again), "copied"), 0);
    assert_eq!(count(&out(&again), "skipped"), copied);
    assert_eq!(stored_files(&s3), files);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dry_run_reports_counts_and_writes_nothing() {
    let (src, s3) = (tempfile::tempdir().unwrap(), Server::start());
    populated(src.path()).await;
    let env = s3_env(&s3);
    let before = stored_files(&s3);
    let dry = run(
        &[
            "--from-data-dir",
            src.path().to_str().unwrap(),
            "--to-s3",
            "--dry-run",
        ],
        &env,
    );
    assert!(dry.status.success(), "{}", err(&dry));
    assert!(out(&dry).starts_with("would copy: "), "{}", out(&dry));
    assert!(count(&out(&dry), "would copy") > 12);
    assert_eq!(stored_files(&s3), before);

    let fresh = tempfile::tempdir().unwrap().keep().join("new");
    let dry = run(
        &[
            "--from-data-dir",
            src.path().to_str().unwrap(),
            "--to-data-dir",
            fresh.to_str().unwrap(),
            "--dry-run",
        ],
        &env,
    );
    assert!(dry.status.success(), "{}", err(&dry));
    assert!(
        !fresh.exists(),
        "a dry run created the destination directory"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_interrupted_copy_resumes_and_copies_only_what_is_missing() {
    let (src, s3) = (tempfile::tempdir().unwrap(), Server::start());
    populated(src.path()).await;
    let env = s3_env(&s3);
    let from = src.path().to_str().unwrap();
    let all = run(&["--from-data-dir", from, "--to-s3"], &env);
    let total = count(&out(&all), "copied");

    let mut lost = 0;
    for shard in std::fs::read_dir(bucket_dir(&s3)).unwrap().flatten() {
        if !shard.path().is_dir() || lost == 3 {
            continue;
        }
        let first = std::fs::read_dir(shard.path()).unwrap().flatten().next();
        if let Some(file) = first {
            std::fs::remove_file(file.path()).unwrap();
            lost += 1;
        }
    }
    assert_eq!(lost, 3);
    let resumed = run(&["--from-data-dir", from, "--to-s3"], &env);
    assert!(resumed.status.success(), "{}", err(&resumed));
    assert_eq!(count(&out(&resumed), "copied"), 3);
    assert_eq!(count(&out(&resumed), "skipped"), total - 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn verify_and_the_next_run_catch_a_corrupted_destination_object() {
    let (src, s3) = (tempfile::tempdir().unwrap(), Server::start());
    let (_, ids, _) = populated(src.path()).await;
    let env = s3_env(&s3);
    let from = src.path().to_str().unwrap();
    let args = ["--from-data-dir", from, "--to-s3", "--verify"];
    let clean = run(&args, &env);
    assert!(clean.status.success(), "{}", err(&clean));
    assert_eq!(
        count(&out(&clean), "verified"),
        count(&out(&clean), "copied")
    );

    let hex = ids[2].to_hex();
    let at = bucket_dir(&s3).join(&hex[..2]).join(&hex[2..]);
    let len = std::fs::metadata(&at).unwrap().len() as usize;
    std::fs::write(&at, vec![0x55u8; len]).unwrap();

    let bad = run(&args, &env);
    assert!(!bad.status.success());
    assert_eq!(count(&out(&bad), "failed"), 1);
    assert!(out(&bad).contains(&hex), "{}", out(&bad));
    assert!(err(&bad).contains("failed"), "{}", err(&bad));
    assert_eq!(
        std::fs::read(&at).unwrap(),
        vec![0x55u8; len],
        "nothing is overwritten"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn parallel_and_serial_runs_give_the_same_result() {
    let src = tempfile::tempdir().unwrap();
    populated(src.path()).await;
    let from = src.path().to_str().unwrap();
    let mut results = Vec::new();
    for jobs in ["1", "8"] {
        let s3 = Server::start();
        let o = run(
            &["--from-data-dir", from, "--to-s3", "--jobs", jobs],
            &s3_env(&s3),
        );
        assert!(o.status.success(), "{}", err(&o));
        results.push((out(&o), stored_files(&s3)));
    }
    assert_eq!(results[0], results[1]);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_copy_back_to_a_data_directory_restores_every_object() {
    let (src, s3) = (tempfile::tempdir().unwrap(), Server::start());
    let (original, ids, big) = populated(src.path()).await;
    let env = s3_env(&s3);
    let up = run(
        &["--from-data-dir", src.path().to_str().unwrap(), "--to-s3"],
        &env,
    );
    assert!(up.status.success(), "{}", err(&up));

    let back = tempfile::tempdir().unwrap();
    let dest = back.path().join("restored");
    let down = run(
        &[
            "--from-s3",
            "--to-data-dir",
            dest.to_str().unwrap(),
            "--verify",
        ],
        &env,
    );
    assert!(down.status.success(), "{}", err(&down));
    let restored = ChunkedStore::new(Arc::new(FsObjectStore::open(&dest).await.unwrap()));
    for id in &ids {
        assert_eq!(
            restored.get(id).await.unwrap(),
            original.get(id).await.unwrap()
        );
    }
    assert_eq!(
        restored.get(ids.last().unwrap()).await.unwrap().unwrap(),
        big
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn refusals_name_the_cause_and_never_echo_the_secret() {
    let (src, s3) = (tempfile::tempdir().unwrap(), Server::start());
    populated(src.path()).await;
    let env = s3_env(&s3);
    let from = src.path().to_str().unwrap();

    let same = run(&["--from-data-dir", from, "--to-data-dir", from], &env);
    assert!(
        !same.status.success() && err(&same).contains("same store"),
        "{}",
        err(&same)
    );
    let nested = src.path().join("inside");
    let inside = run(
        &[
            "--from-data-dir",
            from,
            "--to-data-dir",
            nested.to_str().unwrap(),
        ],
        &env,
    );
    assert!(!inside.status.success() && err(&inside).contains("same store"));
    assert!(!nested.exists());

    let both = run(&["--from-s3", "--to-s3"], &env);
    assert!(!both.status.success() && err(&both).contains("both the bucket"));

    let bare: Vec<_> = env
        .iter()
        .filter(|(k, _)| k == "PYN_ADDR")
        .cloned()
        .collect();
    let unset = run(&["--from-data-dir", from, "--to-s3"], &bare);
    assert!(
        !unset.status.success() && err(&unset).contains("PYN_S3_BUCKET"),
        "{}",
        err(&unset)
    );

    let mut wrong = env.clone();
    wrong.retain(|(k, _)| k != "PYN_S3_SECRET_ACCESS_KEY");
    wrong.push((
        "PYN_S3_SECRET_ACCESS_KEY".into(),
        "a-wrong-secret-value".into(),
    ));
    let denied = run(&["--from-data-dir", from, "--to-s3"], &wrong);
    assert!(!denied.status.success());
    assert!(!err(&denied).contains("a-wrong-secret-value") && !out(&denied).contains("a-wrong"));

    let missing = run(&["--from-data-dir", "/no/such/dir", "--to-s3"], &env);
    assert!(!missing.status.success() && err(&missing).contains("/no/such/dir"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_answering_on_its_address_stops_the_run_unless_forced() {
    let (src, s3) = (tempfile::tempdir().unwrap(), Server::start());
    populated(src.path()).await;
    let mut env = s3_env(&s3);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    env.retain(|(k, _)| k != "PYN_ADDR");
    env.push(("PYN_ADDR".into(), addr));
    std::thread::spawn(move || {
        use std::io::{Read, Write};
        for stream in listener.incoming().flatten() {
            let mut stream = stream;
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok");
        }
    });
    let from = src.path().to_str().unwrap();
    let refused = run(&["--from-data-dir", from, "--to-s3"], &env);
    assert!(!refused.status.success());
    assert!(err(&refused).contains("stop it") && err(&refused).contains("--force"));
    assert_eq!(stored_files(&s3), 0);

    let dry = run(&["--from-data-dir", from, "--to-s3", "--dry-run"], &env);
    assert!(dry.status.success(), "{}", err(&dry));
    let forced = run(&["--from-data-dir", from, "--to-s3", "--force"], &env);
    assert!(forced.status.success(), "{}", err(&forced));
    assert!(stored_files(&s3) > 0);
}

// The data shape: a repository pushed over the API with a chunked file, moved to the bucket and back.

struct Shared {
    meta: Arc<MemoryMetadataStore>,
    access_store: Arc<MemoryAccessStore>,
    audit: Arc<MemoryAuditStore>,
}

fn app_over(objects: Arc<dyn ObjectStore>, shared: &Shared) -> Router {
    let clock = Arc::new(SystemClock);
    let access = Arc::new(
        AccessService::new(
            shared.access_store.clone(),
            clock.clone(),
            Arc::new(pyn_core::MemoryLimitEvents::new()),
        )
        .with_config(AccessConfig {
            registration: RegistrationMode::Open,
            require_email_verification: false,
            ..AccessConfig::default()
        })
        .with_registry(shared.meta.clone())
        .with_audit(shared.audit.clone()),
    );
    let repos = Repositories::new(
        shared.meta.clone(),
        objects.clone(),
        shared.audit.clone(),
        access.clone(),
        clock,
        Rules::from_toml("[meta]\ndefault = \"shared\"\n").unwrap(),
    );
    router(AppState {
        repos: Arc::new(repos),
        objects,
        access: access.clone(),
        auth: Arc::new(BearerAuth { access }),
        dev_auth: Some(Arc::new(DevHeaderAuth) as Arc<dyn AuthProvider>),
        trust_forwarded: false,
        updates: None,
        storage: Default::default(),
    })
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    token: &str,
    range: Option<&str>,
    body: Body,
    json_body: bool,
) -> (StatusCode, Vec<u8>) {
    let cred = if token.is_empty() {
        Cred::SignedOut
    } else {
        Cred::bearer(token)
    };
    let mut b = Request::builder().method(method).uri(uri).cred(&cred);
    if json_body {
        b = b.header("content-type", "application/json");
    }
    if let Some(r) = range {
        b = b.header("range", r);
    }
    let r = app.clone().oneshot(b.body(body).unwrap()).await.unwrap();
    let status = r.status();
    (
        status,
        r.into_body().collect().await.unwrap().to_bytes().to_vec(),
    )
}

async fn json_call(
    app: &Router,
    method: &str,
    uri: &str,
    token: &str,
    body: serde_json::Value,
) -> (StatusCode, Vec<u8>) {
    call(
        app,
        method,
        uri,
        token,
        None,
        Body::from(body.to_string()),
        true,
    )
    .await
}

async fn sign_up(app: &Router, name: &str) -> String {
    let body = json!({"username": name, "password": PASSWORD});
    json_call(app, "POST", "/v1/register", "", body.clone()).await;
    let (_, bytes) = json_call(app, "POST", "/v1/login", "", body).await;
    serde_json::from_slice::<api::CreatedToken>(&bytes)
        .unwrap()
        .token
}

async fn upload(app: &Router, token: &str, bytes: &[u8]) -> ObjectId {
    let (status, body) = call(
        app,
        "PUT",
        "/v1/repos/alice/game/objects",
        token,
        None,
        Body::from(bytes.to_vec()),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let id = serde_json::from_slice::<api::PutObjectResponse>(&body)
        .unwrap()
        .content;
    ObjectId::from_hex(REPO_ALGORITHM, &id).unwrap()
}

type Built = (ObjectId, Vec<u8>, Vec<(ObjectId, Vec<u8>)>);

fn commit_of(files: &BTreeMap<&str, ObjectId>) -> Built {
    let tree = tree_of(
        files
            .iter()
            .map(|(p, id)| TreeChange {
                path: RepoPath::from_stored(*p).unwrap(),
                mode: FileMode::Regular,
                id: *id,
                removed: false,
            })
            .collect(),
    )
    .unwrap();
    let who = Signature {
        name: b"carol".to_vec(),
        email: b"carol@example.test".to_vec(),
        time: 1_790_000_000,
        offset_minutes: 0,
    };
    let raw = pyn_object::Commit {
        tree: tree.root,
        parents: vec![],
        author: who.clone(),
        committer: who,
        extra_headers: vec![],
        message: b"first\n".to_vec(),
    }
    .encode(Algorithm::Sha256)
    .unwrap();
    (
        hash_object(Algorithm::Sha256, Kind::Commit, &raw).unwrap(),
        raw,
        tree.trees,
    )
}

async fn assert_intact(app: &Router, token: &str, files: &[(ObjectId, &[u8])]) {
    for (id, want) in files {
        let uri = format!("/v1/repos/alice/game/objects/{}", id.to_hex());
        let (status, got) = call(app, "GET", &uri, token, None, Body::empty(), false).await;
        assert_eq!(status, StatusCode::OK);
        assert!(got == *want, "{} differs", id.to_hex());
    }
    let (id, want) = files.last().unwrap();
    let uri = format!("/v1/repos/alice/game/objects/{}", id.to_hex());
    let (status, got) = call(
        app,
        "GET",
        &uri,
        token,
        Some("bytes=1000000-2100000"),
        Body::empty(),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert!(got == want[1_000_000..=2_100_000]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pushed_repository_with_a_chunked_file_is_intact_in_the_bucket_and_back() {
    let shared = Shared {
        meta: Arc::new(MemoryMetadataStore::new()),
        access_store: Arc::new(MemoryAccessStore::new()),
        audit: Arc::new(MemoryAuditStore::new()),
    };
    let dir = tempfile::tempdir().unwrap();
    let fs: Arc<dyn ObjectStore> = Arc::new(ChunkedStore::new(Arc::new(
        FsObjectStore::open(dir.path().join("data")).await.unwrap(),
    )));
    let on_fs = app_over(fs, &shared);
    let token = sign_up(&on_fs, "alice").await;
    let (status, _) = json_call(&on_fs, "POST", "/v1/repos", &token, json!({"name": "game"})).await;
    assert_eq!(status, StatusCode::CREATED);

    let small = b"print('hello')\n".to_vec();
    let big = noise(3 << 20, 7);
    let (small_id, big_id) = (
        upload(&on_fs, &token, &small).await,
        upload(&on_fs, &token, &big).await,
    );
    let (commit, raw, trees) = commit_of(&BTreeMap::from([
        ("Source/main.py", small_id),
        ("Content/level.bin", big_id),
    ]));
    let obj =
        |id: &ObjectId, body: &[u8]| json!({"id": id.to_hex(), "data": STANDARD.encode(body)});
    let push = json!({
        "commits": [obj(&commit, &raw)],
        "trees": trees.iter().map(|(id, b)| obj(id, b)).collect::<Vec<_>>(),
    });
    let (status, reply) =
        json_call(&on_fs, "POST", "/v1/repos/alice/game/push", &token, push).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&reply)
    );
    let files = [(small_id, &small[..]), (big_id, &big[..])];
    assert_intact(&on_fs, &token, &files).await;

    let s3 = Server::start();
    let source = Arc::new(
        FsObjectStore::open_existing(dir.path().join("data"))
            .await
            .unwrap(),
    );
    let bucket = Arc::new(S3ObjectStore::open(s3.settings()).await.unwrap());
    let opts = Options {
        dry_run: false,
        verify: true,
        jobs: 4,
    };
    let report = migrate(source.clone(), bucket.clone(), &opts, &|_| {})
        .await
        .unwrap();
    assert!(report.failures.is_empty(), "{:?}", report.failures);
    let kinds: Vec<_> = {
        let mut keys = Vec::new();
        let mut after = None;
        loop {
            let page = bucket.list_keys(after.as_deref(), 1000).await.unwrap();
            keys.extend(page.keys);
            match page.next {
                Some(n) => after = Some(n),
                None => break keys,
            }
        }
    };
    assert!(
        kinds.iter().any(|k| k.kind == StoredKind::Manifest),
        "the big file has a manifest"
    );
    assert_eq!(report.copied as usize, kinds.len());

    let on_bucket = app_over(Arc::new(ChunkedStore::new(bucket.clone())), &shared);
    assert_intact(&on_bucket, &token, &files).await;

    let back_dir = dir.path().join("restored");
    let restored = Arc::new(FsObjectStore::open(&back_dir).await.unwrap());
    let report = migrate(bucket, restored.clone(), &opts, &|_| {})
        .await
        .unwrap();
    assert!(report.failures.is_empty(), "{:?}", report.failures);
    assert_eq!(report.copied as usize, kinds.len());
    let on_restored = app_over(Arc::new(ChunkedStore::new(restored)), &shared);
    assert_intact(&on_restored, &token, &files).await;
}
