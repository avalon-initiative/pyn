//! The HTTP contract of a repository's own storage cap and usage breakdown: routes, error code, access and downgrade.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore,
};
use pyn_core::{
    AccessConfig, AccessService, AuthProvider, Limits, RegistrationMode, Repositories, Rules,
    SystemClock, UserId,
};
use pyn_proto as api;
use pyn_server::auth::{BearerAuth, DevHeaderAuth};
use pyn_server::{AppState, router};
use serde_json::json;
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";

async fn server(default_limits: Limits) -> Router {
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
            require_email_verification: false,
            default_limits,
            ..AccessConfig::default()
        })
        .with_registry(meta.clone())
        .with_audit(audit.clone()),
    );
    access.admin_for_tests(&UserId::new("root")).await.unwrap();
    let repos = Repositories::new(
        meta,
        objects.clone(),
        audit,
        access.clone(),
        clock,
        Rules::from_toml("[meta]\ndefault = \"shared\"\n").unwrap(),
    );
    let repos = Arc::new(repos);
    router(AppState {
        repos,
        objects,
        access: access.clone(),
        auth: Arc::new(BearerAuth { access }),
        dev_auth: Some(Arc::new(DevHeaderAuth) as Arc<dyn AuthProvider>),
        trust_forwarded: false,
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

    fn is(&self, status: StatusCode, code: &str) -> bool {
        self.status == status && self.json::<api::ErrorBody>().code == code
    }
}

/// `who` is a dev-header user name, or a bearer secret (`pyn_...` token or `pyns_...` service credential).
async fn send(
    app: &Router,
    who: &str,
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
) -> Reply {
    let builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    let builder = if who.starts_with("pyn_") || who.starts_with("pyns_") {
        builder.header("authorization", format!("Bearer {who}"))
    } else {
        builder.header(api::DEV_USER_HEADER, who)
    };
    let req = builder
        .body(Body::from(body.map(|b| b.to_string()).unwrap_or_default()))
        .unwrap();
    let r = app.clone().oneshot(req).await.unwrap();
    Reply {
        status: r.status(),
        body: r.into_body().collect().await.unwrap().to_bytes().to_vec(),
    }
}

async fn put_object(app: &Router, who: &str, repo: &str, bytes: &str) -> String {
    let req = Request::builder()
        .method("PUT")
        .uri(format!("/v1/repos/{repo}/objects"))
        .header("authorization", format!("Bearer {who}"))
        .body(Body::from(bytes.to_string()))
        .unwrap();
    let r = app.clone().oneshot(req).await.unwrap();
    let body = r.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice::<api::PutObjectResponse>(&body)
        .unwrap()
        .content
}

/// Signs `name` up and returns a bearer token without admin rights.
async fn token_for(app: &Router, name: &str) -> String {
    let body = json!({"username": name, "password": PASSWORD});
    let r = send(app, "root", "POST", "/v1/register", Some(body.clone())).await;
    assert_eq!(r.status, StatusCode::CREATED);
    let r = send(app, "root", "POST", "/v1/login", Some(body)).await;
    r.json::<api::CreatedToken>().token
}

async fn service(app: &Router, name: &str, scopes: &[&str]) -> String {
    let r = send(
        app,
        "root",
        "POST",
        "/v1/admin/service-credentials",
        Some(json!({"name": name, "scopes": scopes})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED);
    r.json::<api::CreatedServiceCredential>().secret
}

async fn make_repo(app: &Router, who: &str, name: &str) -> Reply {
    send(app, who, "POST", "/v1/repos", Some(json!({"name": name}))).await
}

fn checkin(path: &str, content: &str, base: Option<u64>) -> serde_json::Value {
    json!({"path": path, "content": content, "base_revision": base, "message": "m"})
}

async fn write(app: &Router, who: &str, path: &str, bytes: &str, base: Option<u64>) -> Reply {
    let content = put_object(app, who, "alice/game", bytes).await;
    send(
        app,
        who,
        "POST",
        "/v1/repos/alice/game/checkin",
        Some(checkin(path, &content, base)),
    )
    .await
}

#[tokio::test]
async fn a_repository_cap_has_its_own_code_and_a_downgrade_deletes_nothing() {
    let app = server(Limits::default()).await;
    let alice = token_for(&app, "alice").await;
    make_repo(&app, &alice, "game").await;
    assert_eq!(
        write(&app, &alice, "a", "123456", None).await.status,
        StatusCode::OK
    );

    let r = send(
        &app,
        &alice,
        "PUT",
        "/v1/repos/alice/game/storage-cap",
        Some(json!({"storage_bytes": 8})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    let cap = r.json::<api::RepoStorageCap>();
    assert_eq!(
        (cap.repository.as_str(), cap.storage_bytes),
        ("alice/game", Some(8))
    );

    let r = write(&app, &alice, "b", "abcdef", None).await;
    assert!(
        r.is(StatusCode::CONFLICT, "repo_storage_limit_reached"),
        "{:?}",
        String::from_utf8_lossy(&r.body)
    );
    assert!(String::from_utf8_lossy(&r.body).contains("alice/game"));

    // Below current use: nothing is deleted, reads and bytes-free writes work, growth is refused.
    send(
        &app,
        &alice,
        "PUT",
        "/v1/repos/alice/game/storage-cap",
        Some(json!({"storage_bytes": 1})),
    )
    .await;
    let used = send(&app, &alice, "GET", "/v1/repos/alice/game/usage", None)
        .await
        .json::<api::RepoUsage>();
    assert_eq!((used.stored_bytes, used.storage_cap), (6, Some(1)));
    assert_eq!(
        write(&app, &alice, "c", "123456", None).await.status,
        StatusCode::OK
    );
    let r = write(&app, &alice, "d", "zz", None).await;
    assert!(r.is(StatusCode::CONFLICT, "repo_storage_limit_reached"));
    let r = send(&app, &alice, "GET", "/v1/repos/alice/game/files", None).await;
    assert_eq!(r.status, StatusCode::OK);

    let r = send(
        &app,
        &alice,
        "PUT",
        "/v1/repos/alice/game/storage-cap",
        Some(json!({"storage_bytes": null})),
    )
    .await;
    assert_eq!(r.json::<api::RepoStorageCap>().storage_bytes, None);
    assert_eq!(
        write(&app, &alice, "d", "zz", None).await.status,
        StatusCode::OK
    );

    let r = send(&app, "root", "GET", "/v1/owners/alice/usage", None).await;
    let listing = r.json::<api::OwnerUsage>();
    assert_eq!(listing.repositories[0].storage_cap, None);
}

#[tokio::test]
async fn only_the_owner_an_administrator_and_manage_limits_set_the_cap() {
    let app = server(Limits::default()).await;
    let alice = token_for(&app, "alice").await;
    let bob = token_for(&app, "bob").await;
    make_repo(&app, &alice, "game").await;
    let put = |who: String, uri: &'static str, n: u64| {
        let app = app.clone();
        async move { send(&app, &who, "PUT", uri, Some(json!({"storage_bytes": n}))).await }
    };
    let uri = "/v1/repos/alice/game/storage-cap";
    assert!(
        put(bob.clone(), uri, 5)
            .await
            .is(StatusCode::FORBIDDEN, "not_namespace_owner")
    );
    assert!(
        put(bob.clone(), "/v1/repos/alice/ghost/storage-cap", 5)
            .await
            .is(StatusCode::FORBIDDEN, "not_namespace_owner"),
        "a stranger learns nothing about which repositories exist"
    );
    let freeze_only = service(&app, "f", &["manage_freeze"]).await;
    assert!(
        put(freeze_only, uri, 5)
            .await
            .is(StatusCode::FORBIDDEN, "service_scope_required")
    );
    let billing = service(&app, "billing", &["manage_limits"]).await;
    assert_eq!(put(billing.clone(), uri, 5).await.status, StatusCode::OK);
    assert_eq!(put("root".into(), uri, 6).await.status, StatusCode::OK);
    assert_eq!(put(alice.clone(), uri, 7).await.status, StatusCode::OK);
    assert!(
        put(alice.clone(), uri, u64::MAX)
            .await
            .is(StatusCode::BAD_REQUEST, "invalid_request")
    );
    assert!(
        put(alice.clone(), "/v1/repos/alice/ghost/storage-cap", 5)
            .await
            .is(StatusCode::NOT_FOUND, "repo_not_found")
    );

    // A frozen owner cannot change its own cap; an operator still can.
    let freeze = service(&app, "freezer", &["manage_freeze"]).await;
    let r = send(
        &app,
        &freeze,
        "PUT",
        "/v1/admin/owners/alice/freeze",
        Some(json!({"reason": "lapsed"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(
        put(alice.clone(), uri, 9)
            .await
            .is(StatusCode::FORBIDDEN, "owner_frozen")
    );
    assert_eq!(put(billing, uri, 9).await.status, StatusCode::OK);
}

#[tokio::test]
async fn the_breakdown_is_for_repository_admins_and_lists_folders_and_paths() {
    let app = server(Limits::default()).await;
    let alice = token_for(&app, "alice").await;
    let bob = token_for(&app, "bob").await;
    make_repo(&app, &alice, "game").await;
    write(&app, &alice, "Art/a.png", "0123456789", None).await;
    write(&app, &alice, "Art/b.png", "0123456789", None).await;
    write(&app, &alice, "notes", "abc", None).await;
    let uri = "/v1/repos/alice/game/usage/breakdown";

    let r = send(&app, &alice, "GET", uri, None).await;
    assert_eq!(r.status, StatusCode::OK);
    let report = r.json::<api::RepoUsageBreakdown>();
    assert_eq!(
        (report.repository.as_str(), report.stored_bytes),
        ("alice/game", 13)
    );
    let folders: Vec<_> = report
        .folders
        .iter()
        .map(|f| (f.folder.as_str(), f.stored_bytes, f.files))
        .collect();
    assert_eq!(folders, [("Art", 10, 2), ("", 3, 1)]);
    let paths: Vec<_> = report
        .largest_paths
        .iter()
        .map(|p| p.path.as_str())
        .collect();
    assert_eq!(paths, ["Art/a.png", "Art/b.png", "notes"]);

    let r = send(
        &app,
        &alice,
        "PUT",
        "/v1/repos/alice/game/members/bob",
        Some(json!({"role": "writer"})),
    )
    .await;
    assert!(
        r.status.is_success(),
        "{:?}",
        String::from_utf8_lossy(&r.body)
    );
    let r = send(&app, &bob, "GET", uri, None).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(
        send(&app, &bob, "GET", "/v1/repos/alice/game/usage", None)
            .await
            .status,
        StatusCode::OK,
        "the plain usage stays readable by readers"
    );
    let carol = token_for(&app, "carol").await;
    assert_eq!(
        send(&app, &carol, "GET", uri, None).await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn the_owner_listing_is_largest_first_with_each_cap() {
    let app = server(Limits::default()).await;
    let alice = token_for(&app, "alice").await;
    make_repo(&app, &alice, "game").await;
    make_repo(&app, &alice, "tools").await;
    write(&app, &alice, "a", "123", None).await;
    let big = put_object(&app, &alice, "alice/tools", "123456789").await;
    send(
        &app,
        &alice,
        "POST",
        "/v1/repos/alice/tools/checkin",
        Some(checkin("b", &big, None)),
    )
    .await;
    let billing = service(&app, "billing", &["manage_limits"]).await;
    send(
        &app,
        &billing,
        "PUT",
        "/v1/repos/alice/game/storage-cap",
        Some(json!({"storage_bytes": 100})),
    )
    .await;
    let r = send(&app, &billing, "GET", "/v1/owners/alice/usage", None).await;
    let listing = r.json::<api::OwnerUsage>();
    let rows: Vec<_> = listing
        .repositories
        .iter()
        .map(|r| (r.repository.as_str(), r.stored_bytes, r.storage_cap))
        .collect();
    assert_eq!(
        rows,
        [("alice/tools", 9, None), ("alice/game", 3, Some(100))]
    );
}
