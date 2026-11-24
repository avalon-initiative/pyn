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

async fn star(app: &Router, who: &str, method: &str, repo: &str) -> Reply {
    call(app, who, method, &format!("/v1/repos/{repo}/star"), None).await
}

async fn info(app: &Router, who: &str, repo: &str) -> api::RepoInfo {
    call(app, who, "GET", &format!("/v1/repos/{repo}"), None)
        .await
        .json()
}

async fn starred(app: &Router, who: &str, user: &str) -> Vec<String> {
    let r = call(app, who, "GET", &format!("/v1/users/{user}/starred"), None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json::<Vec<api::StarredRepo>>()
        .into_iter()
        .map(|s| format!("{}/{}", s.repo.owner, s.repo.name))
        .collect()
}

#[tokio::test]
async fn starring_marks_a_repository_once_and_counts_it() {
    let app = server().await;
    let (alice, bob) = (token_for(&app, "alice").await, token_for(&app, "bob").await);
    make_repo(&app, &alice, "game", "public").await;

    let r = star(&app, &bob, "GET", "alice/game").await;
    assert_eq!(
        r.json::<api::StarState>(),
        api::StarState {
            starred: false,
            count: 0
        }
    );
    for _ in 0..2 {
        let r = star(&app, &bob, "PUT", "alice/game").await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text());
        assert_eq!(
            r.json::<api::StarState>(),
            api::StarState {
                starred: true,
                count: 1
            }
        );
    }
    let r = star(&app, &alice, "PUT", "alice/game").await;
    assert_eq!(r.json::<api::StarState>().count, 2);

    let as_bob = info(&app, &bob, "alice/game").await;
    assert_eq!((as_bob.star_count, as_bob.starred), (2, true));
    let signed_out = info(&app, "", "alice/game").await;
    assert_eq!((signed_out.star_count, signed_out.starred), (2, false));
    let listed = call(&app, &alice, "GET", "/v1/repos", None).await;
    assert_eq!(listed.json::<Vec<api::RepoInfo>>()[0].star_count, 2);
    let explored = call(&app, "", "GET", "/v1/explore/repos", None).await;
    assert_eq!(explored.json::<api::RepoPage>().repos[0].star_count, 2);

    for _ in 0..2 {
        let r = star(&app, &bob, "DELETE", "alice/game").await;
        assert_eq!(
            r.json::<api::StarState>(),
            api::StarState {
                starred: false,
                count: 1
            }
        );
    }
    assert_eq!(info(&app, &bob, "alice/game").await.star_count, 1);
}

#[tokio::test]
async fn only_someone_who_can_read_a_repository_can_star_it_or_see_its_count() {
    let app = server().await;
    let (alice, bob, carol) = (
        token_for(&app, "alice").await,
        token_for(&app, "bob").await,
        token_for(&app, "carol").await,
    );
    make_repo(&app, &alice, "vault", "private").await;
    for who in [&bob, &carol, ""] {
        let r = star(&app, who, "PUT", "alice/vault").await;
        assert!(
            matches!(r.status, StatusCode::NOT_FOUND | StatusCode::UNAUTHORIZED),
            "{}",
            r.text()
        );
    }
    let r = star(&app, &bob, "PUT", "alice/vault").await;
    assert_eq!(r.code(), "repo_not_found");
    let r = star(&app, &bob, "GET", "alice/vault").await;
    assert_eq!(r.code(), "repo_not_found");

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
    star(&app, &alice, "PUT", "alice/vault").await;
    let r = star(&app, &bob, "PUT", "alice/vault").await;
    assert_eq!(r.json::<api::StarState>().count, 2);
    assert_eq!(info(&app, &bob, "alice/vault").await.star_count, 2);

    let r = call(&app, &carol, "GET", "/v1/repos/alice/vault", None).await;
    assert_eq!(
        r.code(),
        "repo_not_found",
        "no count for someone with no access"
    );
    assert_eq!(starred(&app, &carol, "bob").await, Vec::<String>::new());
    assert_eq!(starred(&app, "", "bob").await, Vec::<String>::new());
    assert_eq!(starred(&app, &alice, "bob").await, ["alice/vault"]);
    assert_eq!(starred(&app, &bob, "bob").await, ["alice/vault"]);
}

#[tokio::test]
async fn a_users_starred_list_is_newest_first_and_follows_the_profile_setting() {
    let app = server().await;
    let (alice, bob) = (token_for(&app, "alice").await, token_for(&app, "bob").await);
    for name in ["a", "b", "c"] {
        make_repo(&app, &alice, name, "public").await;
    }
    for name in ["b", "a", "c"] {
        star(&app, &bob, "PUT", &format!("alice/{name}")).await;
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    let expected = ["alice/c", "alice/a", "alice/b"];
    assert_eq!(starred(&app, "", "bob").await, expected);
    let r = call(&app, &alice, "GET", "/v1/users/bob/starred", None).await;
    let first = &r.json::<Vec<api::StarredRepo>>()[0];
    assert_eq!(first.repo.role.as_deref(), Some("admin"));
    assert_eq!(first.repo.star_count, 1);

    let hide = json!({"links": [], "visibility": {"starred": false}});
    let r = call(&app, &bob, "PUT", "/v1/me/profile", Some(hide)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(starred(&app, &alice, "bob").await, Vec::<String>::new());
    assert_eq!(starred(&app, "", "bob").await, Vec::<String>::new());
    assert_eq!(
        starred(&app, &bob, "bob").await,
        expected,
        "the owner sees their own"
    );

    let r = call(&app, &alice, "GET", "/v1/users/nobody/starred", None).await;
    assert_eq!(r.code(), "user_not_found");
}

#[tokio::test]
async fn deleting_a_repository_drops_its_stars() {
    let app = server().await;
    let (alice, bob) = (token_for(&app, "alice").await, token_for(&app, "bob").await);
    make_repo(&app, &alice, "game", "public").await;
    star(&app, &bob, "PUT", "alice/game").await;
    let r = call(&app, &alice, "DELETE", "/v1/repos/alice/game", None).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert_eq!(starred(&app, &bob, "bob").await, Vec::<String>::new());
    make_repo(&app, &alice, "game", "public").await;
    assert_eq!(info(&app, &bob, "alice/game").await.star_count, 0);
}

#[tokio::test]
async fn starring_needs_a_sign_in_and_is_rate_limited() {
    let app = server().await;
    let (alice, bob) = (token_for(&app, "alice").await, token_for(&app, "bob").await);
    make_repo(&app, &alice, "game", "public").await;
    let r = star(&app, "", "PUT", "alice/game").await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let mut last = StatusCode::OK;
    for _ in 0..121 {
        last = star(&app, &bob, "PUT", "alice/game").await.status;
    }
    assert_eq!(last, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        star(&app, &bob, "GET", "alice/game").await.status,
        StatusCode::OK,
        "reading is not limited"
    );
}
