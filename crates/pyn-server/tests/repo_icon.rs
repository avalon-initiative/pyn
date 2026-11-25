//! Repository icons over HTTP: uploaded and cleared by the owner, served to readers, on every repository payload.

#[path = "common/cred.rs"]
mod cred;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use cred::{Cred, WithCred};
use http_body_util::BodyExt;
use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore,
};
use pyn_core::{
    AccessConfig, AccessMode, AccessService, AuthProvider, RegistrationMode, Repositories, Rules,
    SystemClock, UserId,
};
use pyn_proto as api;
use pyn_server::auth::{BearerAuth, DevHeaderAuth};
use pyn_server::{AppState, router};
use serde_json::json;
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";

async fn server() -> Router {
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
            access_mode: AccessMode::Public,
            require_email_verification: false,
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
        trust_forwarded: false,
        trusted_proxies: 1,
        updates: None,
        storage: Default::default(),
        certificate: Default::default(),
        ready: Default::default(),
    })
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
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

    fn header(&self, name: &str) -> &str {
        self.headers.get(name).map_or("", |v| v.to_str().unwrap())
    }
}

/// `who` is a bearer token, a development user name, or empty for no credentials.
async fn send(
    app: &Router,
    who: &str,
    method: &str,
    uri: &str,
    body: Vec<u8>,
    extra: &[(&str, &str)],
) -> Reply {
    let mut b = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
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
        headers: parts.headers,
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
    send(app, who, method, uri, bytes, &[]).await
}

async fn token_for(app: &Router, name: &str) -> String {
    let body = json!({"username": name, "password": PASSWORD});
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

async fn put_icon(app: &Router, who: &str, repo: &str, bytes: Vec<u8>) -> Reply {
    send(
        app,
        who,
        "PUT",
        &format!("/v1/repos/{repo}/icon"),
        bytes,
        &[],
    )
    .await
}

async fn info(app: &Router, who: &str, repo: &str) -> api::RepoInfo {
    let r = call(app, who, "GET", &format!("/v1/repos/{repo}"), None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()
}

fn png() -> Vec<u8> {
    let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
    b.resize(100, 3);
    b
}

fn jpeg() -> Vec<u8> {
    let mut b = vec![0xFF, 0xD8, 0xFF, 0xE0];
    b.resize(100, 9);
    b
}

#[tokio::test]
async fn an_icon_is_uploaded_served_on_every_payload_replaced_and_cleared() {
    let app = server().await;
    let alice = token_for(&app, "alice").await;
    make_repo(&app, &alice, "game", "public").await;
    assert_eq!(info(&app, "", "alice/game").await.icon_url, None);
    let r = call(&app, "", "GET", "/v1/repos/alice/game/icon", None).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::NOT_FOUND, "icon_not_found")
    );

    let r = put_icon(&app, &alice, "alice/game", png()).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let url = r.json::<api::RepoInfo>().icon_url.expect("icon url");
    assert!(url.starts_with("/v1/repos/alice/game/icon?v="), "{url}");
    assert_eq!(
        info(&app, "", "alice/game").await.icon_url.as_ref(),
        Some(&url)
    );
    let r = call(&app, &alice, "GET", "/v1/repos", None).await;
    assert_eq!(
        r.json::<Vec<api::RepoInfo>>()[0].icon_url.as_ref(),
        Some(&url)
    );
    let r = call(&app, "", "GET", "/v1/explore/repos", None).await;
    assert_eq!(
        r.json::<api::RepoPage>().repos[0].icon_url.as_ref(),
        Some(&url)
    );

    let r = call(&app, "", "GET", &url, None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body, png());
    assert_eq!(r.header("content-type"), "image/png");
    assert_eq!(r.header("x-content-type-options"), "nosniff");
    assert_eq!(r.header("cache-control"), "no-cache");
    let etag = r.header("etag").to_string();
    let r = send(
        &app,
        "",
        "GET",
        "/v1/repos/alice/game/icon",
        vec![],
        &[("if-none-match", &etag)],
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_MODIFIED);

    let r = put_icon(&app, &alice, "alice/game", jpeg()).await;
    let new_url = r.json::<api::RepoInfo>().icon_url.unwrap();
    assert_ne!(new_url, url, "a new image is a new url");
    let r = call(&app, "", "GET", &new_url, None).await;
    assert_eq!(r.header("content-type"), "image/jpeg");
    assert_eq!(r.body, jpeg());

    let r = call(&app, &alice, "DELETE", "/v1/repos/alice/game/icon", None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(r.json::<api::RepoInfo>().icon_url, None);
    let r = call(&app, "", "GET", "/v1/repos/alice/game/icon", None).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = call(&app, &alice, "DELETE", "/v1/repos/alice/game/icon", None).await;
    assert_eq!(r.status, StatusCode::OK, "removing none is harmless");

    let r = call(&app, &alice, "GET", "/v1/repos/alice/game/audit", None).await;
    let details: Vec<_> = r
        .json::<api::AuditPage>()
        .entries
        .into_iter()
        .filter(|e| e.action == "repo_updated")
        .map(|e| e.detail)
        .collect();
    assert_eq!(
        details.len(),
        3,
        "set, replaced, cleared; not the no-op: {details:?}"
    );
    assert!(
        details.iter().all(|d| d.ends_with("changed icon")),
        "{details:?}"
    );
}

#[tokio::test]
async fn icons_are_sniffed_bounded_and_rate_limited_like_avatars() {
    let app = server().await;
    let alice = token_for(&app, "alice").await;
    make_repo(&app, &alice, "game", "private").await;
    for (bytes, status, code) in [
        (
            b"GIF89a....".to_vec(),
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_image",
        ),
        (Vec::new(), StatusCode::BAD_REQUEST, "invalid_request"),
        (
            {
                let mut b = png();
                b.resize((1 << 20) + 1, 0);
                b
            },
            StatusCode::PAYLOAD_TOO_LARGE,
            "object_too_large",
        ),
    ] {
        let r = put_icon(&app, &alice, "alice/game", bytes).await;
        assert_eq!((r.status, r.code().as_str()), (status, code));
    }
    assert_eq!(info(&app, &alice, "alice/game").await.icon_url, None);

    let mut max = png();
    max.resize(1 << 20, 0);
    for _ in 0..30 {
        let r = put_icon(&app, &alice, "alice/game", max.clone()).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    }
    let r = put_icon(&app, &alice, "alice/game", png()).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::TOO_MANY_REQUESTS, "too_many_attempts")
    );
}

