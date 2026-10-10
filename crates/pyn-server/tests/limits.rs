//! The HTTP contract of per-owner limits and usage: opt-in, distinct error codes, admin and service access.

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
        AccessService::new(Arc::new(MemoryAccessStore::new()), clock.clone())
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
    let repos = Arc::new(Repositories::new(
        meta,
        objects.clone(),
        audit,
        access.clone(),
        clock,
        Rules::from_toml("[meta]\ndefault = \"shared\"\n").unwrap(),
    ));
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

#[tokio::test]
async fn a_server_with_no_limits_set_reports_everything_unlimited_and_enforces_nothing() {
    let app = server(Limits::default()).await;
    let alice = token_for(&app, "alice").await;
    for i in 0..8 {
        assert_eq!(
            make_repo(&app, &alice, &format!("r{i}")).await.status,
            StatusCode::CREATED
        );
    }
    let r = send(&app, &alice, "GET", "/v1/owners/alice/limits", None).await;
    let limits = r.json::<api::OwnerLimits>();
    assert_eq!(limits.kind, api::OwnerKind::User);
    assert_eq!(limits.effective, api::Limits::default());
    assert_eq!(limits.own, api::Limits::default());

    let r = send(&app, &alice, "GET", "/v1/owners/alice/usage", None).await;
    let usage = r.json::<api::OwnerUsage>();
    assert_eq!(
        (
            usage.usage.repositories,
            usage.usage.members,
            usage.usage.stored_bytes
        ),
        (8, None, 0)
    );
    assert_eq!(usage.repositories.len(), 8);

    let r = send(&app, "root", "GET", "/v1/admin/limits", None).await;
    let listing = r.json::<api::LimitsListing>();
    assert_eq!(listing.defaults, api::Limits::default());
    assert!(listing.owners.is_empty());
}

