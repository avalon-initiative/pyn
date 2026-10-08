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

#[tokio::test]
async fn files_listing_shows_mode_and_lock() {
    let app = app();
    let body = serde_json::json!({"path": "Content/a.umap", "base_revision": null});
    app.clone()
        .oneshot(json_post("/v1/checkout", "alice", body))
        .await
        .unwrap();

    let r = app
        .oneshot(Request::get("/v1/files").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let page: api::FilePage =
        serde_json::from_slice(&r.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(page.entries.len(), 1);
    let entry = &page.entries[0];
    assert_eq!(
        (entry.path.as_str(), entry.mode, entry.revision),
        ("Content/a.umap", api::Mode::Exclusive, None)
    );
    assert_eq!(entry.lock.as_ref().unwrap().owner, "alice");
    assert!(page.next_after.is_none());
}

#[tokio::test]
async fn any_revision_can_be_fetched() {
    let app = app();
    let mut base = serde_json::Value::Null;
    for text in ["v1", "v2"] {
        let put = Request::put("/v1/objects")
            .header(api::DEV_USER_HEADER, "bob")
            .body(Body::from(text))
            .unwrap();
        let r = app.clone().oneshot(put).await.unwrap();
        let obj: api::PutObjectResponse =
            serde_json::from_slice(&r.into_body().collect().await.unwrap().to_bytes()).unwrap();
        let body = serde_json::json!({"path": "Source/a.cpp", "content": obj.content, "base_revision": base, "message": text});
        let r = app
            .clone()
            .oneshot(json_post("/v1/checkin", "bob", body))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        base = serde_json::json!(base.as_u64().unwrap_or(0) + 1);
    }

    let fetch = |query: &'static str| {
        let app = app.clone();
        async move {
            app.oneshot(
                Request::get(format!("/v1/content?{query}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
        }
    };
    let r = fetch("path=Source/a.cpp&revision=1").await;
    assert_eq!(r.headers()[api::REVISION_HEADER], "1");
    assert_eq!(
        r.into_body().collect().await.unwrap().to_bytes().as_ref(),
        b"v1"
    );

    let r = fetch("path=Source/a.cpp").await;
    assert_eq!(r.headers()[api::REVISION_HEADER], "2");
    assert_eq!(
        r.into_body().collect().await.unwrap().to_bytes().as_ref(),
        b"v2"
    );

    let r = fetch("path=Source/a.cpp&revision=9").await;
    assert_eq!(r.status(), StatusCode::NOT_FOUND);
    let err: api::ErrorBody =
        serde_json::from_slice(&r.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(err.code, "revision_not_found");
}
