//! A local Git LFS Batch API server for tests: `basic` transfer, optional Basic authentication, a request log and a
//! few switches for misbehaviour.
#![allow(dead_code)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::response::Response;
use base64::Engine as _;
use sha2::{Digest as _, Sha256};

pub fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[derive(Default)]
pub struct Inner {
    pub objects: HashMap<String, Vec<u8>>,
    /// `METHOD path` per request, with object ids cut to eight digits.
    pub log: Vec<String>,
    /// The `Range` header of each object GET.
    pub ranges: Vec<String>,
    /// The `Authorization` header of each request that had one.
    pub auth_seen: Vec<String>,
    pub credentials: Option<(String, String)>,
    pub corrupt_downloads: bool,
    pub refuse_uploads: bool,
    /// Answers every batch with this status.
    pub batch_status: Option<u16>,
    /// Hosts the action URLs at this host instead of the request's.
    pub href_host: Option<String>,
    /// Only the batch endpoint asks for credentials.
    pub open_objects: bool,
    /// Holds each object GET and PUT this long, so overlapping transfers can be seen.
    pub object_delay_ms: u64,
    pub in_flight: usize,
    /// The most object transfers that were in progress at once.
    pub max_in_flight: usize,
    /// Object GETs and PUTs for these ids answer 500.
    pub fail_objects: Vec<String>,
}

pub struct LfsServer {
    pub addr: SocketAddr,
    pub state: Arc<Mutex<Inner>>,
    _rt: tokio::runtime::Runtime,
}

impl LfsServer {
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

