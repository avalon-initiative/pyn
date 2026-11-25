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
    AccessConfig, AccessMode, AccessService, AuthProvider, Limits, RateLimits, RegistrationMode,
    Repositories, Rules, SystemClock, UserId,
};
use pyn_proto as api;
use pyn_server::auth::{BearerAuth, DevHeaderAuth};
use pyn_server::{AppState, router};
use serde_json::json;
use sha2::Digest;
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";
const EMAIL: &str = "secret.person@mail.test";

async fn server() -> Router {
    build(Limits::default(), (10, 10)).await
}

async fn build(default_limits: Limits, fork_caps: (u64, u64)) -> Router {
    let (mode, limits, trust_forwarded) = (AccessMode::Public, RateLimits::default(), false);
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
            default_limits,
            ..AccessConfig::default()
        })
        .with_registry(meta.clone())
        .with_audit(audit.clone()),
    );
    access.admin_for_tests(&UserId::new("root")).await.unwrap();
    let repos = Arc::new(
        Repositories::new(
            meta,
            objects.clone(),
            audit,
            access.clone(),
            clock,
            Rules::empty(),
        )
        .with_fork_caps(fork_caps.0, fork_caps.1),
    );
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

async fn check_in(app: &Router, who: &str, repo: &str, path: &str, content: &[u8]) {
    check_in_on(app, who, repo, None, path, content).await;
}

async fn check_in_on(
    app: &Router,
    who: &str,
    repo: &str,
    branch: Option<&str>,
    path: &str,
    content: &[u8],
) {
    let r = send(
        app,
        who,
        "PUT",
        &format!("/v1/repos/{repo}/objects"),
        "application/octet-stream",
        content.to_vec(),
        &[],
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let blob = r.json::<api::PutObjectResponse>().content;
    let body = json!({"path": path, "content": blob, "message": path, "branch": branch});
    let r = call(
        app,
        who,
        "POST",
        &format!("/v1/repos/{repo}/checkin"),
        Some(body),
    )
    .await;
    assert!(r.status.is_success(), "{}", r.text());
}

async fn check_in_locked(app: &Router, who: &str, repo: &str, path: &str, content: &[u8]) {
    let r = call(
        app,
        who,
        "POST",
        &format!("/v1/repos/{repo}/checkout"),
        Some(json!({"path": path})),
    )
    .await;
    assert!(r.status.is_success(), "{}", r.text());
    check_in(app, who, repo, path, content).await;
}

async fn fork(app: &Router, who: &str, repo: &str, body: serde_json::Value) -> Reply {
    call(
        app,
        who,
        "POST",
        &format!("/v1/repos/{repo}/forks"),
        Some(body),
    )
    .await
}

#[tokio::test]
async fn a_fork_copies_files_and_history_links_its_source_and_starts_without_locks() {
    let app = server().await;
    let (alice, bob) = (token_for(&app, "alice").await, token_for(&app, "bob").await);
    let body = json!({"name": "game", "visibility": "public", "description": "A game", "topics": ["rust"]});
    let r = call(&app, &alice, "POST", "/v1/repos", Some(body)).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    check_in(&app, &alice, "alice/game", "src/main.rs", b"fn main() {}\n").await;
    let r = call(
        &app,
        &alice,
        "POST",
        "/v1/repos/alice/game/checkout",
        Some(json!({"path": "Content/map.umap"})),
    )
    .await;
    assert!(r.status.is_success(), "{}", r.text());

    let r = fork(&app, &bob, "alice/game", json!({})).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let made: api::RepoInfo = r.json();
    assert_eq!(format!("{}/{}", made.owner, made.name), "bob/game");
    assert_eq!(
        made.visibility,
        api::Visibility::Public,
        "the root's visibility"
    );
    assert_eq!(
        made.fork_of,
        Some(api::RepoRef {
            owner: "alice".into(),
            name: "game".into()
        })
    );
    assert_eq!(made.role.as_deref(), Some("admin"));
    assert_eq!(made.description.as_deref(), Some("A game"));
    assert!(made.topics.is_empty());
    assert_eq!(made.languages, info(&app, "", "alice/game").await.languages);

    let r = call(
        &app,
        &bob,
        "GET",
        "/v1/repos/bob/game/content?path=src/main.rs",
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let history = call(&app, &bob, "GET", "/v1/repos/bob/game/history", None).await;
    assert_eq!(history.status, StatusCode::OK, "{}", history.text());
    let locks: Vec<serde_json::Value> = call(&app, &bob, "GET", "/v1/repos/bob/game/locks", None)
        .await
        .json();
    assert!(
        locks.is_empty(),
        "a lock on the source means nothing to the fork"
    );
    let r = call(
        &app,
        &bob,
        "POST",
        "/v1/repos/bob/game/checkout",
        Some(json!({"path": "Content/map.umap"})),
    )
    .await;
    assert!(r.status.is_success(), "{}", r.text());

    assert_eq!(info(&app, "", "alice/game").await.fork_count, 1);
    let r = fork(&app, &bob, "alice/game", json!({"name": "game2"})).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = fork(&app, &bob, "alice/game", json!({})).await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::CONFLICT, "repo_exists".into())
    );
    let forks: Vec<api::RepoInfo> = call(&app, "", "GET", "/v1/repos/alice/game/forks", None)
        .await
        .json();
    let names: Vec<_> = forks
        .iter()
        .map(|f| format!("{}/{}", f.owner, f.name))
        .collect();
    assert_eq!(names, ["bob/game", "bob/game2"]);

    let r = call(&app, &alice, "DELETE", "/v1/repos/alice/game", None).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text());
    let orphan = info(&app, &bob, "bob/game").await;
    assert_eq!(orphan.fork_of, None, "the link goes with a deleted source");
    let r = call(
        &app,
        &bob,
        "GET",
        "/v1/repos/bob/game/content?path=src/main.rs",
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "the copy stays");
}

