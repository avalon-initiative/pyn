//! The S3 object store selected by the environment, run as the real server against the in-process S3 server:
//! start-up refusals, the start-up probe, the health report and the `_FILE` variants.

use std::fs::File;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use pyn_proto as api;
use pyn_s3::testing::{ACCESS_KEY, BUCKET, SECRET_KEY, Server};
use pyn_s3::{S3ObjectStore, S3Settings};
use pyn_server::storage::{StorageReport, spawn_monitor};

const SETUP_TOKEN: &str = "test-setup-token";
const PASSWORD: &str = "correct horse battery";

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn s3_env(s3: &Server) -> Vec<(String, String)> {
    [
        ("PYN_S3_ENDPOINT", format!("http://{}", s3.addr)),
        ("PYN_S3_BUCKET", BUCKET.into()),
        ("PYN_S3_PATH_STYLE", "true".into()),
        ("PYN_S3_ACCESS_KEY_ID", ACCESS_KEY.into()),
        ("PYN_S3_SECRET_ACCESS_KEY", SECRET_KEY.into()),
        ("PYN_S3_MAX_RETRIES", "0".into()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect()
}

fn with(mut env: Vec<(String, String)>, name: &str, value: &str) -> Vec<(String, String)> {
    env.retain(|(k, _)| k != name);
    env.push((name.into(), value.into()));
    env
}

fn without(mut env: Vec<(String, String)>, name: &str) -> Vec<(String, String)> {
    env.retain(|(k, _)| k != name);
    env
}

struct Running {
    child: Child,
    url: String,
    dir: tempfile::TempDir,
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn(addr: &str, env: &[(String, String)], dir: &std::path::Path) -> Child {
    Command::new(env!("CARGO_BIN_EXE_pyn-server"))
        .env("PYN_ADDR", addr)
        .env("PYN_SETUP_TOKEN", SETUP_TOKEN)
        .env("PYN_REGISTRATION", "open")
        .env("PYN_DEV_AUTH", "true")
        .env("PYN_PUBLIC_URL", "http://localhost:5173")
        .env_remove("PYN_DATABASE_URL")
        .env_remove("PYN_DATA_DIR")
        .env_remove("PYN_CONFIG")
        .env_remove("PYN_INSTANCES")
        .env_remove("PYN_MAILER")
        .envs(env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
        .stdout(File::create(dir.join("out.log")).unwrap())
        .stderr(File::create(dir.join("err.log")).unwrap())
        .spawn()
        .unwrap()
}

/// Starts the server and sets it up.
async fn start(env: &[(String, String)]) -> Running {
    let dir = tempfile::tempdir().unwrap();
    let addr = format!("127.0.0.1:{}", free_port());
    let child = spawn(&addr, env, dir.path());
    let server = Running {
        child,
        url: format!("http://{addr}"),
        dir,
    };
    let http = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    while http
        .get(format!("{}/healthz", server.url))
        .send()
        .await
        .is_err()
    {
        assert!(Instant::now() < deadline, "the server did not start");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let setup = serde_json::json!({
        "token": SETUP_TOKEN, "username": "root", "password": PASSWORD, "registration": "open"
    });
    let r = http
        .post(format!("{}/v1/setup", server.url))
        .json(&setup)
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success(), "{}", r.text().await.unwrap());
    server
}

async fn storage_of(s: &Running) -> api::StorageSettings {
    let r = reqwest::Client::new()
        .get(format!("{}/v1/admin/settings", s.url))
        .header("x-pyn-user", "root")
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success());
    r.json::<api::AdminSettings>().await.unwrap().storage
}

/// Runs the server to its first exit and returns (success, standard error).
fn exits(env: &[(String, String)]) -> (bool, String) {
    let dir = tempfile::tempdir().unwrap();
    let addr = format!("127.0.0.1:{}", free_port());
    let mut child = spawn(&addr, env, dir.path());
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    (
        status.is_some_and(|s| s.success()),
        std::fs::read_to_string(dir.path().join("err.log")).unwrap(),
    )
}

fn refused(env: &[(String, String)]) -> String {
    let (ok, err) = exits(env);
    assert!(!ok, "the server should have refused to start: {err}");
    assert!(!err.contains(SECRET_KEY), "the secret was echoed: {err}");
    err
}

#[tokio::test]
async fn the_bucket_selects_s3_and_the_report_names_the_kind_and_bucket_only() {
    let s3 = Server::start();
    let server = start(&s3_env(&s3)).await;
    let report = storage_of(&server).await;
    assert_eq!(report.kind, api::StorageKind::S3);
    assert_eq!(report.bucket.as_deref(), Some(BUCKET));
    assert_eq!(report.status, api::StorageStatus::Ok);

    let shown = serde_json::to_string(&report).unwrap();
    for secret in [SECRET_KEY, ACCESS_KEY, "127.0.0.1"] {
        assert!(!shown.contains(secret), "{shown}");
    }
    let log = std::fs::read_to_string(server.dir.path().join("out.log")).unwrap();
    assert!(!log.contains(SECRET_KEY), "{log}");
    let probe_left = s3.root.join(BUCKET).join(".pyn-probe");
    assert_eq!(std::fs::read_dir(probe_left).map_or(0, |d| d.count()), 0);
}

#[tokio::test]
async fn without_a_bucket_the_report_says_memory() {
    let server = start(&[]).await;
    let report = storage_of(&server).await;
    assert_eq!(report.kind, api::StorageKind::Memory);
    assert_eq!(report.bucket, None);
}

#[tokio::test]
async fn the_file_variants_supply_the_credentials() {
    let s3 = Server::start();
    let dir = tempfile::tempdir().unwrap();
    let (id, key) = (dir.path().join("id"), dir.path().join("key"));
    std::fs::write(&id, format!("{ACCESS_KEY}\n")).unwrap();
    std::fs::write(&key, format!("{SECRET_KEY}\n")).unwrap();
    let env = without(
        without(s3_env(&s3), "PYN_S3_ACCESS_KEY_ID"),
        "PYN_S3_SECRET_ACCESS_KEY",
    );
    let env = with(env, "PYN_S3_ACCESS_KEY_ID_FILE", id.to_str().unwrap());
    let env = with(env, "PYN_S3_SECRET_ACCESS_KEY_FILE", key.to_str().unwrap());
    let server = start(&env).await;
    assert_eq!(storage_of(&server).await.status, api::StorageStatus::Ok);

    let both = with(
        s3_env(&s3),
        "PYN_S3_SECRET_ACCESS_KEY_FILE",
        key.to_str().unwrap(),
    );
    let err = refused(&both);
    assert!(
        err.contains("PYN_S3_SECRET_ACCESS_KEY_FILE") && err.contains("both set"),
        "{err}"
    );

    let gone = dir.path().join("gone");
    let env = with(
        without(s3_env(&s3), "PYN_S3_SECRET_ACCESS_KEY"),
        "PYN_S3_SECRET_ACCESS_KEY_FILE",
        gone.to_str().unwrap(),
    );
    let err = refused(&env);
    assert!(
        err.contains("PYN_S3_SECRET_ACCESS_KEY_FILE") && err.contains("gone"),
        "{err}"
    );
}

#[test]
fn conflicting_and_incomplete_settings_stop_the_server_without_echoing_the_secret() {
    let s3 = Server::start();
    let both = with(s3_env(&s3), "PYN_DATA_DIR", "/tmp/pyn-data");
    let err = refused(&both);
    assert!(
        err.contains("PYN_S3_BUCKET") && err.contains("PYN_DATA_DIR"),
        "{err}"
    );

    for missing in [
        "PYN_S3_ENDPOINT",
        "PYN_S3_ACCESS_KEY_ID",
        "PYN_S3_SECRET_ACCESS_KEY",
    ] {
        let err = refused(&without(s3_env(&s3), missing));
        assert!(err.contains(missing), "{err}");
    }

    let stray = without(s3_env(&s3), "PYN_S3_BUCKET");
    let err = refused(&stray);
    assert!(err.contains("without PYN_S3_BUCKET"), "{err}");

    let userinfo = with(
        s3_env(&s3),
        "PYN_S3_ENDPOINT",
        &format!("http://admin:{SECRET_KEY}@{}", s3.addr),
    );
    let err = refused(&userinfo);
    assert!(err.contains("PYN_S3_ENDPOINT"), "{err}");
}

#[test]
fn the_start_up_probe_fails_clearly_on_a_wrong_secret_or_an_unreachable_endpoint() {
    let s3 = Server::start();
    let wrong = with(
        s3_env(&s3),
        "PYN_S3_SECRET_ACCESS_KEY",
        "a-wrong-secret-value",
    );
    let (ok, err) = exits(&wrong);
    assert!(!ok);
    assert!(
        err.contains("start-up check") && err.contains(BUCKET),
        "{err}"
    );
    assert!(err.contains("PutObject") && err.contains("403"), "{err}");
    assert!(!err.contains("a-wrong-secret-value"), "{err}");

    let down = with(
        s3_env(&s3),
        "PYN_S3_ENDPOINT",
        &format!("http://127.0.0.1:{}", free_port()),
    );
    let err = refused(&down);
    assert!(
        err.contains("start-up check") && err.contains("PutObject"),
        "{err}"
    );
}

#[test]
fn several_copies_accept_a_bucket_but_still_need_a_database() {
    let s3 = Server::start();
    let env = with(s3_env(&s3), "PYN_INSTANCES", "2");
    let err = refused(&env);
    assert!(err.contains("PYN_DATABASE_URL"), "{err}");
    assert!(err.contains("PYN_S3_BUCKET"), "{err}");
    let none = refused(&[("PYN_INSTANCES".into(), "2".into())]);
    assert!(none.contains("PYN_DATABASE_URL"), "{none}");
}

#[tokio::test]
async fn an_unreachable_store_turns_the_report_degraded_without_stopping_anything() {
    let s3 = Server::start();
    let settings: S3Settings = s3.settings();
    let store = Arc::new(S3ObjectStore::open(settings).await.unwrap());
    let report = Arc::new(StorageReport::new(
        api::StorageKind::S3,
        Some(BUCKET.into()),
    ));
    spawn_monitor(store, report.clone(), Duration::from_millis(50));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(report.settings().status, api::StorageStatus::Ok);

    drop(s3);
    let deadline = Instant::now() + Duration::from_secs(20);
    while report.settings().status == api::StorageStatus::Ok {
        assert!(
            Instant::now() < deadline,
            "the store never reported degraded"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
