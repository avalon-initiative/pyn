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

async fn tag_call(
    app: &Router,
    who: &str,
    method: &str,
    repo: &str,
    tail: &str,
    body: Option<serde_json::Value>,
) -> Reply {
    call(
        app,
        who,
        method,
        &format!("/v1/repos/{repo}/tags{tail}"),
        body,
    )
    .await
}

/// A commit on the default branch; returns its id.
async fn commit(app: &Router, who: &str, repo: &str, path: &str, text: &str) -> String {
    let put = send(
        app,
        who,
        "PUT",
        &format!("/v1/repos/{repo}/objects"),
        "application/octet-stream",
        text.as_bytes().to_vec(),
        &[],
    )
    .await;
    assert_eq!(put.status, StatusCode::OK, "{}", put.text());
    let content = put.json::<api::PutObjectResponse>().content;
    let body = json!({"path": path, "content": content, "base_revision": null, "message": "m"});
    let r = call(
        app,
        who,
        "POST",
        &format!("/v1/repos/{repo}/checkin"),
        Some(body),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let log = call(
        app,
        who,
        "GET",
        &format!("/v1/repos/{repo}/branches/main"),
        None,
    )
    .await;
    log.json::<api::Branch>().commit.unwrap()
}

#[tokio::test]
async fn a_tag_is_created_listed_read_and_deleted() {
    let app = server().await;
    let alice = token_for(&app, "alice").await;
    make_repo(&app, &alice, "game", "public").await;

    let c1 = commit(&app, &alice, "alice/game", "a.txt", "one").await;
    let r = tag_call(
        &app,
        &alice,
        "POST",
        "alice/game",
        "",
        Some(json!({"name": "rel/1.0", "message": "first"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let made = r.json::<api::Tag>();
    assert_eq!(
        (
            made.name.as_str(),
            made.commit.as_str(),
            made.message.as_deref(),
            made.created_by.as_str()
        ),
        ("rel/1.0", c1.as_str(), Some("first"), "alice")
    );
    assert!(made.committed_at.is_some());

    let c2 = commit(&app, &alice, "alice/game", "b.txt", "two").await;
    let r = tag_call(
        &app,
        &alice,
        "POST",
        "alice/game",
        "",
        Some(json!({"name": "v2", "target": c2})),
    )
    .await;
    assert_eq!(r.json::<api::Tag>().commit, c2);

    let listed = tag_call(&app, "", "GET", "alice/game", "", None).await;
    assert_eq!(
        listed.status,
        StatusCode::OK,
        "readable signed out on a public repository"
    );
    let names: Vec<_> = listed
        .json::<Vec<api::Tag>>()
        .into_iter()
        .map(|t| t.name)
        .collect();
    assert_eq!(names, ["rel/1.0", "v2"]);
    let one = tag_call(&app, "", "GET", "alice/game", "/rel/1.0", None).await;
    assert_eq!(
        one.json::<api::Tag>().commit,
        c1,
        "a slash in the name is part of the path"
    );
    let encoded = tag_call(&app, "", "GET", "alice/game", "/rel%2F1.0", None).await;
    assert_eq!(encoded.status, StatusCode::OK, "{}", encoded.text());

    let r = tag_call(&app, &alice, "DELETE", "alice/game", "/rel/1.0", None).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let r = tag_call(&app, &alice, "DELETE", "alice/game", "/rel/1.0", None).await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::NOT_FOUND, "tag_not_found".to_string())
    );
    let r = tag_call(&app, &alice, "GET", "alice/game", "/rel/1.0", None).await;
    assert_eq!(r.code(), "tag_not_found");
    let again = tag_call(
        &app,
        &alice,
        "POST",
        "alice/game",
        "",
        Some(json!({"name": "rel/1.0", "target": c2})),
    )
    .await;
    assert_eq!(
        again.json::<api::Tag>().commit,
        c2,
        "a deleted name can be used again"
    );
}

#[tokio::test]
async fn a_tag_never_moves_and_names_are_checked() {
    let app = server().await;
    let alice = token_for(&app, "alice").await;
    make_repo(&app, &alice, "game", "private").await;
    let c1 = commit(&app, &alice, "alice/game", "a.txt", "one").await;
    let c2 = commit(&app, &alice, "alice/game", "b.txt", "two").await;
    let made = |name: &str, target: Option<&str>| json!({"name": name, "target": target});
    tag_call(
        &app,
        &alice,
        "POST",
        "alice/game",
        "",
        Some(made("v1", Some(&c1))),
    )
    .await;
    for (name, target, status, code) in [
        ("v1", Some(c2.as_str()), StatusCode::CONFLICT, "tag_exists"),
        ("v1/x", None, StatusCode::CONFLICT, "tag_exists"),
        (
            "bad name",
            None,
            StatusCode::BAD_REQUEST,
            "invalid_tag_name",
        ),
        (
            "v3",
            Some("deadbeefdead"),
            StatusCode::NOT_FOUND,
            "commit_not_found",
        ),
    ] {
        let r = tag_call(
            &app,
            &alice,
            "POST",
            "alice/game",
            "",
            Some(made(name, target)),
        )
        .await;
        assert_eq!((r.status, r.code()), (status, code.to_string()), "{name}");
    }
    let r = tag_call(&app, &alice, "GET", "alice/game", "/v1", None).await;
    assert_eq!(r.json::<api::Tag>().commit, c1);
    let r = tag_call(
        &app,
        &alice,
        "POST",
        "alice/game",
        "",
        Some(made("v1.1", Some("v1"))),
    )
    .await;
    assert_eq!(
        r.json::<api::Tag>().commit,
        c1,
        "a tag name is a target too"
    );
}

#[tokio::test]
async fn tag_permissions_follow_the_repository() {
    let app = server().await;
    let (alice, bob, carol) = (
        token_for(&app, "alice").await,
        token_for(&app, "bob").await,
        token_for(&app, "carol").await,
    );
    make_repo(&app, &alice, "vault", "private").await;
    commit(&app, &alice, "alice/vault", "a.txt", "one").await;
    let grant = json!({"role": "reader"});
    let r = call(
        &app,
        &alice,
        "PUT",
        "/v1/repos/alice/vault/members/bob",
        Some(grant),
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
    let named = Some(json!({"name": "v1"}));

    let r = tag_call(&app, &bob, "POST", "alice/vault", "", named.clone()).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "a reader cannot tag");
    let r = tag_call(&app, &bob, "GET", "alice/vault", "", None).await;
    assert_eq!(r.status, StatusCode::OK, "a reader lists tags");
    for who in [&carol, ""] {
        let r = tag_call(&app, who, "GET", "alice/vault", "", None).await;
        assert!(
            matches!(r.status, StatusCode::NOT_FOUND | StatusCode::UNAUTHORIZED),
            "{}",
            r.text()
        );
    }
    let r = tag_call(&app, &alice, "POST", "alice/vault", "", named).await;
    assert_eq!(r.status, StatusCode::OK);
    let r = tag_call(&app, &bob, "DELETE", "alice/vault", "/v1", None).await;
    assert_eq!(
        r.status,
        StatusCode::FORBIDDEN,
        "a reader cannot delete a tag"
    );
    let r = tag_call(&app, &alice, "GET", "alice/vault", "/v1", None).await;
    assert_eq!(r.status, StatusCode::OK);
}

#[tokio::test]
async fn deleting_a_repository_drops_its_tags() {
    let app = server().await;
    let alice = token_for(&app, "alice").await;
    make_repo(&app, &alice, "game", "public").await;
    commit(&app, &alice, "alice/game", "a.txt", "one").await;
    tag_call(
        &app,
        &alice,
        "POST",
        "alice/game",
        "",
        Some(json!({"name": "v1"})),
    )
    .await;
    let r = call(&app, &alice, "DELETE", "/v1/repos/alice/game", None).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    make_repo(&app, &alice, "game", "public").await;
    let listed = tag_call(&app, &alice, "GET", "alice/game", "", None).await;
    assert!(listed.json::<Vec<api::Tag>>().is_empty());
}