#[tokio::test]
async fn a_private_repository_forks_only_when_allowed_and_its_forks_stay_private() {
    let app = server().await;
    let (alice, bob, carol) = (
        token_for(&app, "alice").await,
        token_for(&app, "bob").await,
        token_for(&app, "carol").await,
    );
    make_repo(&app, &alice, "secret", "private").await;
    let r = call(
        &app,
        &alice,
        "PUT",
        "/v1/repos/alice/secret/members/bob",
        Some(json!({"role": "reader"})),
    )
    .await;
    assert!(r.status.is_success(), "{}", r.text());

    let r = fork(&app, &carol, "alice/secret", json!({})).await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::NOT_FOUND, "repo_not_found".into()),
        "hidden"
    );
    let r = fork(&app, &bob, "alice/secret", json!({})).await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::FORBIDDEN, "fork_forbidden".into())
    );

    let r = patch(
        &app,
        &alice,
        "alice/secret",
        json!({"allow_private_forks": true}),
    )
    .await;
    assert!(r.json::<api::RepoInfo>().allow_private_forks);
    let r = fork(&app, &bob, "alice/secret", json!({})).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let made: api::RepoInfo = r.json();
    assert_eq!(made.visibility, api::Visibility::Private);
    assert!(!made.allow_private_forks, "the setting is not copied");
    assert_eq!(made.fork_of.unwrap().name, "secret");

    let r = patch(&app, &bob, "bob/secret", json!({"visibility": "public"})).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text());
    let r = patch(
        &app,
        &alice,
        "alice/secret",
        json!({"visibility": "public"}),
    )
    .await;
    assert!(r.status.is_success(), "{}", r.text());
    assert_eq!(
        info(&app, &bob, "bob/secret").await.visibility,
        api::Visibility::Private,
        "a root's later change leaves its forks as they are"
    );
    let r = patch(&app, &bob, "bob/secret", json!({"visibility": "public"})).await;
    assert!(
        r.status.is_success(),
        "a public root lets its fork go public: {}",
        r.text()
    );
}

#[tokio::test]
async fn a_fork_over_the_storage_or_fork_limits_is_refused_with_a_clear_message() {
    let limits = Limits {
        storage_bytes: Some(20),
        ..Limits::default()
    };
    let app = build(limits, (2, 1)).await;
    let (alice, bob, carol, dave) = (
        token_for(&app, "alice").await,
        token_for(&app, "bob").await,
        token_for(&app, "carol").await,
        token_for(&app, "dave").await,
    );
    let body = json!({"name": "game", "visibility": "public", "starter_policy": false});
    assert_eq!(
        call(&app, &alice, "POST", "/v1/repos", Some(body))
            .await
            .status,
        StatusCode::CREATED
    );
    let body = json!({"name": "tools", "visibility": "public", "starter_policy": false});
    assert_eq!(
        call(&app, &alice, "POST", "/v1/repos", Some(body))
            .await
            .status,
        StatusCode::CREATED
    );
    check_in_locked(&app, &alice, "alice/game", "a.txt", b"twelve bytes").await;

    let r = call(
        &app,
        &bob,
        "POST",
        "/v1/repos",
        Some(json!({"name": "big", "starter_policy": false})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED);
    check_in_locked(&app, &bob, "bob/big", "b.txt", b"ten bytes!").await;
    let r = fork(&app, &bob, "alice/game", json!({})).await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::CONFLICT, "storage_limit_reached".into())
    );
    let msg = r.json::<api::ErrorBody>().message;
    assert!(msg.contains("the fork needs 12 bytes"), "{msg}");
    assert_eq!(
        call(&app, &bob, "GET", "/v1/repos/bob/game", None)
            .await
            .status,
        StatusCode::NOT_FOUND
    );

    let r = fork(&app, &carol, "alice/game", json!({})).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = fork(&app, &carol, "alice/tools", json!({})).await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::CONFLICT, "fork_limit_reached".into()),
        "per owner"
    );
    let r = fork(&app, &dave, "alice/game", json!({})).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    let r = fork(&app, &alice, "alice/game", json!({"name": "again"})).await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::CONFLICT, "fork_limit_reached".into()),
        "per repository"
    );
    assert!(r.json::<api::ErrorBody>().message.contains("alice/game"));
}

