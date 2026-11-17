//! `GET /v1/me` and `GET /v1/account/deletion` per credential kind: a credential that is not full-scope never reads
//! the owner's hidden profile fields, visibility settings or `has_password`, nor the deletion status.

#[path = "common/cred.rs"]
mod cred;
use cred::{Cred, WithCred};
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore,
};
use pyn_core::{
    AccessConfig, AccessService, AuthProvider, RegistrationMode, Repositories, Rules, SystemClock,
    UserId,
};
use pyn_proto as api;
use pyn_server::auth::{BearerAuth, DevHeaderAuth};
use pyn_server::{AppState, router};
use serde_json::json;
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";
const BIO: &str = "secret bio";

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
    })
}

struct Reply {
    status: StatusCode,
    set_cookie: Option<String>,
    body: Vec<u8>,
}

impl Reply {
    fn json<T: serde::de::DeserializeOwned>(&self) -> T {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&self.body)))
    }

    fn raw(&self) -> serde_json::Value {
        self.json()
    }

    fn code(&self) -> String {
        self.json::<api::ErrorBody>().code
    }
}

async fn call(
    app: &Router,
    cred: &Cred,
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
) -> Reply {
    let mut b = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    b = b.cred(cred);
    let body = body.map(|b| b.to_string()).unwrap_or_default();
    let r = app
        .clone()
        .oneshot(b.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let (parts, body) = r.into_parts();
    Reply {
        status: parts.status,
        set_cookie: parts
            .headers
            .get("set-cookie")
            .map(|v| v.to_str().unwrap().to_string()),
        body: body.collect().await.unwrap().to_bytes().to_vec(),
    }
}

struct World {
    app: Router,
    session: Cred,
    full: Cred,
    scoped: Cred,
    read_only: Cred,
    service: Cred,
}

async fn token(app: &Router, who: &Cred, perms: &[&str]) -> Cred {
    let body = json!({"name": "ci", "permissions": perms, "repos": ["alice/game"]});
    let r = call(app, who, "POST", "/v1/tokens", Some(body)).await;
    assert!(
        r.status.is_success(),
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    Cred::token(&r.json::<api::CreatedToken>().token)
}

/// alice has a hidden bio and a repository; each credential kind of hers (and a service credential) is minted.
async fn world() -> World {
    let app = server().await;
    let body = json!({"username": "alice", "password": PASSWORD});
    let r = call(
        &app,
        &Cred::SignedOut,
        "POST",
        "/v1/register",
        Some(body.clone()),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED);
    let r = call(
        &app,
        &Cred::SignedOut,
        "POST",
        "/v1/login",
        Some(body.clone()),
    )
    .await;
    let full = Cred::token(&r.json::<api::CreatedToken>().token);
    let r = call(&app, &Cred::SignedOut, "POST", "/v1/session", Some(body)).await;
    assert_eq!(r.status, StatusCode::OK);
    let cookie = r.set_cookie.clone().unwrap();
    let cookie = cookie.split(';').next().unwrap();
    let session = Cred::session_read(cookie.strip_prefix("pyn_session=").unwrap());

    let r = call(
        &app,
        &full,
        "PUT",
        "/v1/me/profile",
        Some(json!({"bio": BIO, "company": "Acme", "visibility": {"bio": false}})),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let r = call(
        &app,
        &full,
        "POST",
        "/v1/repos",
        Some(json!({"owner": "alice", "name": "game"})),
    )
    .await;
    assert!(
        r.status.is_success(),
        "{}",
        String::from_utf8_lossy(&r.body)
    );

    let scoped = token(&app, &full, &["read", "lock", "checkin"]).await;
    let read_only = token(&app, &full, &["read"]).await;
    let r = call(
        &app,
        &Cred::dev("root"),
        "POST",
        "/v1/admin/service-credentials",
        Some(json!({"name": "billing", "scopes": ["manage_accounts"]})),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let service = Cred::service(&r.json::<api::CreatedServiceCredential>().secret);
    World {
        app,
        session,
        full,
        scoped,
        read_only,
        service,
    }
}

fn assert_full_view(r: &Reply, label: &str) {
    assert_eq!(
        r.status,
        StatusCode::OK,
        "{label}: {}",
        String::from_utf8_lossy(&r.body)
    );
    let me: api::Account = r.json();
    assert_eq!(me.user, "alice", "{label}");
    assert_eq!(me.has_password, Some(true), "{label}");
    assert_eq!(me.profile.bio.as_deref(), Some(BIO), "{label}");
    assert_eq!(me.profile.company.as_deref(), Some("Acme"), "{label}");
    let v = me.profile.visibility.unwrap_or_else(|| panic!("{label}"));
    assert!(!v.bio, "{label}");
}

fn assert_reduced_view(r: &Reply, label: &str) {
    assert_eq!(
        r.status,
        StatusCode::OK,
        "{label}: {}",
        String::from_utf8_lossy(&r.body)
    );
    let me: api::Account = r.json();
    assert_eq!(me.user, "alice", "{label}");
    assert!(!me.id.is_empty() && !me.admin, "{label}");
    assert_eq!(me.profile.bio, None, "{label}: hidden field");
    assert_eq!(
        me.profile.company.as_deref(),
        Some("Acme"),
        "{label}: public field"
    );
    assert_eq!(me.profile.visibility, None, "{label}");
    let raw = r.raw();
    assert!(raw.get("has_password").is_none(), "{label}: {raw}");
    assert!(raw["profile"].get("visibility").is_none(), "{label}: {raw}");
    assert!(!String::from_utf8_lossy(&r.body).contains(BIO), "{label}");
}

#[tokio::test]
async fn me_gives_a_full_scope_credential_the_full_own_view() {
    let w = world().await;
    for (label, cred) in [("session", &w.session), ("full token", &w.full)] {
        assert_full_view(&call(&w.app, cred, "GET", "/v1/me", None).await, label);
    }
    let dev = call(&w.app, &Cred::dev("root"), "GET", "/v1/me", None).await;
    let me: api::Account = dev.json();
    assert_eq!((me.user.as_str(), me.admin), ("root", true));
    assert_eq!(me.has_password, Some(false));
    assert!(me.profile.visibility.is_some());
}

#[tokio::test]
async fn me_gives_a_narrower_token_only_the_public_view() {
    let w = world().await;
    for (label, cred) in [("scoped", &w.scoped), ("read-only", &w.read_only)] {
        assert_reduced_view(&call(&w.app, cred, "GET", "/v1/me", None).await, label);
    }
}

#[tokio::test]
async fn me_refuses_signed_out_and_service_credentials() {
    let w = world().await;
    let r = call(&w.app, &Cred::SignedOut, "GET", "/v1/me", None).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r = call(&w.app, &w.service, "GET", "/v1/me", None).await;
    assert_eq!(
        r.status,
        StatusCode::FORBIDDEN,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    assert_eq!(r.code(), "service_credential_not_allowed");
}

#[tokio::test]
async fn deletion_status_needs_a_full_scope_credential() {
    let w = world().await;
    for (label, cred) in [("session", &w.session), ("full token", &w.full)] {
        let r = call(&w.app, cred, "GET", "/v1/account/deletion", None).await;
        assert_eq!(r.status, StatusCode::OK, "{label}");
        let s: api::AccountDeletionStatus = r.json();
        assert_eq!((s.user.as_str(), s.pending), ("alice", false), "{label}");
    }
    let r = call(
        &w.app,
        &Cred::dev("alice"),
        "GET",
        "/v1/account/deletion",
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json::<api::AccountDeletionStatus>().user, "alice");

    let r = call(
        &w.app,
        &Cred::SignedOut,
        "GET",
        "/v1/account/deletion",
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r = call(&w.app, &w.service, "GET", "/v1/account/deletion", None).await;
    assert_eq!(r.code(), "service_credential_not_allowed");
    for (label, cred) in [("scoped", &w.scoped), ("read-only", &w.read_only)] {
        let r = call(&w.app, cred, "GET", "/v1/account/deletion", None).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{label}");
        assert_eq!(r.code(), "forbidden", "{label}");
        let r = call(&w.app, cred, "POST", "/v1/account/deletion/restore", None).await;
        assert_eq!(r.code(), "forbidden", "{label}: restore agrees");
    }
}

#[tokio::test]
async fn a_pending_accounts_own_session_still_reads_the_status() {
    let w = world().await;
    let body = json!({"username": "carol", "password": PASSWORD});
    call(
        &w.app,
        &Cred::SignedOut,
        "POST",
        "/v1/register",
        Some(body.clone()),
    )
    .await;
    let r = call(
        &w.app,
        &Cred::SignedOut,
        "POST",
        "/v1/login",
        Some(body.clone()),
    )
    .await;
    let token = Cred::token(&r.json::<api::CreatedToken>().token);
    let r = call(&w.app, &Cred::SignedOut, "POST", "/v1/session", Some(body)).await;
    let set = r.set_cookie.unwrap();
    let cookie = set.split(';').next().unwrap().strip_prefix("pyn_session=");
    let session = Cred::session_read(cookie.unwrap());

    let schedule = json!({"password": PASSWORD});
    let r = call(
        &w.app,
        &token,
        "POST",
        "/v1/account/deletion",
        Some(schedule),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    for (label, cred) in [("session", &session), ("token", &token)] {
        let r = call(&w.app, cred, "GET", "/v1/account/deletion", None).await;
        assert_eq!(r.status, StatusCode::OK, "{label}");
        let s: api::AccountDeletionStatus = r.json();
        assert_eq!((s.user.as_str(), s.pending), ("carol", true), "{label}");
        let r = call(&w.app, cred, "GET", "/v1/me", None).await;
        assert_eq!(r.code(), "account_pending_deletion", "{label}");
    }
}
