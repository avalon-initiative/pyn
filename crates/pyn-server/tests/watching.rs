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

async fn watch(app: &Router, who: &str, method: &str, repo: &str, level: Option<&str>) -> Reply {
    let body = level.map(|l| json!({ "level": l }));
    call(app, who, method, &format!("/v1/repos/{repo}/watch"), body).await
}

async fn info(app: &Router, who: &str, repo: &str) -> api::RepoInfo {
    call(app, who, "GET", &format!("/v1/repos/{repo}"), None)
        .await
        .json()
}

async fn watching(app: &Router, who: &str) -> Vec<(String, api::WatchLevel)> {
    let r = call(app, who, "GET", "/v1/me/watching", None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json::<Vec<api::WatchedRepo>>()
        .into_iter()
        .map(|w| (format!("{}/{}", w.repo.owner, w.repo.name), w.level))
        .collect()
}

fn state(level: Option<api::WatchLevel>, watchers: u64) -> api::WatchState {
    api::WatchState { level, watchers }
}

#[tokio::test]
async fn a_level_is_set_replaced_cleared_and_counted() {
    let app = server().await;
    let (alice, bob) = (token_for(&app, "alice").await, token_for(&app, "bob").await);
    make_repo(&app, &alice, "game", "public").await;

    let r = watch(&app, &bob, "GET", "alice/game", None).await;
    assert_eq!(
        r.json::<api::WatchState>(),
        state(None, 1),
        "the creator watches by default"
    );
    let r = watch(&app, &bob, "PUT", "alice/game", Some("locks")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(
        r.json::<api::WatchState>(),
        state(Some(api::WatchLevel::Locks), 2)
    );
    let r = watch(&app, &bob, "PUT", "alice/game", Some("all")).await;
    assert_eq!(
        r.json::<api::WatchState>(),
        state(Some(api::WatchLevel::All), 2)
    );

    let as_bob = info(&app, &bob, "alice/game").await;
    assert_eq!(
        (as_bob.watching, as_bob.watcher_count),
        (Some(api::WatchLevel::All), 2)
    );
    let signed_out = info(&app, "", "alice/game").await;
    assert_eq!((signed_out.watching, signed_out.watcher_count), (None, 2));
    let listed = call(&app, &alice, "GET", "/v1/repos", None).await;
    assert_eq!(
        listed.json::<Vec<api::RepoInfo>>()[0].watching,
        Some(api::WatchLevel::All)
    );
    let explored = call(&app, "", "GET", "/v1/explore/repos", None).await;
    assert_eq!(explored.json::<api::RepoPage>().repos[0].watcher_count, 2);

    let r = watch(&app, &bob, "PUT", "alice/game", Some("ignore")).await;
    assert_eq!(
        r.json::<api::WatchState>(),
        state(Some(api::WatchLevel::Ignore), 1),
        "an ignoring account is not counted"
    );
    for _ in 0..2 {
        let r = watch(&app, &bob, "DELETE", "alice/game", None).await;
        assert_eq!(r.json::<api::WatchState>(), state(None, 1));
    }
    let r = watch(&app, &bob, "PUT", "alice/game", Some("sometimes")).await;
    assert!(r.status.is_client_error(), "{}", r.text());
}

#[tokio::test]
async fn only_someone_who_can_read_a_repository_can_watch_it() {
    let app = server().await;
    let (alice, bob, carol) = (
        token_for(&app, "alice").await,
        token_for(&app, "bob").await,
        token_for(&app, "carol").await,
    );
    make_repo(&app, &alice, "vault", "private").await;
    for who in [&bob, &carol, ""] {
        let r = watch(&app, who, "PUT", "alice/vault", Some("all")).await;
        assert!(
            matches!(r.status, StatusCode::NOT_FOUND | StatusCode::UNAUTHORIZED),
            "{}",
            r.text()
        );
    }
    let r = watch(&app, &bob, "PUT", "alice/vault", Some("all")).await;
    assert_eq!(r.code(), "repo_not_found");
    let r = watch(&app, &bob, "GET", "alice/vault", None).await;
    assert_eq!(r.code(), "repo_not_found");
    assert!(watching(&app, &bob).await.is_empty());

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
    let r = watch(&app, &bob, "PUT", "alice/vault", Some("locks")).await;
    assert_eq!(r.json::<api::WatchState>().watchers, 2);
    assert_eq!(
        watching(&app, &bob).await,
        [("alice/vault".to_string(), api::WatchLevel::Locks)]
    );
    let r = call(&app, &carol, "GET", "/v1/repos/alice/vault", None).await;
    assert_eq!(
        r.code(),
        "repo_not_found",
        "no count for someone with no access"
    );
}

#[tokio::test]
async fn losing_read_access_hides_a_watched_repository_until_it_returns() {
    let app = server().await;
    let (alice, bob) = (token_for(&app, "alice").await, token_for(&app, "bob").await);
    make_repo(&app, &alice, "game", "public").await;
    watch(&app, &bob, "PUT", "alice/game", Some("all")).await;
    assert_eq!(watching(&app, &bob).await.len(), 1);

    let r = call(
        &app,
        &alice,
        "PATCH",
        "/v1/repos/alice/game",
        Some(json!({"visibility": "private"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert!(watching(&app, &bob).await.is_empty());

    let r = call(
        &app,
        &alice,
        "PATCH",
        "/v1/repos/alice/game",
        Some(json!({"visibility": "public"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    assert_eq!(watching(&app, &bob).await.len(), 1, "the level was kept");
}

#[tokio::test]
async fn the_list_names_every_level_newest_change_first() {
    let app = server().await;
    let (alice, bob) = (token_for(&app, "alice").await, token_for(&app, "bob").await);
    for name in ["a", "b", "c"] {
        make_repo(&app, &alice, name, "public").await;
    }
    for (name, level) in [("b", "locks"), ("a", "ignore"), ("c", "all")] {
        watch(&app, &bob, "PUT", &format!("alice/{name}"), Some(level)).await;
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert_eq!(
        watching(&app, &bob).await,
        [
            ("alice/c".to_string(), api::WatchLevel::All),
            ("alice/a".to_string(), api::WatchLevel::Ignore),
            ("alice/b".to_string(), api::WatchLevel::Locks),
        ]
    );
    let r = call(&app, "", "GET", "/v1/me/watching", None).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn deleting_a_repository_drops_its_watches() {
    let app = server().await;
    let (alice, bob) = (token_for(&app, "alice").await, token_for(&app, "bob").await);
    make_repo(&app, &alice, "game", "public").await;
    watch(&app, &bob, "PUT", "alice/game", Some("all")).await;
    let r = call(&app, &alice, "DELETE", "/v1/repos/alice/game", None).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert!(watching(&app, &bob).await.is_empty());
    make_repo(&app, &alice, "game", "public").await;
    let again = info(&app, &bob, "alice/game").await;
    assert_eq!((again.watching, again.watcher_count), (None, 1));
}

#[tokio::test]
async fn watching_needs_a_sign_in_and_is_rate_limited() {
    let app = server().await;
    let (alice, bob) = (token_for(&app, "alice").await, token_for(&app, "bob").await);
    make_repo(&app, &alice, "game", "public").await;
    let r = watch(&app, "", "PUT", "alice/game", Some("all")).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let mut last = StatusCode::OK;
    for _ in 0..121 {
        last = watch(&app, &bob, "PUT", "alice/game", Some("all"))
            .await
            .status;
    }
    assert_eq!(last, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        watch(&app, &bob, "GET", "alice/game", None).await.status,
        StatusCode::OK,
        "reading is not limited"
    );
}
