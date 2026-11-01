//! Drives stock `git` / `git lfs` against a live in-process server, with a request trace and a hard per-command timeout.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::{Request, State};
use axum::http::header::{AUTHORIZATION, CONTENT_LENGTH, EXPECT, TRANSFER_ENCODING};
use axum::middleware::{Next, from_fn_with_state};
use axum::response::Response;

pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(120);

pub type Trace = Arc<Mutex<Vec<String>>>;

fn header(req: &Request, name: impl axum::http::header::AsHeaderName) -> String {
    req.headers()
        .get(name)
        .map_or("-".into(), |v| v.to_str().unwrap_or("?").to_string())
}

/// The path with 64-digit hex segments (object ids) cut to eight digits; the query is dropped.
fn short_path(path: &str) -> String {
    path.split('/')
        .map(|s| {
            if s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()) {
                format!("{}..", &s[..8])
            } else {
                s.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

async fn record(State(trace): State<Trace>, req: Request, next: Next) -> Response {
    let scheme = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map_or("none", |v| v.split(' ').next().unwrap_or("?"))
        .to_string();
    let line = format!(
        "{} {} auth={scheme} content-length={} transfer-encoding={} expect={}",
        req.method(),
        short_path(req.uri().path()),
        header(&req, CONTENT_LENGTH),
        header(&req, TRANSFER_ENCODING),
        header(&req, EXPECT),
    );
    trace.lock().unwrap().push(format!("-> {line}"));
    let start = Instant::now();
    let res = next.run(req).await;
    trace.lock().unwrap().push(format!(
        "<- {line} status={} {}ms",
        res.status().as_u16(),
        start.elapsed().as_millis()
    ));
    res
}

/// Serves `app` on a loopback port behind the recording middleware.
pub async fn serve(app: Router) -> (SocketAddr, Trace) {
    let trace = Trace::default();
    let app = app.layer(from_fn_with_state(trace.clone(), record));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (addr, trace)
}

pub struct Git {
    pub dir: PathBuf,
    pub trace: Trace,
}

impl Git {
    /// Runs `git args`, returning stdout and stderr; panics with the server trace on failure or timeout.
    pub fn run(&self, args: &[&str]) -> String {
        let (ok, out) = self.try_run(args);
        assert!(ok, "git {args:?} failed:\n{out}\n{}", self.trace_text());
        out
    }

    /// Like `run`, but returns whether the command succeeded; still panics on timeout.
    pub fn try_run(&self, args: &[&str]) -> (bool, String) {
        let (out_f, err_f) = (tempfile::tempfile().unwrap(), tempfile::tempfile().unwrap());
        let mut child = Command::new("git")
            .args(args)
            .current_dir(&self.dir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .stdin(Stdio::null())
            .stdout(Stdio::from(out_f.try_clone().unwrap()))
            .stderr(Stdio::from(err_f.try_clone().unwrap()))
            .spawn()
            .unwrap();
        let deadline = Instant::now() + COMMAND_TIMEOUT;
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break Some(status);
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        let text = |mut f: std::fs::File| {
            use std::io::{Read, Seek, SeekFrom};
            let mut s = String::new();
            f.seek(SeekFrom::Start(0)).unwrap();
            f.read_to_string(&mut s).ok();
            s
        };
        let out = format!(
            "stdout:\n{}\nstderr:\n{}",
            text(out_f).trim_end(),
            text(err_f).trim_end()
        );
        match status {
            Some(status) => (status.success(), out),
            None => panic!(
                "git {args:?} timed out after {COMMAND_TIMEOUT:?}:\n{out}\n{}",
                self.trace_text()
            ),
        }
    }

    pub fn trace_text(&self) -> String {
        format!("server trace:\n{}", self.trace.lock().unwrap().join("\n"))
    }
}