/// Uploads `bytes` as an LFS object of `repo` and checks in a pointer file to it at `path`.
async fn lfs_file(
    app: &Router,
    who: &str,
    repo: &str,
    branch: Option<&str>,
    path: &str,
    bytes: &[u8],
) {
    let oid = hex::encode(sha2::Sha256::digest(bytes));
    let len = bytes.len().to_string();
    let r = send(
        app,
        who,
        "PUT",
        &format!("/v1/repos/{repo}/info/lfs/objects/{oid}"),
        "application/octet-stream",
        bytes.to_vec(),
        &[("content-length", &len)],
    )
    .await;
    assert!(r.status.is_success(), "{}", r.text());
    let pointer =
        format!("version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize {len}\n");
    check_in_on(app, who, repo, branch, path, pointer.as_bytes()).await;
}

async fn ref_names(app: &Router, who: &str, repo: &str, kind: &str) -> Vec<String> {
    let r = call(app, who, "GET", &format!("/v1/repos/{repo}/{kind}"), None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let mut names: Vec<String> = r
        .json::<Vec<serde_json::Value>>()
        .iter()
        .map(|b| b["name"].as_str().unwrap().to_string())
        .collect();
    names.sort();
    names
}

async fn usage(app: &Router, who: &str, repo: &str) -> api::RepoUsage {
    let r = call(app, who, "GET", &format!("/v1/repos/{repo}/usage"), None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    r.json()
}

#[tokio::test]
async fn a_fork_copies_the_default_branch_only_unless_asked_for_every_branch() {
    let app = server().await;
    let (alice, bob, carol) = (
        token_for(&app, "alice").await,
        token_for(&app, "bob").await,
        token_for(&app, "carol").await,
    );
    make_repo(&app, &alice, "game", "public").await;
    check_in(
        &app,
        &alice,
        "alice/game",
        "main.txt",
        b"on the default branch\n",
    )
    .await;
    lfs_file(
        &app,
        &alice,
        "alice/game",
        None,
        "art/main.ptr",
        b"main asset",
    )
    .await;
    let r = call(
        &app,
        &alice,
        "POST",
        "/v1/repos/alice/game/branches",
        Some(json!({"name": "feature"})),
    )
    .await;
    assert!(r.status.is_success(), "{}", r.text());
    let side = b"only on the feature branch\n";
    check_in_on(
        &app,
        &alice,
        "alice/game",
        Some("feature"),
        "side.txt",
        side,
    )
    .await;
    let side_asset = b"side asset bytes";
    lfs_file(
        &app,
        &alice,
        "alice/game",
        Some("feature"),
        "art/side.ptr",
        side_asset,
    )
    .await;
    for body in [
        json!({"name": "v1"}),
        json!({"name": "f1", "target": "feature"}),
    ] {
        let r = call(
            &app,
            &alice,
            "POST",
            "/v1/repos/alice/game/tags",
            Some(body),
        )
        .await;
        assert!(r.status.is_success(), "{}", r.text());
    }

    let r = fork(&app, &bob, "alice/game", json!({})).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(
        ref_names(&app, &bob, "bob/game", "branches").await,
        ["main"]
    );
    assert_eq!(ref_names(&app, &bob, "bob/game", "tags").await, ["v1"]);
    let r = call(
        &app,
        &bob,
        "GET",
        "/v1/repos/bob/game/content?path=main.txt",
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());

    let r = fork(
        &app,
        &carol,
        "alice/game",
        json!({"default_branch_only": false}),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text());
    assert_eq!(
        ref_names(&app, &carol, "carol/game", "branches").await,
        ["feature", "main"]
    );
    assert_eq!(
        ref_names(&app, &carol, "carol/game", "tags").await,
        ["f1", "v1"]
    );

    let (only, all, source) = (
        usage(&app, &bob, "bob/game").await,
        usage(&app, &carol, "carol/game").await,
        usage(&app, &alice, "alice/game").await,
    );
    assert_eq!(
        (all.stored_bytes, all.lfs_bytes),
        (source.stored_bytes, source.lfs_bytes)
    );
    assert_eq!(
        only.lfs_bytes,
        b"main asset".len() as u64,
        "the asset the default branch names"
    );
    let side_pointer = format!(
        "version https://git-lfs.github.com/spec/v1\noid sha256:{}\nsize {}\n",
        hex::encode(sha2::Sha256::digest(side_asset)),
        side_asset.len()
    );
    assert_eq!(
        only.stored_bytes,
        all.stored_bytes - side.len() as u64 - side_pointer.len() as u64,
        "storage counts what was copied"
    );
}
