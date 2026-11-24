use std::sync::Arc;

#[path = "common/cred.rs"]
mod cred;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use cred::{Cred, WithCred};
use http_body_util::BodyExt;
use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore,
};
use pyn_core::{
    AccessConfig, AccessMode, AccessService, AuthProvider, RateLimits, RegistrationMode,
    Repositories, Rules, SystemClock, UserId,
};
use pyn_proto as api;
use pyn_server::auth::{BearerAuth, DevHeaderAuth};
use pyn_server::{AppState, router};
use serde_json::json;
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";
const EMAIL: &str = "secret.person@mail.test";

async fn server() -> Router {
    build(AccessMode::Public, RateLimits::default(), false).await
}

async fn build(mode: AccessMode, limits: RateLimits, trust_forwarded: bool) -> Router {
    let clock = Arc::new(SystemClock);
    let objects = Arc::new(MemoryObjectStore::new());
    let audit = Arc::new(MemoryAuditStore::new());
    let meta = Arc::new(MemoryMetadataStore::new());
    let access = Arc::new(
        AccessService::new(
            Arc::new(MemoryAccessStore::new()),
            clock.clone(),
            Arc::new(pyn_core::MemoryLimitEvents::new()),
        )
        .with_config(AccessConfig {
            registration: RegistrationMode::Open,
            access_mode: mode,
            require_email_verification: false,
            limits,
            ..AccessConfig::default()
        })
        .with_registry(meta.clone())
        .with_audit(audit.clone()),
    );
    access.admin_for_tests(&UserId::new("root")).await.unwrap();
    let repos = Arc::new(Repositories::new(
        meta,
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
        dev_auth: Some(Arc::new(DevHeaderAuth) as Arc<dyn AuthProvider>),
        trust_forwarded,
        trusted_proxies: 1,
        updates: None,
        storage: Default::default(),
        certificate: Default::default(),
        ready: Default::default(),
    })
}

struct Reply {
    status: StatusCode,
    body: Vec<u8>,
}

