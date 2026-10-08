use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use pyn_core::memory::{MemoryMetadataStore, MemoryObjectStore};
use pyn_core::{RepoId, RepoService, Rules, ServiceConfig, SystemClock};
use pyn_proto as api;
use pyn_server::{AppState, auth::DevHeaderAuth, router};
use tower::ServiceExt;

fn app() -> axum::Router {
    let objects = Arc::new(MemoryObjectStore::new());
    let service = Arc::new(RepoService::new(
        RepoId::new("t"),
        Rules::from_toml("[meta]\ndefault = \"shared\"\n[exclusive]\npaths = [\"Content/\"]\n")
            .unwrap(),
        Arc::new(MemoryMetadataStore::new()),
        objects.clone(),
        Arc::new(SystemClock),
        ServiceConfig::default(),
    ));
    router(AppState {
        service,
        objects,
        auth: Arc::new(DevHeaderAuth),
    })
}

fn json_post(uri: &str, user: &str, body: serde_json::Value) -> Request<Body> {
    Request::post(uri)
        .header("content-type", "application/json")
        .header(api::DEV_USER_HEADER, user)
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[tokio::test]
async fn lock_conflict_is_a_409_with_a_stable_code() {
    let app = app();
    let body = serde_json::json!({"path": "Content/a.umap", "base_revision": null});

    let r = app
        .clone()
        .oneshot(json_post("/v1/checkout", "alice", body.clone()))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);

    let r = app
        .oneshot(json_post("/v1/checkout", "bob", body))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::CONFLICT);
    let err: api::ErrorBody =
        serde_json::from_slice(&r.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(err.code, "lock_held");
    assert!(err.message.contains("alice"));
}

#[tokio::test]
async fn unauthenticated_requests_are_rejected() {
    let r = app()
        .oneshot(
            Request::post("/v1/checkout")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"path":"Content/a.umap","base_revision":null}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(r.status().is_client_error());
}

#[tokio::test]
async fn openapi_document_lists_the_lock_routes() {
    let r = app()
        .oneshot(Request::get("/openapi.json").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let doc: serde_json::Value =
        serde_json::from_slice(&r.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert!(doc["paths"]["/v1/checkout"].is_object());
    assert!(doc["components"]["schemas"]["ErrorBody"].is_object());
}
