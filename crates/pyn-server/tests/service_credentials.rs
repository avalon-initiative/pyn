//! The HTTP contract of service credentials: scopes, revocation, audit, and what they cannot reach.

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
};
use pyn_proto as api;
use pyn_server::auth::{BearerAuth, DevHeaderAuth};
use pyn_server::{AppState, router};
use serde_json::json;
use tower::ServiceExt;

fn app() -> Router {
    build(false)
}

fn app_behind_proxy() -> Router {
    build(true)
}

fn build(trust_forwarded: bool) -> Router {
    let clock = Arc::new(SystemClock);
    let objects = Arc::new(MemoryObjectStore::new());
    let audit = Arc::new(MemoryAuditStore::new());
    let cfg = AccessConfig {
        registration: RegistrationMode::Open,
        require_email_verification: false,
        ..AccessConfig::default()
    };
    let access = Arc::new(
        AccessService::new(
            Arc::new(MemoryAccessStore::new()),
            clock.clone(),
            Arc::new(pyn_core::MemoryLimitEvents::new()),
        )
        .with_config(cfg)
        .with_audit(audit.clone()),
    );
    let repos = Arc::new(Repositories::new(
        Arc::new(MemoryMetadataStore::new()),
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
        updates: None,
        storage: Default::default(),
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

    fn is(&self, status: StatusCode, code: &str) -> bool {
        self.status == status && self.code() == code
    }
}

async fn call(
    app: &Router,
    auth: &Cred,
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
) -> Reply {
    call_from(app, auth, method, uri, body, "").await
}

/// Like `call`, as a proxy would forward it from `address` (none when empty).
async fn call_from(
    app: &Router,
    auth: &Cred,
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
    address: &str,
) -> Reply {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    if !address.is_empty() {
        req = req.header("x-forwarded-for", address);
    }
    let req = req
        .cred(auth)
        .body(Body::from(body.map(|b| b.to_string()).unwrap_or_default()))
        .unwrap();
    let r = app.clone().oneshot(req).await.unwrap();
    Reply {
        status: r.status(),
        body: r.into_body().collect().await.unwrap().to_bytes().to_vec(),
    }
}

fn root() -> Cred {
    Cred::dev("root")
}

async fn mint(app: &Router, name: &str, scopes: &[&str]) -> String {
    let r = call(
        app,
        &root(),
        "POST",
        "/v1/admin/service-credentials",
        Some(json!({"name": name, "scopes": scopes})),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::CREATED,
        "{:?}",
        String::from_utf8_lossy(&r.body)
    );
    r.json::<api::CreatedServiceCredential>().secret
}

fn bearer(secret: &str) -> Cred {
    Cred::service(secret)
}

async fn sign_up(app: &Router, name: &str) {
    let r = call(
        app,
        &Cred::SignedOut,
        "POST",
        "/v1/register",
        Some(json!({"username": name, "password": "correct horse battery"})),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::CREATED,
        "{:?}",
        String::from_utf8_lossy(&r.body)
    );
}

#[tokio::test]
async fn the_secret_is_shown_once_and_the_listing_has_no_secret() {
    let app = app();
    let r = call(
        &app,
        &root(),
        "POST",
        "/v1/admin/service-credentials",
        Some(json!({"name": "provisioner", "scopes": ["manage_accounts"]})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED);
    let created = r.json::<api::CreatedServiceCredential>();
    assert!(created.secret.starts_with("pyns_"));
    assert_eq!(created.info.name, "provisioner");
    assert_eq!(created.info.scopes, ["manage_accounts"]);
    assert_eq!(created.info.created_by, "root");

    let list = call(&app, &root(), "GET", "/v1/admin/service-credentials", None).await;
    assert_eq!(list.status, StatusCode::OK);
    assert!(!String::from_utf8_lossy(&list.body).contains(&created.secret));
    let infos = list.json::<Vec<api::ServiceCredentialInfo>>();
    assert_eq!(infos.len(), 1);
    assert_eq!(infos[0].last_used_at, None);

    let svc = bearer(&created.secret);
    let r = call(&app, &svc, "GET", "/v1/admin/users", None).await;
    assert_eq!(r.status, StatusCode::OK);
    let list = call(&app, &root(), "GET", "/v1/admin/service-credentials", None).await;
    assert!(
        list.json::<Vec<api::ServiceCredentialInfo>>()[0]
            .last_used_at
            .is_some()
    );
}

#[tokio::test]
async fn creation_rejects_bad_input_and_duplicates() {
    let app = app();
    for body in [
        json!({"name": "ok", "scopes": []}),
        json!({"name": "ok", "scopes": ["read_usage"]}),
        json!({"name": "Bad Name", "scopes": ["manage_accounts"]}),
    ] {
        let r = call(
            &app,
            &root(),
            "POST",
            "/v1/admin/service-credentials",
            Some(body),
        )
        .await;
        assert!(r.is(StatusCode::BAD_REQUEST, "invalid_request"));
    }
    mint(&app, "dup", &["manage_accounts"]).await;
    let r = call(
        &app,
        &root(),
        "POST",
        "/v1/admin/service-credentials",
        Some(json!({"name": "dup", "scopes": ["manage_accounts"]})),
    )
    .await;
    assert!(r.is(StatusCode::CONFLICT, "service_credential_exists"));
}

#[tokio::test]
async fn scopes_gate_each_admin_route() {
    let app = app();
    sign_up(&app, "alice").await;
    let accounts = mint(&app, "accounts", &["manage_accounts"]).await;
    let orgs = mint(&app, "orgs", &["manage_organizations"]).await;
    let a = &bearer(&accounts);
    let o = &bearer(&orgs);

    let r = call(&app, a, "GET", "/v1/admin/users", None).await;
    assert_eq!(r.status, StatusCode::OK);
    let r = call(&app, o, "GET", "/v1/admin/users", None).await;
    assert!(r.is(StatusCode::FORBIDDEN, "service_scope_required"));
    let r = call(
        &app,
        o,
        "POST",
        "/v1/admin/users/alice/disable",
        Some(json!({})),
    )
    .await;
    assert!(r.is(StatusCode::FORBIDDEN, "service_scope_required"));
    let r = call(
        &app,
        a,
        "POST",
        "/v1/admin/users/alice/disable",
        Some(json!({"reason": "x"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    let r = call(&app, a, "POST", "/v1/admin/users/alice/enable", None).await;
    assert_eq!(r.status, StatusCode::OK);
    let r = call(&app, a, "POST", "/v1/admin/users/alice/approve", None).await;
    assert!(r.is(StatusCode::BAD_REQUEST, "invalid_request"));

    let new_org = json!({"name": "acme", "owner": "alice"});
    let r = call(&app, a, "POST", "/v1/admin/orgs", Some(new_org.clone())).await;
    assert!(r.is(StatusCode::FORBIDDEN, "service_scope_required"));
    let r = call(&app, o, "POST", "/v1/admin/orgs", Some(new_org.clone())).await;
    assert_eq!(r.status, StatusCode::CREATED);
    assert_eq!(r.json::<api::OrgInfo>().name, "acme");
    let r = call(&app, o, "POST", "/v1/admin/orgs", Some(new_org)).await;
    assert!(r.is(StatusCode::CONFLICT, "user_exists"));
    let r = call(
        &app,
        o,
        "POST",
        "/v1/admin/orgs",
        Some(json!({"name": "x1", "owner": "ghost"})),
    )
    .await;
    assert!(r.is(StatusCode::NOT_FOUND, "user_not_found"));
    let r = call(&app, a, "DELETE", "/v1/admin/orgs/acme", None).await;
    assert!(r.is(StatusCode::FORBIDDEN, "service_scope_required"));
    let r = call(&app, o, "DELETE", "/v1/admin/orgs/acme", None).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let r = call(&app, o, "DELETE", "/v1/admin/orgs/acme", None).await;
    assert!(r.is(StatusCode::NOT_FOUND, "org_not_found"));
}

#[tokio::test]
async fn a_credential_cannot_manage_credentials_or_read_the_server_log() {
    let app = app();
    let secret = mint(&app, "all", &["manage_accounts", "manage_organizations"]).await;
    let svc = bearer(&secret);
    let c = &svc;
    let r = call(
        &app,
        c,
        "POST",
        "/v1/admin/service-credentials",
        Some(json!({"name": "other", "scopes": ["manage_accounts"]})),
    )
    .await;
    assert!(r.is(StatusCode::FORBIDDEN, "server_admin_required"));
    for (m, u) in [
        ("GET", "/v1/admin/service-credentials"),
        ("DELETE", "/v1/admin/service-credentials/all"),
        ("GET", "/v1/admin/audit"),
        ("PUT", "/v1/admin/users/alice/admin"),
        ("DELETE", "/v1/admin/users/alice/admin"),
    ] {
        let r = call(&app, c, m, u, None).await;
        assert!(
            r.is(StatusCode::FORBIDDEN, "server_admin_required"),
            "{m} {u}"
        );
    }
}

#[tokio::test]
async fn a_credential_is_refused_outside_the_admin_routes() {
    let app = app();
    let secret = mint(&app, "all", &["manage_accounts", "manage_organizations"]).await;
    let svc = bearer(&secret);
    let c = &svc;
    for (m, u, body) in [
        ("GET", "/v1/me", None),
        ("GET", "/v1/orgs", None),
        ("POST", "/v1/orgs", Some(json!({"name": "mine"}))),
        ("GET", "/v1/repos", None),
        (
            "POST",
            "/v1/tokens",
            Some(cred::token_body("t", ["a/b"], ["read"])),
        ),
    ] {
        let r = call(&app, c, m, u, body).await;
        assert!(
            r.is(StatusCode::FORBIDDEN, "service_credential_not_allowed"),
            "{m} {u}: {:?}",
            String::from_utf8_lossy(&r.body)
        );
    }
}

#[tokio::test]
async fn a_credential_cannot_sign_in() {
    let app = app();
    mint(&app, "provisioner", &["manage_accounts"]).await;
    for name in ["provisioner", "@service:provisioner"] {
        let body = json!({"username": name, "password": "correct horse battery"});
        for uri in ["/v1/login", "/v1/session"] {
            let r = call(&app, &Cred::SignedOut, "POST", uri, Some(body.clone())).await;
            assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{name} {uri}");
        }
    }
}

#[tokio::test]
async fn revoking_stops_the_credential_at_once_and_keeps_it_listed() {
    let app = app();
    let secret = mint(&app, "provisioner", &["manage_accounts"]).await;
    let svc = bearer(&secret);
    let c = &svc;
    assert_eq!(
        call(&app, c, "GET", "/v1/admin/users", None).await.status,
        StatusCode::OK
    );
    let r = call(
        &app,
        &root(),
        "DELETE",
        "/v1/admin/service-credentials/provisioner",
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let r = call(&app, c, "GET", "/v1/admin/users", None).await;
    assert!(r.is(StatusCode::UNAUTHORIZED, "unauthenticated"));
    let r = call(
        &app,
        &root(),
        "DELETE",
        "/v1/admin/service-credentials/nobody",
        None,
    )
    .await;
    assert!(r.is(StatusCode::NOT_FOUND, "service_credential_not_found"));
    let list = call(&app, &root(), "GET", "/v1/admin/service-credentials", None).await;
    assert!(
        list.json::<Vec<api::ServiceCredentialInfo>>()[0]
            .revoked_at
            .is_some()
    );
}

#[tokio::test]
async fn the_server_log_names_the_credential_as_actor() {
    let app = app();
    sign_up(&app, "alice").await;
    let secret = mint(
        &app,
        "provisioner",
        &["manage_accounts", "manage_organizations"],
    )
    .await;
    let svc = bearer(&secret);
    let c = &svc;
    call(
        &app,
        c,
        "POST",
        "/v1/admin/users/alice/disable",
        Some(json!({})),
    )
    .await;
    call(&app, c, "POST", "/v1/admin/users/alice/enable", None).await;
    call(
        &app,
        c,
        "POST",
        "/v1/admin/orgs",
        Some(json!({"name": "acme", "owner": "alice"})),
    )
    .await;

    let page = call(&app, &root(), "GET", "/v1/admin/audit", None)
        .await
        .json::<api::AuditPage>();
    let seen: Vec<_> = page
        .entries
        .iter()
        .rev()
        .map(|e| (e.actor.as_str(), e.action.as_str()))
        .collect();
    assert_eq!(
        seen,
        [
            ("root", "service_credential_created"),
            ("@service:provisioner", "account_disabled"),
            ("@service:provisioner", "account_enabled"),
            ("@service:provisioner", "org_created"),
        ]
    );
}

async fn mint_with(app: &Router, name: &str, extra: serde_json::Value) -> (String, Reply) {
    let mut body = json!({"name": name, "scopes": ["manage_accounts"]});
    body.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    let r = call(
        app,
        &root(),
        "POST",
        "/v1/admin/service-credentials",
        Some(body),
    )
    .await;
    let secret = if r.status == StatusCode::CREATED {
        r.json::<api::CreatedServiceCredential>().secret
    } else {
        String::new()
    };
    (secret, r)
}

async fn refusal_log(app: &Router) -> Vec<String> {
    call(app, &root(), "GET", "/v1/admin/audit", None)
        .await
        .json::<api::AuditPage>()
        .entries
        .into_iter()
        .rev()
        .filter(|e| e.action == "service_credential_refused")
        .map(|e| format!("{}: {}", e.actor, e.detail))
        .collect()
}

#[tokio::test]
async fn creation_accepts_and_lists_the_optional_restrictions() {
    let app = app();
    let (_, r) = mint_with(
        &app,
        "billing",
        json!({"expires_at": "2099-01-01T00:00:00Z", "allowed_sources": ["10.0.0.0/8"], "rate_limit_per_minute": 30}),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED);
    let info = r.json::<api::CreatedServiceCredential>().info;
    assert_eq!(info.allowed_sources, ["10.0.0.0/8"]);
    assert_eq!(info.rate_limit_per_minute, Some(30));
    assert!(info.expires_at.is_some());

    // A request with none of the new fields still works and lists them empty.
    mint(&app, "plain", &["manage_accounts"]).await;
    let list = call(&app, &root(), "GET", "/v1/admin/service-credentials", None).await;
    let infos = list.json::<Vec<api::ServiceCredentialInfo>>();
    let plain = infos.iter().find(|i| i.name == "plain").unwrap();
    assert_eq!(
        (
            plain.expires_at,
            plain.allowed_sources.len(),
            plain.rate_limit_per_minute
        ),
        (None, 0, None)
    );

    for bad in [
        json!({"expires_at": "2001-01-01T00:00:00Z"}),
        json!({"allowed_sources": ["nope"]}),
        json!({"rate_limit_per_minute": 0}),
    ] {
        let (_, r) = mint_with(&app, "bad", bad).await;
        assert!(r.is(StatusCode::BAD_REQUEST, "invalid_request"));
    }
}

#[tokio::test]
async fn the_source_comes_from_the_trusted_forwarded_address_and_is_recorded() {
    let app = app_behind_proxy();
    let (secret, _) = mint_with(
        &app,
        "billing",
        json!({"allowed_sources": ["192.0.2.0/24"]}),
    )
    .await;
    let svc = bearer(&secret);

    let ok = call_from(
        &app,
        &svc,
        "GET",
        "/v1/admin/users",
        None,
        "198.51.100.1, 192.0.2.44",
    )
    .await;
    assert_eq!(ok.status, StatusCode::OK);
    let infos = call(&app, &root(), "GET", "/v1/admin/service-credentials", None)
        .await
        .json::<Vec<api::ServiceCredentialInfo>>();
    assert_eq!(infos[0].last_used_from.as_deref(), Some("192.0.2.44"));

    // Only the last entry counts: a client cannot put an allowed address in front of its own.
    let spoof = call_from(
        &app,
        &svc,
        "GET",
        "/v1/admin/users",
        None,
        "192.0.2.44, 198.51.100.1",
    )
    .await;
    assert_eq!(spoof.status, StatusCode::UNAUTHORIZED);
    assert_eq!(spoof.code(), "unauthenticated");
    let none = call(&app, &svc, "GET", "/v1/admin/users", None).await;
    assert_eq!(
        none.status,
        StatusCode::UNAUTHORIZED,
        "no known address fails closed"
    );

    let log = refusal_log(&app).await;
    assert_eq!(log.len(), 2);
    assert!(log[0].starts_with("@service:billing:") && log[0].contains("198.51.100.1"));
    assert!(log.iter().all(|l| !l.contains(&secret)));
}

#[tokio::test]
async fn without_a_trusted_proxy_the_forwarded_header_is_ignored() {
    let app = app();
    let (secret, _) = mint_with(
        &app,
        "billing",
        json!({"allowed_sources": ["192.0.2.0/24"]}),
    )
    .await;
    let r = call_from(
        &app,
        &bearer(&secret),
        "GET",
        "/v1/admin/users",
        None,
        "192.0.2.44",
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn an_expired_credential_gets_401_and_stays_listed() {
    let app = app();
    // Expires 2 seconds from now: far enough to create, short enough to wait out.
    let soon = chrono::Utc::now() + chrono::Duration::seconds(2);
    let (secret, r) = mint_with(&app, "short", json!({"expires_at": soon})).await;
    assert_eq!(r.status, StatusCode::CREATED);
    let svc = bearer(&secret);
    assert_eq!(
        call(&app, &svc, "GET", "/v1/admin/users", None)
            .await
            .status,
        StatusCode::OK
    );

    tokio::time::sleep(std::time::Duration::from_millis(2200)).await;
    let r = call(&app, &svc, "GET", "/v1/admin/users", None).await;
    assert!(r.is(StatusCode::UNAUTHORIZED, "unauthenticated"));
    let infos = call(&app, &root(), "GET", "/v1/admin/service-credentials", None)
        .await
        .json::<Vec<api::ServiceCredentialInfo>>();
    assert_eq!(infos.len(), 1);
    assert!(infos[0].expires_at.is_some() && infos[0].revoked_at.is_none());
    let log = refusal_log(&app).await;
    assert!(log.len() == 1 && log[0].contains("expired") && !log[0].contains(&secret));
}

#[tokio::test]
async fn the_per_credential_rate_limit_answers_429_with_retry_after() {
    let app = app();
    let (secret, _) = mint_with(&app, "busy", json!({"rate_limit_per_minute": 2})).await;
    let svc = bearer(&secret);
    for _ in 0..2 {
        assert_eq!(
            call(&app, &svc, "GET", "/v1/admin/users", None)
                .await
                .status,
            StatusCode::OK
        );
    }
    let r = call(&app, &svc, "GET", "/v1/admin/users", None).await;
    assert!(r.is(StatusCode::TOO_MANY_REQUESTS, "too_many_attempts"));
    let other = mint(&app, "calm", &["manage_accounts"]).await;
    let ok = call(&app, &bearer(&other), "GET", "/v1/admin/users", None).await;
    assert_eq!(
        ok.status,
        StatusCode::OK,
        "another credential is not affected"
    );
}

#[tokio::test]
async fn a_refused_scope_and_a_refused_route_are_audited_with_the_credential_name() {
    let app = app();
    let secret = mint(&app, "billing", &["manage_accounts"]).await;
    let svc = bearer(&secret);
    let r = call(&app, &svc, "GET", "/v1/admin/webhooks", None).await;
    assert!(r.is(StatusCode::FORBIDDEN, "service_scope_required"));
    let r = call(&app, &svc, "GET", "/v1/admin/service-credentials", None).await;
    assert!(r.is(StatusCode::FORBIDDEN, "server_admin_required"));
    let r = call(&app, &svc, "GET", "/v1/me", None).await;
    assert!(r.is(StatusCode::FORBIDDEN, "service_credential_not_allowed"));

    let log = refusal_log(&app).await;
    assert_eq!(log.len(), 3, "{log:?}");
    assert!(
        log.iter()
            .all(|l| l.starts_with("@service:billing:") && !l.contains(&secret))
    );
    assert!(log[0].contains("manage_webhooks"));
}
