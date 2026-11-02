//! A local Git LFS Locks API server for tests: users by Basic authentication, locks in memory, paging, a request log
//! with bodies and switches for `verify` support.
#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::{Request, State};
use axum::http::{Method, header};
use axum::response::Response;
use base64::Engine as _;
use serde_json::{Value, json};

#[derive(Clone)]
pub struct StoredLock {
    pub id: String,
    pub path: String,
    pub owner: String,
}

#[derive(Default)]
pub struct Inner {
    pub locks: Vec<StoredLock>,
    pub next_id: u32,
    /// `METHOD path?query` per request.
    pub log: Vec<String>,
    /// The JSON body of each POST, in order.
    pub bodies: Vec<Value>,
    /// Known `user` / `password` pairs; empty means every request is anonymous.
    pub users: Vec<(String, String)>,
    /// Answers `verify` with this status.
    pub verify_status: Option<u16>,
    /// Locks per page, 0 for everything.
    pub page: usize,
}

pub struct LocksServer {
    pub addr: SocketAddr,
    pub state: Arc<Mutex<Inner>>,
    _rt: tokio::runtime::Runtime,
}

impl LocksServer {
    pub fn start() -> Self {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let state = Arc::new(Mutex::new(Inner::default()));
        let app = Router::new().fallback(handle).with_state(state.clone());
        let addr = rt.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            addr
        });
        Self {
            addr,
            state,
            _rt: rt,
        }
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    pub fn url_as(&self, user: &str, password: &str, path: &str) -> String {
        format!("http://{user}:{password}@{}{path}", self.addr)
    }

    pub fn add_user(&self, user: &str, password: &str) {
        self.state
            .lock()
            .unwrap()
            .users
            .push((user.into(), password.into()));
    }

    pub fn lock_for(&self, user: &str, path: &str) -> String {
        let mut st = self.state.lock().unwrap();
        st.next_id += 1;
        let id = format!("lock{}", st.next_id);
        st.locks.push(StoredLock {
            id: id.clone(),
            path: path.into(),
            owner: user.into(),
        });
        id
    }

    pub fn held(&self) -> Vec<(String, String)> {
        let st = self.state.lock().unwrap();
        st.locks
            .iter()
            .map(|l| (l.owner.clone(), l.path.clone()))
            .collect()
    }

    pub fn log(&self) -> Vec<String> {
        self.state.lock().unwrap().log.clone()
    }

    pub fn count(&self, prefix: &str) -> usize {
        self.log().iter().filter(|l| l.starts_with(prefix)).count()
    }

    pub fn bodies(&self) -> Vec<Value> {
        self.state.lock().unwrap().bodies.clone()
    }
}

fn reply(status: u16, body: Value) -> Response {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/vnd.git-lfs+json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn wire(l: &StoredLock) -> Value {
    json!({"id": l.id, "path": l.path, "locked_at": "2026-01-02T03:04:05Z", "owner": {"name": l.owner}})
}

fn caller(st: &Inner, req: &axum::http::request::Parts) -> Option<String> {
    if st.users.is_empty() {
        return Some("anonymous".into());
    }
    let engine = base64::engine::general_purpose::STANDARD;
    let decoded = req
        .headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Basic "))
        .and_then(|b| engine.decode(b).ok())
        .and_then(|b| String::from_utf8(b).ok())?;
    let (user, pass) = decoded.split_once(':')?;
    st.users
        .iter()
        .any(|(u, p)| u == user && p == pass)
        .then(|| user.to_owned())
}

/// A page of `locks` from the opaque cursor (an offset) on.
fn paged<'a>(
    st: &Inner,
    locks: Vec<&'a StoredLock>,
    cursor: Option<&str>,
) -> (Vec<&'a StoredLock>, Option<String>) {
    let start: usize = cursor.and_then(|c| c.parse().ok()).unwrap_or(0);
    let rest: Vec<_> = locks.into_iter().skip(start).collect();
    if st.page == 0 || rest.len() <= st.page {
        return (rest, None);
    }
    let next = (start + st.page).to_string();
    (rest.into_iter().take(st.page).collect(), Some(next))
}

