use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use pyn_core::memory::{MemoryAccessStore, MemoryMetadataStore, MemoryObjectStore};
use pyn_core::{
    AccessService, AuthProvider, RepoId, RepoService, Rules, ServiceConfig, SystemClock, UserId,
};
use pyn_proto as api;
use pyn_server::auth::{BearerAuth, DevHeaderAuth};
use pyn_server::{AppState, router};
use tower::ServiceExt;

const RULES: &str = "[meta]\ndefault = \"shared\"\n[exclusive]\npaths = [\"Content/\"]\n";

fn state(dev: bool) -> AppState {
    let repo = RepoId::new("t");
    let clock = Arc::new(SystemClock);
    let objects = Arc::new(MemoryObjectStore::new());
    let service = Arc::new(RepoService::new(
        repo.clone(),
        Rules::from_toml(RULES).unwrap(),
        Arc::new(MemoryMetadataStore::new()),
        objects.clone(),
        clock.clone(),
        ServiceConfig::default(),
    ));
    let access = Arc::new(AccessService::new(
        Arc::new(MemoryAccessStore::new()),
        clock,
    ));
    AppState {
        service,
        objects,
        access: access.clone(),
        auth: Arc::new(BearerAuth { access, repo }),
        dev_auth: dev.then(|| Arc::new(DevHeaderAuth) as Arc<dyn AuthProvider>),
    }
}