    pub fn require(&self, user: &str, password: &str) {
        self.state.lock().unwrap().credentials = Some((user.into(), password.into()));
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    pub fn put(&self, content: &[u8]) -> String {
        let oid = sha256_hex(content);
        self.state
            .lock()
            .unwrap()
            .objects
            .insert(oid.clone(), content.to_vec());
        oid
    }

    pub fn has(&self, oid: &str) -> Option<Vec<u8>> {
        self.state.lock().unwrap().objects.get(oid).cloned()
    }

    pub fn log(&self) -> Vec<String> {
        self.state.lock().unwrap().log.clone()
    }

    pub fn count(&self, prefix: &str) -> usize {
        self.log().iter().filter(|l| l.starts_with(prefix)).count()
    }
}

fn reply(status: u16, body: impl Into<Body>) -> Response {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/vnd.git-lfs+json")
        .body(body.into())
        .unwrap()
}

fn authorized(headers: &HeaderMap, want: &(String, String)) -> bool {
    let engine = base64::engine::general_purpose::STANDARD;
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Basic "))
        .and_then(|b| engine.decode(b).ok())
        .and_then(|b| String::from_utf8(b).ok())
        .is_some_and(|s| s == format!("{}:{}", want.0, want.1))
}

async fn handle(State(state): State<Arc<Mutex<Inner>>>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let path = parts.uri.path().to_owned();
    let host = parts
        .headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let short: Vec<String> = path
        .split('/')
        .map(|s| {
            if s.len() == 64 {
                format!("{}..", &s[..8])
            } else {
                s.to_owned()
            }
        })
        .collect();
    let body = to_bytes(body, usize::MAX).await.unwrap().to_vec();
    let delayed = matches!(parts.method, Method::GET | Method::PUT)
        && path.contains("/objects/")
        && !path.ends_with("/objects/verify");
    if delayed {
        let delay = {
            let mut st = state.lock().unwrap();
            st.in_flight += 1;
            st.max_in_flight = st.max_in_flight.max(st.in_flight);
            st.object_delay_ms
        };
        tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
        state.lock().unwrap().in_flight -= 1;
    }
    let mut st = state.lock().unwrap();
    st.log.push(format!("{} {}", parts.method, short.join("/")));
    if let Some(a) = parts.headers.get(header::AUTHORIZATION) {
        st.auth_seen.push(a.to_str().unwrap_or("?").to_owned());
    }
    let guarded = !(st.open_objects && !path.ends_with("/objects/batch"));
    if let Some(want) = &st.credentials
        && guarded
        && !authorized(&parts.headers, want)
    {
        return Response::builder()
            .status(401)
            .header(header::WWW_AUTHENTICATE, "Basic realm=\"lfs\"")
            .body(Body::empty())
            .unwrap();
    }
    let host = st.href_host.clone().unwrap_or(host);
    let prefix = path
        .rsplit_once("/objects/")
        .map_or("", |(p, _)| p)
        .to_owned();
    if parts.method == Method::POST && path.ends_with("/objects/batch") {
        if let Some(code) = st.batch_status {
            return reply(code, r#"{"message":"batch refused"}"#);
        }
        let req: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let upload = req["operation"] == "upload";
        let objects: Vec<_> = req["objects"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| {
                let oid = o["oid"].as_str().unwrap();
                let href = format!("http://{host}{prefix}/objects/{oid}");
                let have = st.objects.contains_key(oid);
                match (upload, have) {
                    (true, true) => serde_json::json!({"oid": oid, "size": o["size"]}),
                    (true, false) => serde_json::json!({
                        "oid": oid, "size": o["size"], "authenticated": true,
                        "actions": {
                            "upload": {"href": href},
                            "verify": {"href": format!("http://{host}{prefix}/objects/verify")},
                        },
                    }),
                    (false, true) => serde_json::json!({
                        "oid": oid, "size": o["size"], "authenticated": true,
                        "actions": {"download": {"href": href}},
                    }),
                    (false, false) => serde_json::json!({
                        "oid": oid, "size": o["size"],
                        "error": {"code": 404, "message": "Object does not exist"},
                    }),
                }
            })
            .collect();
        return reply(
            200,
            serde_json::json!({"transfer": "basic", "objects": objects, "hash_algo": "sha256"})
                .to_string(),
        );
    }
    if parts.method == Method::POST && path.ends_with("/objects/verify") {
        let req: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let ok = st
            .objects
            .get(req["oid"].as_str().unwrap())
            .is_some_and(|c| c.len() as u64 == req["size"].as_u64().unwrap());
        return reply(if ok { 200 } else { 404 }, "{}");
    }
    let Some(oid) = path.rsplit_once("/objects/").map(|(_, o)| o.to_owned()) else {
        return reply(404, r#"{"message":"not found"}"#);
    };
    if matches!(parts.method, Method::GET | Method::PUT) && st.fail_objects.contains(&oid) {
        return reply(500, r#"{"message":"boom"}"#);
    }
    match parts.method {
        Method::GET => {
            let Some(mut content) = st.objects.get(&oid).cloned() else {
                return reply(404, r#"{"message":"no such object"}"#);
            };
            if st.corrupt_downloads && !content.is_empty() {
                content[0] ^= 0xff;
            }
            let range = parts
                .headers
                .get(header::RANGE)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            if let Some(r) = &range {
                st.ranges.push(r.clone());
            }
            let start = range
                .as_deref()
                .and_then(|r| r.strip_prefix("bytes="))
                .and_then(|r| r.strip_suffix('-'))
                .and_then(|n| n.parse::<usize>().ok());
            match start {
                Some(s) if s < content.len() => Response::builder()
                    .status(206)
                    .header(
                        header::CONTENT_RANGE,
                        format!("bytes {s}-{}/{}", content.len() - 1, content.len()),
                    )
                    .body(Body::from(content[s..].to_vec()))
                    .unwrap(),
                Some(_) => Response::builder().status(416).body(Body::empty()).unwrap(),
                None => Response::builder()
                    .status(200)
                    .body(Body::from(content))
                    .unwrap(),
            }
        }
        Method::PUT => {
            if st.refuse_uploads {
                return reply(403, r#"{"message":"read only"}"#);
            }
            if sha256_hex(&body) != oid {
                return reply(422, r#"{"message":"digest mismatch"}"#);
            }
            st.objects.insert(oid, body);
            Response::builder()
                .status(StatusCode::OK)
                .body(Body::empty())
                .unwrap()
        }
        _ => reply(405, "{}"),
    }
}