async fn handle(State(state): State<Arc<Mutex<Inner>>>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let body = to_bytes(body, usize::MAX).await.unwrap();
    let json_body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let path = parts.uri.path().to_owned();
    let query = parts.uri.query().unwrap_or("").to_owned();
    let mut st = state.lock().unwrap();
    st.log.push(format!(
        "{} {path}{}",
        parts.method,
        if query.is_empty() {
            String::new()
        } else {
            format!("?{query}")
        }
    ));
    if parts.method == Method::POST {
        st.bodies.push(json_body.clone());
    }
    let Some(user) = caller(&st, &parts) else {
        return Response::builder()
            .status(401)
            .header(header::WWW_AUTHENTICATE, "Basic realm=\"lfs\"")
            .body(Body::empty())
            .unwrap();
    };
    let Some((_, tail)) = path.split_once("/locks") else {
        return reply(404, json!({"message": "not found"}));
    };
    let params: Vec<(String, String)> = url_pairs(&query);
    let param = |k: &str| params.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
    match (parts.method.clone(), tail) {
        (Method::POST, "") => {
            let want = json_body["path"].as_str().unwrap_or("").to_owned();
            if let Some(held) = st.locks.iter().find(|l| l.path == want) {
                return reply(
                    409,
                    json!({"lock": wire(held), "message": "already created lock"}),
                );
            }
            st.next_id += 1;
            let lock = StoredLock {
                id: format!("lock{}", st.next_id),
                path: want,
                owner: user,
            };
            let out = wire(&lock);
            st.locks.push(lock);
            reply(201, json!({"lock": out}))
        }
        (Method::GET, "") => {
            let mut all: Vec<&StoredLock> = st
                .locks
                .iter()
                .filter(|l| param("path").is_none_or(|p| p == l.path))
                .filter(|l| param("id").is_none_or(|i| i == l.id))
                .collect();
            all.sort_by(|a, b| a.path.cmp(&b.path));
            let (page, next) = paged(&st, all, param("cursor"));
            let mut out = json!({"locks": page.iter().map(|l| wire(l)).collect::<Vec<_>>()});
            if let Some(n) = next {
                out["next_cursor"] = json!(n);
            }
            reply(200, out)
        }
        (Method::POST, "/verify") => {
            if let Some(code) = st.verify_status {
                return reply(code, json!({"message": "verify unsupported"}));
            }
            let mut all: Vec<&StoredLock> = st.locks.iter().collect();
            all.sort_by(|a, b| a.path.cmp(&b.path));
            let (page, next) = paged(&st, all, json_body["cursor"].as_str());
            let ours: Vec<_> = page
                .iter()
                .filter(|l| l.owner == user)
                .map(|l| wire(l))
                .collect();
            let theirs: Vec<_> = page
                .iter()
                .filter(|l| l.owner != user)
                .map(|l| wire(l))
                .collect();
            let mut out = json!({"ours": ours, "theirs": theirs});
            if let Some(n) = next {
                out["next_cursor"] = json!(n);
            }
            reply(200, out)
        }
        (Method::POST, t) if t.starts_with('/') && t.ends_with("/unlock") => {
            let id = &t[1..t.len() - "/unlock".len()];
            let Some(pos) = st.locks.iter().position(|l| l.id == id) else {
                return reply(404, json!({"message": "no such lock"}));
            };
            let force = json_body["force"].as_bool().unwrap_or(false);
            if st.locks[pos].owner != user && !force {
                return reply(403, json!({"message": "not your lock"}));
            }
            let gone = st.locks.remove(pos);
            reply(200, json!({"lock": wire(&gone)}))
        }
        _ => reply(405, json!({"message": "method not allowed"})),
    }
}

fn url_pairs(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|p| !p.is_empty())
        .filter_map(|p| p.split_once('='))
        .map(|(k, v)| (k.to_owned(), pct_decode(v)))
        .collect()
}

fn pct_decode(v: &str) -> String {
    let b = v.as_bytes();
    let (mut out, mut i) = (Vec::new(), 0);
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                let hex = std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("00");
                out.push(u8::from_str_radix(hex, 16).unwrap_or(b'?'));
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}