impl Reply {
    fn json<T: serde::de::DeserializeOwned>(&self) -> T {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&self.body)))
    }

    fn code(&self) -> String {
        self.json::<api::ErrorBody>().code
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// `who` is a bearer token, a development user name, or empty for no credentials.
async fn send(
    app: &Router,
    who: &str,
    method: &str,
    uri: &str,
    content_type: &str,
    body: Vec<u8>,
    extra: &[(&str, &str)],
) -> Reply {
    let mut b = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", content_type);
    b = b.cred(&Cred::who(who));
    for (k, v) in extra {
        b = b.header(*k, *v);
    }
    let r = app
        .clone()
        .oneshot(b.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let (parts, body) = r.into_parts();
    Reply {
        status: parts.status,
        body: body.collect().await.unwrap().to_bytes().to_vec(),
    }
}

async fn call(
    app: &Router,
    who: &str,
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
) -> Reply {
    let bytes = body.map(|b| b.to_string().into_bytes()).unwrap_or_default();
    send(app, who, method, uri, "application/json", bytes, &[]).await
}

async fn token_for(app: &Router, name: &str) -> String {
    let body = json!({"username": name, "password": PASSWORD, "email": EMAIL});
    let r = call(app, "", "POST", "/v1/register", Some(body.clone())).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = call(app, "", "POST", "/v1/login", Some(body)).await;
    r.json::<api::CreatedToken>().token
}

async fn make_repo(app: &Router, who: &str, name: &str, visibility: &str) {
    let body = json!({"name": name, "visibility": visibility});
    let r = call(app, who, "POST", "/v1/repos", Some(body)).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
}
async fn info(app: &Router, who: &str, repo: &str) -> api::RepoInfo {
    let r = call(app, who, "GET", &format!("/v1/repos/{repo}"), None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()
}

async fn patch(app: &Router, who: &str, repo: &str, body: serde_json::Value) -> Reply {
    call(app, who, "PATCH", &format!("/v1/repos/{repo}"), Some(body)).await
}

#[tokio::test]
async fn a_description_and_website_are_set_at_creation_and_returned_everywhere() {
    let app = server().await;
    let alice = token_for(&app, "alice").await;
    let body = json!({"name": "game", "visibility": "public",
        "description": "  A small game  ", "website": "https://example.com/game"});
    let r = call(&app, &alice, "POST", "/v1/repos", Some(body)).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let made: api::RepoInfo = r.json();
    assert_eq!(made.description.as_deref(), Some("A small game"));
    assert_eq!(made.website.as_deref(), Some("https://example.com/game"));

    let anonymous = info(&app, "", "alice/game").await;
    assert_eq!(anonymous.description.as_deref(), Some("A small game"));
    let listed: Vec<api::RepoInfo> = call(&app, &alice, "GET", "/v1/repos", None).await.json();
    assert_eq!(
        listed[0].website.as_deref(),
        Some("https://example.com/game")
    );
    let explored: api::RepoPage = call(&app, "", "GET", "/v1/explore/repos", None)
        .await
        .json();
    assert_eq!(
        explored.repos[0].description.as_deref(),
        Some("A small game")
    );

    let r = call(
        &app,
        &alice,
        "POST",
        "/v1/repos",
        Some(json!({"name": "bare"})),
    )
    .await;
    let bare: api::RepoInfo = r.json();
    assert_eq!((bare.description, bare.website), (None, None));
    let raw: serde_json::Value = call(&app, &alice, "GET", "/v1/repos/alice/bare", None)
        .await
        .json();
    assert!(
        raw["description"].is_null() && raw["website"].is_null(),
        "{raw}"
    );
}

#[tokio::test]
async fn the_owner_edits_and_clears_the_about_block_and_the_change_is_audited() {
    let app = server().await;
    let alice = token_for(&app, "alice").await;
    make_repo(&app, &alice, "game", "private").await;

    let r = patch(
        &app,
        &alice,
        "alice/game",
        json!({"description": "Levels and art"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let r = patch(
        &app,
        &alice,
        "alice/game",
        json!({"website": "https://example.com"}),
    )
    .await;
    let both: api::RepoInfo = r.json();
    assert_eq!(
        both.description.as_deref(),
        Some("Levels and art"),
        "a field left out stays"
    );
    assert_eq!(both.website.as_deref(), Some("https://example.com"));

    let r = patch(
        &app,
        &alice,
        "alice/game",
        json!({"description": null, "website": " "}),
    )
    .await;
    let cleared: api::RepoInfo = r.json();
    assert_eq!((cleared.description, cleared.website), (None, None));

    let r = call(&app, &alice, "GET", "/v1/repos/alice/game/audit", None).await;
    let page: api::AuditPage = r.json();
    let details: Vec<_> = page
        .entries
        .iter()
        .filter(|e| e.action == "repo_updated")
        .map(|e| e.detail.clone())
        .collect();
    assert!(
        details[0].ends_with("changed description, website"),
        "{details:?}"
    );
    assert!(
        details.iter().all(|d| !d.contains("Levels")),
        "the log names fields, not content: {details:?}"
    );
}

#[tokio::test]
async fn an_invalid_about_block_is_refused_and_only_the_owner_may_change_it() {
    let app = server().await;
    let (alice, bob) = (token_for(&app, "alice").await, token_for(&app, "bob").await);
    make_repo(&app, &alice, "game", "public").await;
    for body in [
        json!({"description": "two\nlines"}),
        json!({"description": "x".repeat(351)}),
        json!({"website": "ftp://example.com"}),
        json!({"website": "https://user:pw@example.com"}),
    ] {
        let r = patch(&app, &alice, "alice/game", body.clone()).await;
        assert_eq!(
            (r.status, r.code()),
            (StatusCode::BAD_REQUEST, "invalid_request".into()),
            "{body}"
        );
    }
    let r = call(
        &app,
        &alice,
        "POST",
        "/v1/repos",
        Some(json!({"name": "other", "website": "not a link"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    assert_eq!(
        call(&app, &alice, "GET", "/v1/repos/alice/other", None)
            .await
            .status,
        StatusCode::NOT_FOUND
    );

    let r = patch(&app, &bob, "alice/game", json!({"description": "mine now"})).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    assert_eq!(info(&app, &bob, "alice/game").await.description, None);
}
