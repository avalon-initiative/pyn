//! Graceful shutdown of the real server process: in-flight requests finish, lock streams end, the exit code says
//! whether anything was cut off.

use std::fs::File;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const SETUP_TOKEN: &str = "test-setup-token";

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

struct Running {
    child: Child,
    addr: String,
    dir: tempfile::TempDir,
}

impl Running {
    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    fn signal(&self, name: &str) {
        let status = Command::new("kill")
            .args([name, &self.child.id().to_string()])
            .status()
            .unwrap();
        assert!(status.success());
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("out.log")).unwrap_or_default()
    }

    /// The exit status, waiting up to `within`.
    fn exit(&mut self, within: Duration) -> std::process::ExitStatus {
        let deadline = Instant::now() + within;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "the server did not exit: {}",
                self.log()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn accepts(&self) -> bool {
        TcpStream::connect(&self.addr).is_ok()
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

async fn start(env: &[(&str, &str)]) -> Running {
    let dir = tempfile::tempdir().unwrap();
    let addr = format!("127.0.0.1:{}", free_port());
    let child = Command::new(env!("CARGO_BIN_EXE_pyn-server"))
        .env("PYN_ADDR", &addr)
        .env("PYN_SETUP_TOKEN", SETUP_TOKEN)
        .env("PYN_DEV_AUTH", "true")
        .env("PYN_PUBLIC_URL", "http://localhost:5173")
        .env_remove("PYN_DATABASE_URL")
        .env_remove("PYN_DATA_DIR")
        .env_remove("PYN_CONFIG")
        .env_remove("PYN_INSTANCES")
        .env_remove("PYN_MAILER")
        .env_remove("PYN_SHUTDOWN_GRACE_SECS")
        .envs(env.iter().copied())
        .stdin(Stdio::null())
        .stdout(File::create(dir.path().join("out.log")).unwrap())
        .stderr(File::create(dir.path().join("err.log")).unwrap())
        .spawn()
        .unwrap();
    let server = Running { child, addr, dir };
    let http = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(30);
    while http.get(server.url("/healthz")).send().await.is_err() {
        assert!(Instant::now() < deadline, "the server did not start");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let setup = serde_json::json!({
        "token": SETUP_TOKEN, "username": "root", "password": "correct horse battery", "registration": "open"
    });
    let r = http
        .post(server.url("/v1/setup"))
        .json(&setup)
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success(), "{}", r.text().await.unwrap());
    let r = http
        .post(server.url("/v1/repos"))
        .header("x-pyn-user", "root")
        .json(&serde_json::json!({"name": "r"}))
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success(), "{}", r.text().await.unwrap());
    server
}

/// An upload whose body has only begun: the request stays in flight until `finish` is called.
struct SlowUpload(TcpStream);

impl SlowUpload {
    fn begin(server: &Running) -> Self {
        let mut tcp = TcpStream::connect(&server.addr).unwrap();
        tcp.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        write!(
            tcp,
            "PUT /v1/repos/root/r/objects HTTP/1.1\r\nHost: x\r\nx-pyn-user: root\r\nContent-Length: 10\r\nConnection: close\r\n\r\nhello"
        )
        .unwrap();
        Self(tcp)
    }

    /// Sends the rest and returns the whole response.
    fn finish(mut self) -> String {
        self.0.write_all(b"world").unwrap();
        let mut out = String::new();
        self.0.read_to_string(&mut out).unwrap();
        out
    }

    /// What the server sends without the body being completed.
    fn read_to_end(mut self) -> String {
        let mut out = Vec::new();
        let _ = self.0.read_to_end(&mut out);
        String::from_utf8_lossy(&out).into_owned()
    }
}

#[tokio::test]
async fn sigterm_lets_a_slow_request_finish_then_exits_zero_and_refuses_connections() {
    let mut server = start(&[]).await;
    let upload = SlowUpload::begin(&server);
    tokio::time::sleep(Duration::from_millis(300)).await;
    server.signal("-TERM");

    let deadline = Instant::now() + Duration::from_secs(5);
    while server.accepts() {
        assert!(Instant::now() < deadline, "the listener stayed open");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        server.child.try_wait().unwrap().is_none(),
        "exited with a request in flight"
    );

    let response = upload.finish();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.contains("\"content\":\""), "{response}");

    let status = server.exit(Duration::from_secs(10));
    assert_eq!(status.code(), Some(0), "{}", server.log());
    assert!(!server.accepts());
    assert!(server.log().contains("shutting down"), "{}", server.log());
}

