//! The mailer setting, run as the real server: what `PYN_MAILER` turns on, what the log sender writes,
//! and the start-up refusals.

use std::fs::File;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use pyn_proto as api;

const PASSWORD: &str = "correct horse battery";
const SETUP_TOKEN: &str = "test-setup-token";

struct Running {
    child: Child,
    url: String,
    log: PathBuf,
    _dir: tempfile::TempDir,
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn command(addr: &str, env: &[(&str, &str)], out: File, err: File) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_pyn-server"));
    command
        .env("PYN_ADDR", addr)
        .env("PYN_SETUP_TOKEN", SETUP_TOKEN)
        .env("PYN_REGISTRATION", "open")
        .env("PYN_PUBLIC_URL", "http://localhost:5173")
        .env_remove("PYN_DATABASE_URL")
        .env_remove("PYN_DATA_DIR")
        .env_remove("PYN_CONFIG")
        .env_remove("PYN_MAILER")
        .env_remove("PYN_EMAIL_VERIFICATION")
        .env_remove("PYN_MAILER_LOG_ALLOW_NON_LOOPBACK")
        .envs(env.iter().copied())
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err);
    command
}

/// Starts the server, waits until it answers, and sets it up with open registration.
async fn start(env: &[(&str, &str)]) -> Running {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("out.log");
    let addr = format!("127.0.0.1:{}", free_port());
    let child = command(
        &addr,
        env,
        File::create(&log).unwrap(),
        File::create(dir.path().join("err.log")).unwrap(),
    )
    .spawn()
    .unwrap();
    let server = Running {
        child,
        url: format!("http://{addr}"),
        log,
        _dir: dir,
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
    let r = post(&server, "/v1/setup", &setup).await;
    assert!(r.0.is_success(), "{r:?}");
    server
}

async fn post(s: &Running, path: &str, body: &serde_json::Value) -> (reqwest::StatusCode, String) {
    let r = reqwest::Client::new()
        .post(format!("{}{path}", s.url))
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    (r.status(), r.text().await.unwrap())
}

async fn registration(s: &Running) -> api::RegistrationInfo {
    let text = reqwest::get(format!("{}/v1/registration", s.url))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    serde_json::from_str(&text).unwrap()
}

fn log_text(s: &Running) -> String {
    std::fs::read_to_string(&s.log).unwrap()
}

/// Runs the server to its first exit and returns (success, standard error).
fn exits(addr: &str, env: &[(&str, &str)]) -> (bool, String) {
    let dir = tempfile::tempdir().unwrap();
    let err_path = dir.path().join("err.log");
    let mut child = command(
        addr,
        env,
        File::create(dir.path().join("out.log")).unwrap(),
        File::create(&err_path).unwrap(),
    )
    .spawn()
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
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
        std::fs::read_to_string(err_path).unwrap(),
    )
}

#[tokio::test]
async fn the_log_sender_carries_the_verification_flow_end_to_end() {
    let s = start(&[("PYN_MAILER", "log")]).await;
    assert!(registration(&s).await.email_verification);
    let up =
        serde_json::json!({"username": "maya", "password": PASSWORD, "email": "maya@example.org"});
    let (status, body) = post(&s, "/v1/register", &up).await;
    assert_eq!(status, 201, "{body}");
    assert!(body.contains("pending_verification"), "{body}");

    let log = log_text(&s);
    assert!(log.contains("PYN-MAILER-LOG"), "{log}");
    assert!(log.contains("To: maya@example.org"), "{log}");
    assert!(log.contains("Subject: Confirm your email address"), "{log}");
    assert!(
        log.contains("PYN_MAILER=log is on"),
        "the start-up warning: {log}"
    );
    let (_, rest) = log
        .split_once("http://localhost:5173/verify-email?token=")
        .expect("the link in the log");
    let token = rest.split_whitespace().next().unwrap();

    let login = serde_json::json!({"username": "maya", "password": PASSWORD});
    let (status, _) = post(&s, "/v1/login", &login).await;
    assert_eq!(status, 403, "not active before the link is followed");
    let (status, body) = post(
        &s,
        "/v1/register/verify",
        &serde_json::json!({"token": token}),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let (status, body) = post(&s, "/v1/login", &login).await;
    assert_eq!(status, 200, "{body}");
}

#[tokio::test]
async fn without_a_mailer_sign_up_needs_no_address() {
    let s = start(&[]).await;
    let info = registration(&s).await;
    assert_eq!(info.registration, "open");
    assert!(!info.email_verification);
    let up = serde_json::json!({"username": "maya", "password": PASSWORD});
    let (status, body) = post(&s, "/v1/register", &up).await;
    assert_eq!(status, 201, "{body}");
    assert!(body.contains("\"active\""), "{body}");
    let (status, body) = post(&s, "/v1/login", &up).await;
    assert_eq!(status, 200, "{body}");
    assert!(!log_text(&s).contains("PYN-MAILER-LOG"));
}

#[tokio::test]
async fn an_explicit_setting_still_wins_over_the_mailer() {
    let s = start(&[("PYN_MAILER", "log"), ("PYN_EMAIL_VERIFICATION", "false")]).await;
    assert!(!registration(&s).await.email_verification);
}

#[test]
fn verification_forced_on_without_a_mailer_is_refused_at_start() {
    let port = free_port();
    let addr = format!("127.0.0.1:{port}");
    let (ok, err) = exits(&addr, &[("PYN_EMAIL_VERIFICATION", "true")]);
    assert!(!ok);
    assert!(
        err.contains("PYN_EMAIL_VERIFICATION=true needs a mailer"),
        "{err}"
    );
}

#[test]
fn the_log_sender_is_refused_beyond_loopback_unless_acknowledged() {
    let addr = format!("0.0.0.0:{}", free_port());
    let (ok, err) = exits(&addr, &[("PYN_MAILER", "log")]);
    assert!(!ok);
    assert!(err.contains("PYN_MAILER_LOG_ALLOW_NON_LOOPBACK"), "{err}");
    let (ok, err) = exits(
        &addr,
        &[
            ("PYN_MAILER", "log"),
            ("PYN_MAILER_LOG_ALLOW_NON_LOOPBACK", "true"),
        ],
    );
    assert!(!ok, "it keeps running, so the wait ends in a kill: {err}");
    assert!(!err.contains("PYN_MAILER_LOG_ALLOW_NON_LOOPBACK"), "{err}");
}

#[test]
fn incomplete_smtp_settings_are_refused_without_echoing_the_password() {
    let addr = format!("127.0.0.1:{}", free_port());
    let (ok, err) = exits(
        &addr,
        &[
            ("PYN_MAILER", "smtp"),
            ("PYN_SMTP_USER", "mailer"),
            ("PYN_SMTP_PASSWORD", "hunter2-secret"),
        ],
    );
    assert!(!ok);
    assert!(err.contains("PYN_SMTP_HOST"), "{err}");
    assert!(!err.contains("hunter2-secret"), "{err}");
}
