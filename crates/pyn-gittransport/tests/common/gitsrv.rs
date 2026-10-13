//! Real git as an oracle server, for tests only: bare repositories, an ssh shim, and `git http-backend` behind axum.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::response::Response;

pub const FORMATS: [&str; 2] = ["sha1", "sha256"];

pub fn git(dir: &Path) -> Command {
    let mut c = Command::new("git");
    c.current_dir(dir)
        .args([
            "-c",
            "gc.auto=0",
            "-c",
            "maintenance.auto=false",
            "-c",
            "init.defaultBranch=main",
            "-c",
            "advice.detachedHead=false",
            "-c",
            "protocol.file.allow=always",
        ])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com");
    c
}

pub fn run(dir: &Path, args: &[&str]) -> String {
    let out = git(dir).args(args).output().unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

pub fn init(path: &Path, format: &str, bare: bool) {
    fs::create_dir_all(path).unwrap();
    let mut args = vec!["init", "-q", "--object-format", format];
    if bare {
        args.push("--bare");
    }
    run(path, &args);
    if bare {
        run(path, &["config", "http.receivepack", "true"]);
        run(path, &["config", "receive.denyCurrentBranch", "ignore"]);
    }
}

pub fn commit(dir: &Path, file: &str, content: &[u8], msg: &str) -> String {
    fs::write(dir.join(file), content).unwrap();
    run(dir, &["add", file]);
    run(dir, &["commit", "-q", "-m", msg]);
    run(dir, &["rev-parse", "HEAD"]).trim().to_owned()
}

/// A work repository with history on `main`, a branch, a lightweight tag and an annotated tag.
pub fn sample_work(path: &Path, format: &str) {
    init(path, format, false);
    commit(path, "a.txt", b"one\n", "one");
    commit(path, "b.txt", b"two\n", "two");
    run(path, &["tag", "v1"]);
    run(path, &["tag", "-a", "v2", "-m", "second"]);
    run(path, &["switch", "-q", "-c", "feature"]);
    commit(path, "c.txt", b"three\n", "three");
    run(path, &["switch", "-q", "main"]);
}

/// A bare repository holding `sample_work`'s refs.
pub fn sample_bare(root: &Path, format: &str) -> PathBuf {
    let work = root.join("seed");
    sample_work(&work, format);
    let bare = root.join("served.git");
    init(&bare, format, true);
    run(&work, &["push", "-q", bare.to_str().unwrap(), "--all"]);
    run(&work, &["push", "-q", bare.to_str().unwrap(), "--tags"]);
    run(&bare, &["symbolic-ref", "HEAD", "refs/heads/main"]);
    bare
}

/// `git ls-remote` of a local path: name to id, peeled entries included.
pub fn oracle_refs(path: &Path, extra: &[&str]) -> BTreeMap<String, String> {
    let mut args = vec!["ls-remote"];
    args.extend_from_slice(extra);
    args.push(path.to_str().unwrap());
    run(Path::new("/"), &args)
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .map(|(id, name)| (name.to_owned(), id.to_owned()))
        .collect()
}

pub fn fsck(dir: &Path) {
    let out = git(dir)
        .args(["fsck", "--strict", "--full"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git fsck failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

pub fn path_env() -> String {
    std::env::var("PATH").unwrap_or_default()
}

/// A script that acts as the ssh client: it logs what it was given, ignores the host and runs the remote command here.
pub fn ssh_shim(dir: &Path, strip_protocol: bool) -> PathBuf {
    let script = dir.join(if strip_protocol { "ssh-v0" } else { "ssh-shim" });
    let log = dir.join("ssh.log");
    let body = format!(
        r#"#!/bin/sh
for a in "$@"; do printf 'ARG %s\n' "$a" >> '{log}'; done
printf 'GIT_PROTOCOL=%s\n' "${{GIT_PROTOCOL-unset}}" >> '{log}'
while [ $# -gt 0 ]; do
  case "$1" in
    -o|-p|-P) shift 2;;
    --|-batch) shift;;
    *) break;;
  esac
done
cmd="$2"
{strip}
PATH="$(git --exec-path):$PATH"; export PATH
exec sh -c "$cmd"
"#,
        log = log.display(),
        strip = if strip_protocol {
            "unset GIT_PROTOCOL"
        } else {
            ""
        }
    );
    fs::write(&script, body).unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    script
}

pub fn ssh_log(dir: &Path) -> String {
    fs::read_to_string(dir.join("ssh.log")).unwrap_or_default()
}

#[derive(Debug, Clone, Default)]
pub struct Seen {
    pub method: String,
    pub target: String,
    pub headers: BTreeMap<String, String>,
    pub status: u16,
}

#[derive(Default)]
pub struct Opts {
    pub auth: Option<(String, String)>,
    /// Hide `Git-Protocol` from the backend, as a server that predates v2 does.
    pub strip_protocol: bool,
    /// Answer `info/refs` as a dumb server would.
    pub dumb: bool,
    /// Redirect `/moved/...` to this base URL.
    pub redirect_to: Option<String>,
}

struct App {
    root: PathBuf,
    opts: Opts,
    log: Mutex<Vec<Seen>>,
}

pub struct Server {
    pub url: String,
    app: Arc<App>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Server {
    pub fn start(root: &Path, opts: Opts) -> Self {
        let app = Arc::new(App {
            root: root.to_path_buf(),
            opts,
            log: Mutex::new(Vec::new()),
        });
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let router = Router::new().fallback(handle).with_state(app.clone());
        let thread = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                let l = tokio::net::TcpListener::from_std(listener).unwrap();
                axum::serve(l, router)
                    .with_graceful_shutdown(async {
                        let _ = rx.await;
                    })
                    .await
                    .unwrap();
            });
        });
        Self {
            url: format!("http://127.0.0.1:{port}"),
            app,
            shutdown: Some(tx),
            thread: Some(thread),
        }
    }

    pub fn log(&self) -> Vec<Seen> {
        self.app.log.lock().unwrap().clone()
    }

    pub fn clear_log(&self) {
        self.app.log.lock().unwrap().clear();
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

pub fn basic(user: &str, pass: &str) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let data = format!("{user}:{pass}").into_bytes();
    let mut out = String::from("Basic ");
    for c in data.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= c.len() {
                out.push(T[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn headers_of(h: &HeaderMap) -> BTreeMap<String, String> {
    h.iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_owned()))
        .collect()
}

fn reply(status: u16, headers: &[(&str, String)], body: Vec<u8>) -> Response {
    let mut r = Response::builder().status(StatusCode::from_u16(status).unwrap());
    for (k, v) in headers {
        r = r.header(*k, v);
    }
    r.body(Body::from(body)).unwrap()
}

async fn handle(State(app): State<Arc<App>>, req: Request<Body>) -> Response {
    let (parts, body) = req.into_parts();
    let target = parts
        .uri
        .path_and_query()
        .map(|p| p.as_str().to_owned())
        .unwrap_or_default();
    let mut seen = Seen {
        method: parts.method.to_string(),
        target: target.clone(),
        headers: headers_of(&parts.headers),
        status: 0,
    };
    let body = axum::body::to_bytes(body, usize::MAX)
        .await
        .unwrap()
        .to_vec();
    let resp = respond(&app, &parts, &target, body).await;
    seen.status = resp.status().as_u16();
    app.log.lock().unwrap().push(seen);
    resp
}

async fn respond(
    app: &Arc<App>,
    parts: &axum::http::request::Parts,
    target: &str,
    body: Vec<u8>,
) -> Response {
    if let Some((u, p)) = &app.opts.auth {
        let ok = parts
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            == Some(basic(u, p).as_str());
        if !ok {
            return reply(
                401,
                &[("WWW-Authenticate", "Basic realm=\"pyn test\"".into())],
                b"auth".to_vec(),
            );
        }
    }
    if let (Some(base), Some(rest)) = (&app.opts.redirect_to, target.strip_prefix("/moved")) {
        return reply(301, &[("Location", format!("{base}{rest}"))], Vec::new());
    }
    if app.opts.dumb && target.contains("/info/refs") {
        return reply(
            200,
            &[("Content-Type", "text/plain".into())],
            b"deadbeef refs/heads/main\n".to_vec(),
        );
    }
    let app = app.clone();
    let parts = parts.clone();
    tokio::task::spawn_blocking(move || cgi(&app, &parts, body))
        .await
        .unwrap()
}

fn cgi(app: &App, parts: &axum::http::request::Parts, body: Vec<u8>) -> Response {
    use std::io::Write;
    let h = &parts.headers;
    let get = |n: &str| h.get(n).and_then(|v| v.to_str().ok()).unwrap_or("");
    let mut cmd = Command::new("git");
    cmd.arg("http-backend")
        .env_clear()
        .env("PATH", path_env())
        .env("GIT_PROJECT_ROOT", &app.root)
        .env("GIT_HTTP_EXPORT_ALL", "1")
        .env("PATH_INFO", parts.uri.path())
        .env("QUERY_STRING", parts.uri.query().unwrap_or(""))
        .env("REQUEST_METHOD", parts.method.as_str())
        .env("CONTENT_TYPE", get("content-type"))
        .env("CONTENT_LENGTH", body.len().to_string())
        .env("REMOTE_USER", "tester")
        .env("REMOTE_ADDR", "127.0.0.1")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if !app.opts.strip_protocol && !get("git-protocol").is_empty() {
        cmd.env("HTTP_GIT_PROTOCOL", get("git-protocol"));
    }
    if !get("content-encoding").is_empty() {
        cmd.env("HTTP_CONTENT_ENCODING", get("content-encoding"));
    }
    let mut child = cmd.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&body);
    });
    let out = child.wait_with_output().unwrap();
    let _ = writer.join();
    let split = out
        .stdout
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .unwrap_or(out.stdout.len());
    let head = String::from_utf8_lossy(&out.stdout[..split]).into_owned();
    let payload = out.stdout.get(split + 4..).unwrap_or(&[]).to_vec();
    let mut status = 200;
    let mut headers = Vec::new();
    for line in head.lines() {
        if let Some((k, v)) = line.split_once(": ") {
            if k.eq_ignore_ascii_case("status") {
                status = v.split_whitespace().next().unwrap().parse().unwrap();
            } else {
                headers.push((k.to_owned(), v.to_owned()));
            }
        }
    }
    let mut r = Response::builder().status(StatusCode::from_u16(status).unwrap());
    for (k, v) in headers {
        r = r.header(k, v);
    }
    r.body(Body::from(payload)).unwrap()
}