#[tokio::test]
async fn only_the_owner_changes_an_icon_and_only_readers_see_it() {
    let app = server().await;
    let (alice, bob) = (token_for(&app, "alice").await, token_for(&app, "bob").await);
    make_repo(&app, &alice, "open", "public").await;
    make_repo(&app, &alice, "secret", "private").await;
    for repo in ["alice/open", "alice/secret"] {
        let r = put_icon(&app, &alice, repo, png()).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    }

    let r = put_icon(&app, &bob, "alice/open", jpeg()).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    let r = call(&app, &bob, "DELETE", "/v1/repos/alice/open/icon", None).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    let r = put_icon(&app, &bob, "alice/secret", jpeg()).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::NOT_FOUND, "repo_not_found")
    );

    let r = call(&app, &bob, "GET", "/v1/repos/alice/open/icon", None).await;
    assert_eq!((r.status, r.body), (StatusCode::OK, png()));
    for who in [bob.as_str(), ""] {
        let r = call(&app, who, "GET", "/v1/repos/alice/secret/icon", None).await;
        assert_eq!(
            (r.status, r.code().as_str()),
            (StatusCode::NOT_FOUND, "repo_not_found")
        );
    }
}

#[tokio::test]
async fn a_fork_starts_without_its_sources_icon() {
    let app = server().await;
    let (alice, bob) = (token_for(&app, "alice").await, token_for(&app, "bob").await);
    make_repo(&app, &alice, "game", "public").await;
    put_icon(&app, &alice, "alice/game", png()).await;
    let r = call(
        &app,
        &bob,
        "POST",
        "/v1/repos/alice/game/forks",
        Some(json!({})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(r.json::<api::RepoInfo>().icon_url, None);
    assert!(info(&app, &bob, "alice/game").await.icon_url.is_some());
}
