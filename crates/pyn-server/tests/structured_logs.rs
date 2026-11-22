//! Structured logging: JSON lines, request ids, the access log, the audit lines and the secret-free rule.

use std::io::Write;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore,
};
use pyn_core::{
    AccessConfig, AccessService, AuditAction, AuditScope, AuditStore, AuthProvider, NewAuditEvent,
    RegistrationMode, Repositories, Rules, SystemClock, UserId,
};
use pyn_proto as api;
use pyn_server::auth::{BearerAuth, DevHeaderAuth};
use pyn_server::observe::{LogConfig, LogFormat, LoggedAudit, subscriber};
use pyn_server::request_limits::RequestLimits;
use pyn_server::{AppState, router_with_log};
use serde_json::Value;
use tower::ServiceExt;

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = Capture;

    fn make_writer(&'a self) -> Capture {
        self.clone()
    }
}

impl Capture {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }

    fn json_lines(&self) -> Vec<Value> {
        self.text()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}")))
            .collect()
    }
}

fn capture(format: LogFormat) -> (Capture, tracing::subscriber::DefaultGuard) {
    let cap = Capture::default();
    let sub = subscriber(
        format,
        tracing_subscriber::EnvFilter::new("info"),
        cap.clone(),
    );
    (cap, tracing::subscriber::set_default(sub))
}

fn app(log: LogConfig) -> Router {
    let clock = Arc::new(SystemClock);
    let audit = Arc::new(MemoryAuditStore::new());
    let meta = Arc::new(MemoryMetadataStore::new());
    let objects = Arc::new(MemoryObjectStore::new());
    let access = Arc::new(
        AccessService::new(
            Arc::new(MemoryAccessStore::new()),
            clock.clone(),
            Arc::new(pyn_core::MemoryLimitEvents::new()),
        )
        .with_config(AccessConfig {
            registration: RegistrationMode::Open,
            require_email_verification: false,
            ..AccessConfig::default()
        })
        .with_registry(meta.clone())
        .with_audit(audit.clone()),
    );
    let repos = Arc::new(Repositories::new(
        meta,
        objects.clone(),
        audit,
        access.clone(),
        clock,
        Rules::from_toml("[meta]\ndefault = \"shared\"\n").unwrap(),
    ));
    router_with_log(
        AppState {
            repos,
            objects,
            access: access.clone(),
            auth: Arc::new(BearerAuth { access }),
            dev_auth: Some(Arc::new(DevHeaderAuth) as Arc<dyn AuthProvider>),
            trust_forwarded: false,
            updates: None,
            storage: Default::default(),
        },
        RequestLimits::default(),
        log,
    )
}