#[tokio::test]
async fn the_repository_limit_is_set_by_an_administrator_and_refused_with_its_own_code() {
    let app = server(Limits::default()).await;
    let alice = token_for(&app, "alice").await;
    let r = send(
        &app,
        "root",
        "PATCH",
        "/v1/admin/owners/alice/limits",
        Some(json!({"repositories": 1})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json::<api::OwnerLimits>().effective.repositories, Some(1));

    assert_eq!(
        make_repo(&app, &alice, "one").await.status,
        StatusCode::CREATED
    );
    let r = make_repo(&app, &alice, "two").await;
    assert!(
        r.is(StatusCode::CONFLICT, "repo_limit_reached"),
        "{:?}",
        String::from_utf8_lossy(&r.body)
    );

    let shown = send(&app, &alice, "GET", "/v1/owners/alice/limits", None).await;
    assert_eq!(shown.json::<api::OwnerLimits>().own.repositories, Some(1));

    let r = send(
        &app,
        "root",
        "PATCH",
        "/v1/admin/owners/alice/limits",
        Some(json!({"repositories": null})),
    )
    .await;
    assert_eq!(
        r.json::<api::OwnerLimits>().effective,
        api::Limits::default()
    );
    assert_eq!(
        make_repo(&app, &alice, "two").await.status,
        StatusCode::CREATED
    );
}

#[tokio::test]
async fn an_absent_field_is_untouched_and_bad_values_are_refused() {
    let app = server(Limits::default()).await;
    token_for(&app, "alice").await;
    send(
        &app,
        "root",
        "PATCH",
        "/v1/admin/owners/alice/limits",
        Some(json!({"repositories": 3, "storage_bytes": 500})),
    )
    .await;
    let r = send(
        &app,
        "root",
        "PATCH",
        "/v1/admin/owners/alice/limits",
        Some(json!({"storage_bytes": null})),
    )
    .await;
    let own = r.json::<api::OwnerLimits>().own;
    assert_eq!((own.repositories, own.storage_bytes), (Some(3), None));

    for body in [
        json!({"repositories": -1}),
        json!({"repositories": 18446744073709551615u64}),
        json!({"members": 2}),
    ] {
        let r = send(
            &app,
            "root",
            "PATCH",
            "/v1/admin/owners/alice/limits",
            Some(body.clone()),
        )
        .await;
        assert!(r.status.is_client_error(), "{body}: {}", r.status);
    }
    let r = send(
        &app,
        "root",
        "PATCH",
        "/v1/admin/owners/ghost/limits",
        Some(json!({"repositories": 1})),
    )
    .await;
    assert!(r.is(StatusCode::NOT_FOUND, "user_not_found"));
}

#[tokio::test]
async fn storage_and_member_limits_have_their_own_codes() {
    let app = server(Limits::default()).await;
    let alice = token_for(&app, "alice").await;
    token_for(&app, "bob").await;
    token_for(&app, "carol").await;
    make_repo(&app, &alice, "game").await;
    send(
        &app,
        "root",
        "PATCH",
        "/v1/admin/owners/alice/limits",
        Some(json!({"storage_bytes": 8})),
    )
    .await;

    let small = put_object(&app, &alice, "alice/game", "12345").await;
    let checkin = |path: &str, content: &str| json!({"path": path, "content": content, "base_revision": null, "message": "m"});
    let r = send(
        &app,
        &alice,
        "POST",
        "/v1/repos/alice/game/checkin",
        Some(checkin("a", &small)),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    let more = put_object(&app, &alice, "alice/game", "67890").await;
    let r = send(
        &app,
        &alice,
        "POST",
        "/v1/repos/alice/game/checkin",
        Some(checkin("b", &more)),
    )
    .await;
    assert!(
        r.is(StatusCode::CONFLICT, "storage_limit_reached"),
        "{:?}",
        String::from_utf8_lossy(&r.body)
    );

    let r = send(&app, &alice, "GET", "/v1/repos/alice/game/usage", None).await;
    let used = r.json::<api::RepoUsage>();
    assert_eq!(
        (
            used.repository.as_str(),
            used.stored_bytes,
            used.files,
            used.revisions
        ),
        ("alice/game", 5, 1, 1)
    );

    send(
        &app,
        &alice,
        "POST",
        "/v1/orgs",
        Some(json!({"name": "acme"})),
    )
    .await;
    send(
        &app,
        "root",
        "PATCH",
        "/v1/admin/owners/acme/limits",
        Some(json!({"members": 2})),
    )
    .await;
    let r = send(
        &app,
        &alice,
        "POST",
        "/v1/orgs/acme/members",
        Some(json!({"user": "bob", "role": "member"})),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::CREATED,
        "{:?}",
        String::from_utf8_lossy(&r.body)
    );
    let r = send(
        &app,
        &alice,
        "POST",
        "/v1/orgs/acme/members",
        Some(json!({"user": "carol", "role": "member"})),
    )
    .await;
    assert!(r.is(StatusCode::CONFLICT, "member_limit_reached"));
    let r = send(&app, &alice, "GET", "/v1/owners/acme/usage", None).await;
    assert_eq!(r.json::<api::OwnerUsage>().usage.members, Some(2));
}

#[tokio::test]
async fn the_server_default_shows_in_every_owners_effective_limits() {
    let app = server(Limits {
        repositories: Some(2),
        members: Some(10),
        storage_bytes: None,
    })
    .await;
    let alice = token_for(&app, "alice").await;
    let r = send(&app, &alice, "GET", "/v1/owners/alice/limits", None).await;
    let limits = r.json::<api::OwnerLimits>();
    assert_eq!(limits.effective.repositories, Some(2));
    assert_eq!(limits.effective.members, None, "a user has no member limit");
    assert_eq!(limits.own, api::Limits::default());
    let r = send(&app, "root", "GET", "/v1/admin/limits", None).await;
    assert_eq!(r.json::<api::LimitsListing>().defaults.members, Some(10));
}

#[tokio::test]
async fn only_the_right_callers_may_set_or_read_limits() {
    let app = server(Limits::default()).await;
    let alice = token_for(&app, "alice").await;
    let bob = token_for(&app, "bob").await;
    let patch = Some(json!({"repositories": 1}));

    let r = send(
        &app,
        &alice,
        "PATCH",
        "/v1/admin/owners/alice/limits",
        patch.clone(),
    )
    .await;
    assert!(
        r.is(StatusCode::FORBIDDEN, "server_admin_required"),
        "an owner cannot raise their own limit"
    );
    let r = send(&app, &alice, "GET", "/v1/admin/limits", None).await;
    assert!(r.is(StatusCode::FORBIDDEN, "server_admin_required"));
    let r = send(&app, &bob, "GET", "/v1/owners/alice/limits", None).await;
    assert!(r.is(StatusCode::FORBIDDEN, "not_namespace_owner"));
    let r = send(&app, &bob, "GET", "/v1/owners/alice/usage", None).await;
    assert!(r.is(StatusCode::FORBIDDEN, "not_namespace_owner"));

    let narrow = service(&app, "accounts", &["manage_accounts"]).await;
    let r = send(
        &app,
        &narrow,
        "PATCH",
        "/v1/admin/owners/alice/limits",
        patch.clone(),
    )
    .await;
    assert!(r.is(StatusCode::FORBIDDEN, "service_scope_required"));
    let r = send(&app, &narrow, "GET", "/v1/owners/alice/usage", None).await;
    assert!(r.is(StatusCode::FORBIDDEN, "service_scope_required"));

    let billing = service(&app, "billing", &["manage_limits"]).await;
    let r = send(
        &app,
        &billing,
        "PATCH",
        "/v1/admin/owners/alice/limits",
        patch,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    let r = send(&app, &billing, "GET", "/v1/owners/alice/usage", None).await;
    assert_eq!(r.status, StatusCode::OK);
    let r = send(&app, &billing, "GET", "/v1/admin/limits", None).await;
    assert_eq!(r.json::<api::LimitsListing>().owners.len(), 1);
    let r = send(&app, &billing, "GET", "/v1/repos", None).await;
    assert!(r.is(StatusCode::FORBIDDEN, "service_credential_not_allowed"));
}

#[tokio::test]
async fn limit_changes_reach_the_server_audit_log() {
    let app = server(Limits::default()).await;
    token_for(&app, "alice").await;
    send(
        &app,
        "root",
        "PATCH",
        "/v1/admin/owners/alice/limits",
        Some(json!({"repositories": 4})),
    )
    .await;
    let r = send(&app, "root", "GET", "/v1/admin/audit", None).await;
    let page = r.json::<api::AuditPage>();
    let entry = page
        .entries
        .iter()
        .find(|e| e.action == "owner_limits_changed")
        .expect("an audit entry");
    assert_eq!(
        entry.detail,
        "alice: repositories 4, members default, storage bytes default"
    );
}