#[tokio::test]
async fn ctrl_c_stops_an_idle_server_with_exit_zero() {
    let mut server = start(&[]).await;
    server.signal("-INT");
    assert_eq!(server.exit(Duration::from_secs(10)).code(), Some(0));
}

#[tokio::test]
async fn a_lock_stream_ends_on_drain_instead_of_holding_the_shutdown() {
    let mut server = start(&[("PYN_SHUTDOWN_GRACE_SECS", "60")]).await;
    let http = reqwest::Client::new();
    let head: pyn_proto::LockStreamPage = http
        .get(server.url("/v1/repos/root/r/locks/events"))
        .header("x-pyn-user", "root")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let poll = tokio::spawn({
        let url = server.url(&format!(
            "/v1/repos/root/r/locks/events?after={}&wait=25",
            head.cursor
        ));
        let http = http.clone();
        async move { http.get(url).header("x-pyn-user", "root").send().await }
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!poll.is_finished());
    let signalled = Instant::now();
    server.signal("-TERM");

    let reply = poll.await.unwrap().unwrap();
    assert!(reply.status().is_success());
    let page: pyn_proto::LockStreamPage = reply.json().await.unwrap();
    assert!(page.events.is_empty());
    assert!(
        signalled.elapsed() < Duration::from_secs(5),
        "the stream held on"
    );
    assert_eq!(
        server.exit(Duration::from_secs(10)).code(),
        Some(0),
        "{}",
        server.log()
    );
}

#[tokio::test]
async fn when_the_grace_runs_out_the_exit_is_non_zero_and_names_the_cut_off() {
    let mut server = start(&[("PYN_SHUTDOWN_GRACE_SECS", "1")]).await;
    let upload = SlowUpload::begin(&server);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let signalled = Instant::now();
    server.signal("-TERM");
    let status = server.exit(Duration::from_secs(10));
    assert_eq!(status.code(), Some(1), "{}", server.log());
    assert!(signalled.elapsed() >= Duration::from_millis(900));
    let log = server.log();
    assert!(
        log.contains("1 request(s) were still running and are cut off"),
        "{log}"
    );
    assert!(!upload.read_to_end().starts_with("HTTP/1.1 200"));
}

#[tokio::test]
async fn a_second_signal_exits_without_waiting_for_the_grace() {
    let mut server = start(&[("PYN_SHUTDOWN_GRACE_SECS", "60")]).await;
    let _upload = SlowUpload::begin(&server);
    tokio::time::sleep(Duration::from_millis(300)).await;
    server.signal("-TERM");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(server.child.try_wait().unwrap().is_none());
    server.signal("-TERM");
    assert_eq!(
        server.exit(Duration::from_secs(5)).code(),
        Some(1),
        "{}",
        server.log()
    );
}

#[tokio::test]
async fn no_grace_cuts_a_running_request_at_once_and_an_idle_server_exits_zero() {
    let mut busy = start(&[("PYN_SHUTDOWN_GRACE_SECS", "0")]).await;
    let _upload = SlowUpload::begin(&busy);
    tokio::time::sleep(Duration::from_millis(300)).await;
    busy.signal("-TERM");
    assert_eq!(
        busy.exit(Duration::from_secs(5)).code(),
        Some(1),
        "{}",
        busy.log()
    );

    let mut idle = start(&[("PYN_SHUTDOWN_GRACE_SECS", "0")]).await;
    idle.signal("-TERM");
    assert_eq!(
        idle.exit(Duration::from_secs(5)).code(),
        Some(0),
        "{}",
        idle.log()
    );
}

#[test]
fn a_bad_grace_value_stops_the_server_at_start() {
    let out = Command::new(env!("CARGO_BIN_EXE_pyn-server"))
        .env("PYN_ADDR", format!("127.0.0.1:{}", free_port()))
        .env("PYN_SHUTDOWN_GRACE_SECS", "soon")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("PYN_SHUTDOWN_GRACE_SECS"));
}
