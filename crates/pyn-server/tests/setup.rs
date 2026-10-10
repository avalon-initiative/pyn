//! The HTTP contract of first-run setup: what a fresh server serves, the token and the one-time administrator.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore,
};
use pyn_core::{AccessConfig, AccessService, Repositories, Rules, SystemClock};
use pyn_proto as api;
use pyn_server::auth::BearerAuth;
use pyn_server::{AppState, router};
use tower::ServiceExt;

const TOKEN: &str = "0123456789abcdef0123456789abcdef";
const PASSWORD: &str = "correct horse battery";

fn app() -> Router {
    let clock = Arc::new(SystemClock);
    let objects = Arc::new(MemoryObjectStore::new());
    let audit = Arc::new(MemoryAuditStore::new());
    let access = Arc::new(
        AccessService::new(Arc::new(MemoryAccessStore::new()), clock.clone())
            .with_config(AccessConfig::default())
            .with_audit(audit.clone())
            .with_setup_token(TOKEN)
            .unwrap(),
    );
    let repos = Arc::new(Repositories::new(
        Arc::new(MemoryMetadataStore::new()),
        objects.clone(),
        audit,
        access.clone(),
        clock,
        Rules::empty(),
    ));
    router(AppState {
        repos,
        objects,
        access: access.clone(),
        auth: Arc::new(BearerAuth { access }),
        dev_auth: None,
        trust_forwarded: false,
    })
}

async fn send(app: &Router, req: Request<Body>) -> (StatusCode, Vec<u8>) {
    let r = app.clone().oneshot(req).await.unwrap();
    let status = r.status();
    (
        status,
        r.into_body().collect().await.unwrap().to_bytes().to_vec(),
    )
}

fn get(uri: &str) -> Request<Body> {
    Request::get(uri).body(Body::empty()).unwrap()
}

fn post(uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::post(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn setup_body(token: &str) -> serde_json::Value {
    serde_json::json!({
        "token": token,
        "username": "root",
        "password": PASSWORD,
        "server_name": "Studio",
        "registration": "invite",
    })
}

fn code(body: &[u8]) -> String {
    serde_json::from_slice::<api::ErrorBody>(body).unwrap().code
}

#[tokio::test]
async fn a_fresh_server_serves_only_setup_and_the_health_check() {
    let app = app();
    assert_eq!(send(&app, get("/healthz")).await.0, StatusCode::OK);
    let (status, body) = send(&app, get("/v1/setup")).await;
    assert_eq!(status, StatusCode::OK);
    let info: api::SetupStatus = serde_json::from_slice(&body).unwrap();
    assert!(!info.initialised);

    for uri in ["/v1/registration", "/v1/me", "/v1/repos", "/openapi.json"] {
        let (status, body) = send(&app, get(uri)).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{uri}");
        assert_eq!(code(&body), "not_initialised");
    }
    let (status, body) = send(
        &app,
        post(
            "/v1/register",
            serde_json::json!({"username": "eve", "password": PASSWORD}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(code(&body), "not_initialised");
}

#[tokio::test]
async fn a_wrong_token_is_refused() {
    let app = app();
    let (status, body) = send(&app, post("/v1/setup", setup_body("wrong"))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(code(&body), "invalid_setup_token");
    let (_, body) = send(&app, get("/v1/setup")).await;
    assert!(
        !serde_json::from_slice::<api::SetupStatus>(&body)
            .unwrap()
            .initialised
    );
}

#[tokio::test]
async fn setup_creates_the_administrator_then_opens_the_server_and_never_runs_again() {
    let app = app();
    let (status, body) = send(&app, post("/v1/setup", setup_body(TOKEN))).await;
    assert_eq!(status, StatusCode::CREATED);
    let done: api::SetupCompleted = serde_json::from_slice(&body).unwrap();
    assert_eq!(done.user, "root");

    let (_, body) = send(&app, get("/v1/setup")).await;
    let info: api::SetupStatus = serde_json::from_slice(&body).unwrap();
    assert!(info.initialised);
    assert_eq!(info.server_name.as_deref(), Some("Studio"));
    assert_eq!(info.registration, "invite");

    let (_, body) = send(&app, get("/v1/registration")).await;
    let reg: api::RegistrationInfo = serde_json::from_slice(&body).unwrap();
    assert_eq!(reg.registration, "invite");

    let (status, body) = send(
        &app,
        post(
            "/v1/login",
            serde_json::json!({"username": "root", "password": PASSWORD}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));

    let (status, body) = send(&app, post("/v1/setup", setup_body(TOKEN))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(code(&body), "already_initialised");
}

#[tokio::test]
async fn the_setup_audit_entry_is_visible_to_the_administrator() {
    let app = app();
    send(&app, post("/v1/setup", setup_body(TOKEN))).await;
    let (_, body) = send(
        &app,
        post(
            "/v1/login",
            serde_json::json!({"username": "root", "password": PASSWORD}),
        ),
    )
    .await;
    let token = serde_json::from_slice::<serde_json::Value>(&body).unwrap()["token"]
        .as_str()
        .unwrap()
        .to_string();
    let req = Request::get("/v1/admin/audit")
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let (status, body) = send(&app, req).await;
    assert_eq!(status, StatusCode::OK);
    let page: api::AuditPage = serde_json::from_slice(&body).unwrap();
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.entries[0].action, "server_setup_completed");
}