/// An app with development auth on, as `make run` uses it.
fn app() -> axum::Router {
    router(state(true))
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
    assert!(err.message.contains("locked by alice until"));
    assert!(err.message.contains("Ask alice to release it"));
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
        .oneshot(
            Request::get("/v1/files")
                .header(api::DEV_USER_HEADER, "alice")
                .body(Body::empty())
                .unwrap(),
        )
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
                    .header(api::DEV_USER_HEADER, "alice")
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

async fn body_json<T: serde::de::DeserializeOwned>(r: axum::response::Response) -> T {
    serde_json::from_slice(&r.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

fn with_token(
    method: &str,
    uri: &str,
    token: &str,
    body: Option<serde_json::Value>,
) -> Request<Body> {
    let builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {token}"));
    match body {
        Some(b) => builder
            .header("content-type", "application/json")
            .body(Body::from(b.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    }
}

async fn status(app: &axum::Router, req: Request<Body>) -> StatusCode {
    app.clone().oneshot(req).await.unwrap().status()
}

async fn admin_token(state: &AppState) -> String {
    state
        .access
        .bootstrap_admin(state.service.repo(), &UserId::new("root"))
        .await
        .unwrap()
}

#[tokio::test]
async fn the_dev_header_is_ignored_unless_dev_auth_is_on() {
    let app = router(state(false));
    let req = Request::get("/v1/files")
        .header(api::DEV_USER_HEADER, "alice")
        .body(Body::empty())
        .unwrap();
    assert_eq!(status(&app, req).await, StatusCode::UNAUTHORIZED);
    let bare = Request::get("/v1/files").body(Body::empty()).unwrap();
    assert_eq!(status(&app, bare).await, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn health_and_openapi_need_no_credentials() {
    let app = router(state(false));
    for uri in ["/healthz", "/openapi.json"] {
        assert_eq!(
            status(&app, Request::get(uri).body(Body::empty()).unwrap()).await,
            StatusCode::OK,
            "{uri}"
        );
    }
}

#[tokio::test]
async fn a_read_only_token_can_read_but_not_lock_or_check_in() {
    let st = state(false);
    let admin = admin_token(&st).await;
    let app = router(st);

    let made: api::CreatedToken = body_json(
        app.clone()
            .oneshot(with_token(
                "POST",
                "/v1/tokens",
                &admin,
                Some(serde_json::json!({"name": "ci", "permissions": ["read"]})),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert!(made.token.starts_with("pyn_"));
    assert_eq!(made.info.permissions, ["read"]);

    assert_eq!(
        status(&app, with_token("GET", "/v1/files", &made.token, None)).await,
        StatusCode::OK
    );
    let checkout = serde_json::json!({"path": "Content/a.umap", "base_revision": null});
    let r = app
        .clone()
        .oneshot(with_token(
            "POST",
            "/v1/checkout",
            &made.token,
            Some(checkout),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    let err: api::ErrorBody = body_json(r).await;
    assert_eq!(err.code, "forbidden");
}

#[tokio::test]
async fn a_token_cannot_ask_for_more_than_its_owner_holds() {
    let st = state(false);
    let admin = admin_token(&st).await;
    st.access
        .set_user_role(
            &st.access
                .principal(st.service.repo(), &UserId::new("root"))
                .await
                .unwrap(),
            st.service.repo(),
            &UserId::new("wendy"),
            pyn_core::Role::Writer,
        )
        .await
        .unwrap();
    let wendy = st
        .access
        .principal(st.service.repo(), &UserId::new("wendy"))
        .await
        .unwrap();
    let (_, wendy_token) = st
        .access
        .create_token(&wendy, "w", wendy.permissions.clone(), vec![], None)
        .await
        .unwrap();
    let app = router(st);
    let _ = admin;

    let body = serde_json::json!({"name": "sneaky", "permissions": ["read", "restore"]});
    assert_eq!(
        status(
            &app,
            with_token("POST", "/v1/tokens", &wendy_token, Some(body))
        )
        .await,
        StatusCode::FORBIDDEN
    );
    let body = serde_json::json!({"name": "nope", "permissions": ["flying"]});
    assert_eq!(
        status(
            &app,
            with_token("POST", "/v1/tokens", &wendy_token, Some(body))
        )
        .await,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn a_revoked_token_stops_working_and_whoami_reports_the_caller() {
    let st = state(false);
    let admin = admin_token(&st).await;
    let app = router(st);

    let me: api::Me = body_json(
        app.clone()
            .oneshot(with_token("GET", "/v1/me", &admin, None))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(me.user, "root");
    assert_eq!(me.permissions.len(), 8);

    let made: api::CreatedToken = body_json(
        app.clone()
            .oneshot(with_token(
                "POST",
                "/v1/tokens",
                &admin,
                Some(serde_json::json!({"name": "tmp", "permissions": ["read"]})),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        status(&app, with_token("GET", "/v1/me", &made.token, None)).await,
        StatusCode::OK
    );
    let uri = format!("/v1/tokens/{}", made.info.id);
    assert_eq!(
        status(&app, with_token("DELETE", &uri, &admin, None)).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        status(&app, with_token("GET", "/v1/me", &made.token, None)).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        status(
            &app,
            with_token("DELETE", "/v1/tokens/ffffffffffff", &admin, None)
        )
        .await,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn admins_manage_members_and_roles_and_others_cannot() {
    let st = state(false);
    let admin = admin_token(&st).await;
    let app = router(st);

    let add = serde_json::json!({"role": "writer"});
    assert_eq!(
        status(
            &app,
            with_token("PUT", "/v1/members/wendy", &admin, Some(add))
        )
        .await,
        StatusCode::NO_CONTENT
    );
    let members: Vec<api::Member> = body_json(
        app.clone()
            .oneshot(with_token("GET", "/v1/members", &admin, None))
            .await
            .unwrap(),
    )
    .await;
    assert!(
        members
            .iter()
            .any(|m| m.user == "wendy" && m.role == "writer")
    );

    let grants: Vec<api::RoleGrant> = body_json(
        app.clone()
            .oneshot(with_token("GET", "/v1/roles", &admin, None))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(grants.len(), 4);

    let narrow = serde_json::json!({"permissions": ["read"]});
    assert_eq!(
        status(
            &app,
            with_token("PUT", "/v1/roles/writer", &admin, Some(narrow))
        )
        .await,
        StatusCode::NO_CONTENT
    );
    let lock_out = serde_json::json!({"permissions": ["read"]});
    assert_eq!(
        status(
            &app,
            with_token("PUT", "/v1/roles/admin", &admin, Some(lock_out))
        )
        .await,
        StatusCode::BAD_REQUEST
    );
}