async fn get(app: &Router, uri: &str, headers: &[(&str, &str)]) -> axum::response::Response {
    let mut req = Request::builder().method("GET").uri(uri);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    app.clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

fn access_lines(cap: &Capture) -> Vec<Value> {
    cap.json_lines()
        .into_iter()
        .filter(|l| l["target"] == "access")
        .collect()
}

#[tokio::test]
async fn access_line_carries_the_contract_fields() {
    let (cap, _guard) = capture(LogFormat::Json);
    let app = app(LogConfig::default());
    let res = get(
        &app,
        "/v1/repos/alice/demo/audit",
        &[(api::DEV_USER_HEADER, "alice")],
    )
    .await;
    let id = res.headers()["x-request-id"].to_str().unwrap().to_string();
    let lines = access_lines(&cap);
    assert_eq!(lines.len(), 1);
    let line = &lines[0];
    for key in ["timestamp", "level", "target", "message"] {
        assert!(line.get(key).is_some(), "missing {key}: {line}");
    }
    assert_eq!(line["level"], "INFO");
    assert_eq!(line["message"], "request");
    assert_eq!(line["request_id"], id);
    assert_eq!(line["span"]["request_id"], id);
    assert_eq!(line["method"], "GET");
    assert_eq!(line["route"], "/v1/repos/{owner}/{name}/audit");
    assert_eq!(line["status"], res.status().as_u16());
    assert!(line["latency_ms"].is_u64());
    assert!(line["response_bytes"].is_u64());
    assert_eq!(line["account"], "alice");
    assert_eq!(line["repo"], "alice/demo");
    assert!(line.get("client_ip").is_none() && line.get("user_agent").is_none());
    assert!(!cap.text().contains("/v1/repos/alice/demo/audit"));
}

#[tokio::test]
async fn request_id_round_trip() {
    let (cap, _guard) = capture(LogFormat::Json);
    let app = app(LogConfig::default());

    let res = get(&app, "/nowhere", &[("x-request-id", "trace-42.abc")]).await;
    assert_eq!(res.headers()["x-request-id"], "trace-42.abc");

    let res = get(&app, "/nowhere", &[]).await;
    let generated = res.headers()["x-request-id"].to_str().unwrap();
    assert_eq!(generated.len(), 32);
    assert!(generated.bytes().all(|b| b.is_ascii_hexdigit()));

    for bad in ["has space", &"x".repeat(129)] {
        let res = get(&app, "/nowhere", &[("x-request-id", bad)]).await;
        let replaced = res.headers()["x-request-id"].to_str().unwrap();
        assert_ne!(replaced, bad);
        assert_eq!(replaced.len(), 32);
    }

    let ids: Vec<_> = access_lines(&cap)
        .iter()
        .map(|l| l["request_id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids[0], "trace-42.abc");
    assert_eq!(ids[1], generated);
    assert_eq!(ids.len(), 4);
}

#[tokio::test]
async fn unmatched_route_logs_a_fixed_template() {
    let (cap, _guard) = capture(LogFormat::Json);
    let app = app(LogConfig::default());
    let res = get(&app, "/no/such/thing-9", &[]).await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let lines = access_lines(&cap);
    assert_eq!(lines[0]["route"], "unmatched");
    assert!(!cap.text().contains("thing-9"));
}

#[tokio::test]
async fn secrets_never_reach_the_log() {
    let (cap, _guard) = capture(LogFormat::Json);
    let app = app(LogConfig {
        access: true,
        client: false,
    });
    let req = Request::builder()
        .method("POST")
        .uri("/v1/repos/alice/demo/merge?api_key=QUERYSECRET")
        .header("authorization", "Bearer pyn_TOKENSECRET")
        .header("cookie", "pyn_session=COOKIESECRET")
        .header("user-agent", "UASECRET")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"email":"BODYSECRET@example.com"}"#))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert!(res.status().is_client_error());
    let out = cap.text();
    assert!(!out.is_empty());
    for secret in [
        "QUERYSECRET",
        "TOKENSECRET",
        "COOKIESECRET",
        "UASECRET",
        "BODYSECRET",
        "example.com",
    ] {
        assert!(!out.contains(secret), "{secret} leaked: {out}");
    }
}

#[tokio::test]
async fn client_details_are_opt_in() {
    let (cap, _guard) = capture(LogFormat::Json);
    let app = app(LogConfig {
        access: true,
        client: true,
    });
    let mut req = Request::builder()
        .uri("/nowhere")
        .header("user-agent", "pyn-test/1.0")
        .body(Body::empty())
        .unwrap();
    req.extensions_mut()
        .insert(axum::extract::ConnectInfo(std::net::SocketAddr::from((
            [203, 0, 113, 9],
            4000,
        ))));
    app.clone().oneshot(req).await.unwrap();
    let line = &access_lines(&cap)[0];
    assert_eq!(line["client_ip"], "203.0.113.9");
    assert_eq!(line["user_agent"], "pyn-test/1.0");
}

#[tokio::test]
async fn access_log_can_be_turned_off() {
    let (cap, _guard) = capture(LogFormat::Json);
    let app = app(LogConfig {
        access: false,
        client: false,
    });
    let res = get(&app, "/healthz", &[]).await;
    assert!(res.headers().contains_key("x-request-id"));
    assert!(cap.text().is_empty());
}

#[tokio::test]
async fn text_is_the_default_format() {
    assert_eq!(LogFormat::default(), LogFormat::Text);
    let (cap, _guard) = capture(LogFormat::default());
    let app = app(LogConfig::default());
    get(&app, "/nowhere", &[]).await;
    let mut out = cap.text();
    while let Some(start) = out.find('\x1b') {
        let end = start + out[start..].find('m').unwrap() + 1;
        out.replace_range(start..end, "");
    }
    assert!(
        out.contains("request_id=") && out.contains("route=\"unmatched\""),
        "{out}"
    );
    assert!(serde_json::from_str::<Value>(out.lines().next().unwrap()).is_err());
}

#[tokio::test]
async fn audit_lines_name_the_event_without_its_detail() {
    let (cap, _guard) = capture(LogFormat::Json);
    let store = LoggedAudit(Arc::new(MemoryAuditStore::new()));
    store
        .record(
            &AuditScope::Server,
            NewAuditEvent {
                at: chrono::Utc::now(),
                actor: UserId::new("root"),
                action: AuditAction::Checkout,
                path: None,
                detail: "secret@example.com".into(),
            },
        )
        .await
        .unwrap();
    let lines = cap.json_lines();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["target"], "audit");
    assert_eq!(lines[0]["actor"], "root");
    assert_eq!(lines[0]["action"], "checkout");
    assert!(!cap.text().contains("example.com"));
}

#[tokio::test]
async fn health_checks_are_not_logged_but_other_requests_are() {
    for format in [LogFormat::Json, LogFormat::Text] {
        let (cap, _guard) = capture(format);
        let app = app(LogConfig::default());
        let res = get(&app, "/healthz", &[]).await;
        assert!(res.headers().contains_key("x-request-id"));
        assert!(cap.text().is_empty(), "{}", cap.text());
        get(&app, "/nowhere", &[]).await;
        let out = cap.text();
        assert_eq!(out.lines().count(), 1, "{out}");
        assert!(
            out.contains("unmatched") && !out.contains("/healthz"),
            "{out}"
        );
    }
}
