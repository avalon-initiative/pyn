//! The HTTP contract of service credentials: scopes, revocation, audit, and what they cannot reach.

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
    let clock = Arc::new(SystemClock);
    let objects = Arc::new(MemoryObjectStore::new());
    let audit = Arc::new(MemoryAuditStore::new());
    let cfg = AccessConfig {
        registration: RegistrationMode::Open,
        require_email_verification: false,
        ..AccessConfig::default()
    };
    let access = Arc::new(
        AccessService::new(Arc::new(MemoryAccessStore::new()), clock.clone())
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

    fn code(&self) -> String {
        self.json::<api::ErrorBody>().code
    }

    fn is(&self, status: StatusCode, code: &str) -> bool {
        self.status == status && self.code() == code
    }
}

async fn call(
    app: &Router,
    auth: (&str, &str),
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
) -> Reply {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header(auth.0, auth.1)
        .body(Body::from(body.map(|b| b.to_string()).unwrap_or_default()))
        .unwrap();
    let r = app.clone().oneshot(req).await.unwrap();
    Reply {
        status: r.status(),
        body: r.into_body().collect().await.unwrap().to_bytes().to_vec(),
    }
}

const ROOT: (&str, &str) = (api::DEV_USER_HEADER, "root");

async fn mint(app: &Router, name: &str, scopes: &[&str]) -> String {
    let r = call(
        app,
        ROOT,
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

fn bearer(secret: &str) -> (&'static str, String) {
    ("authorization", format!("Bearer {secret}"))
}

async fn sign_up(app: &Router, name: &str) {
    let r = call(
        app,
        ("x-none", "1"),
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
        ROOT,
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

    let list = call(&app, ROOT, "GET", "/v1/admin/service-credentials", None).await;
    assert_eq!(list.status, StatusCode::OK);
    assert!(!String::from_utf8_lossy(&list.body).contains(&created.secret));
    let infos = list.json::<Vec<api::ServiceCredentialInfo>>();
    assert_eq!(infos.len(), 1);
    assert_eq!(infos[0].last_used_at, None);

    let (h, v) = bearer(&created.secret);
    let r = call(&app, (h, &v), "GET", "/v1/admin/users", None).await;
    assert_eq!(r.status, StatusCode::OK);
    let list = call(&app, ROOT, "GET", "/v1/admin/service-credentials", None).await;
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
            ROOT,
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
        ROOT,
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
    let (ah, av) = bearer(&accounts);
    let (oh, ov) = bearer(&orgs);
    let (a, o) = ((ah, av.as_str()), (oh, ov.as_str()));

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
    let (h, v) = bearer(&secret);
    let c = (h, v.as_str());
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
    let (h, v) = bearer(&secret);
    let c = (h, v.as_str());
    for (m, u, body) in [
        ("GET", "/v1/me", None),
        ("GET", "/v1/orgs", None),
        ("POST", "/v1/orgs", Some(json!({"name": "mine"}))),
        ("GET", "/v1/repos", None),
        (
            "POST",
            "/v1/tokens",
            Some(json!({"name": "t", "permissions": ["read"], "repos": ["a/b"]})),
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
            let r = call(&app, ("x-none", "1"), "POST", uri, Some(body.clone())).await;
            assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{name} {uri}");
        }
    }
}

#[tokio::test]
async fn revoking_stops_the_credential_at_once_and_keeps_it_listed() {
    let app = app();
    let secret = mint(&app, "provisioner", &["manage_accounts"]).await;
    let (h, v) = bearer(&secret);
    let c = (h, v.as_str());
    assert_eq!(
        call(&app, c, "GET", "/v1/admin/users", None).await.status,
        StatusCode::OK
    );
    let r = call(
        &app,
        ROOT,
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
        ROOT,
        "DELETE",
        "/v1/admin/service-credentials/nobody",
        None,
    )
    .await;
    assert!(r.is(StatusCode::NOT_FOUND, "service_credential_not_found"));
    let list = call(&app, ROOT, "GET", "/v1/admin/service-credentials", None).await;
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
    let (h, v) = bearer(&secret);
    let c = (h, v.as_str());
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

    let page = call(&app, ROOT, "GET", "/v1/admin/audit", None)
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
