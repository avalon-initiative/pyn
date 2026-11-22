//! Development auth: the header is only read when the flag is on, never outranks a bearer token, never changes what
//! a service credential may do, and the flag shows on `/healthz`; the startup bind check.

#[path = "common/cred.rs"]
mod cred;
use cred::{Cred, send};
use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore,
};
use pyn_core::{
    AccessConfig, AccessService, AuthProvider, RegistrationMode, Repositories, Rules, SystemClock,
};
use pyn_proto as api;
use pyn_server::auth::{BearerAuth, DevHeaderAuth};
use pyn_server::{AppState, check_dev_auth_bind, router};
use serde_json::json;

fn state() -> AppState {
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
    AppState {
        repos,
        objects,
        access: access.clone(),
        auth: Arc::new(BearerAuth { access }),
        dev_auth: Some(Arc::new(DevHeaderAuth) as Arc<dyn AuthProvider>),
        trust_forwarded: false,
        updates: None,
        storage: Default::default(),
    }
}

fn without_dev(state: &AppState) -> AppState {
    AppState {
        dev_auth: None,
        ..state.clone()
    }
}

/// Signs `alice` up and mints a user token and a service credential, with development auth on.
async fn secrets(app: &Router) -> (String, String) {
    let body = json!({"username": "alice", "password": "correct horse battery"});
    let signed_out = Cred::SignedOut;
    let r = send(
        app,
        &signed_out,
        "POST",
        "/v1/register",
        Some(body.clone()),
        &[],
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED);
    let r = send(app, &signed_out, "POST", "/v1/login", Some(body), &[]).await;
    let token = serde_json::from_slice::<api::CreatedToken>(&r.body)
        .unwrap()
        .token;
    let body = json!({"name": "billing", "scopes": ["manage_accounts"]});
    let r = send(
        app,
        &Cred::dev("root"),
        "POST",
        "/v1/admin/service-credentials",
        Some(body),
        &[],
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED);
    let service = serde_json::from_slice::<api::CreatedServiceCredential>(&r.body)
        .unwrap()
        .secret;
    (token, service)
}

fn error_code(body: &[u8]) -> String {
    serde_json::from_slice::<api::ErrorBody>(body).unwrap().code
}

#[tokio::test]
async fn the_header_is_ignored_when_the_flag_is_off() {
    let on = state();
    let off = without_dev(&on);
    let r = send(&router(on), &Cred::dev("alice"), "GET", "/v1/me", None, &[]).await;
    assert_eq!(r.status, StatusCode::OK);
    let r = send(
        &router(off),
        &Cred::dev("alice"),
        "GET",
        "/v1/me",
        None,
        &[],
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(error_code(&r.body), "unauthenticated");
}

#[tokio::test]
async fn a_bearer_token_wins_over_the_header() {
    let on = state();
    let (token, _) = secrets(&router(on.clone())).await;
    for state in [on.clone(), without_dev(&on)] {
        let r = send(
            &router(state),
            &Cred::token(&token),
            "GET",
            "/v1/me",
            None,
            &[(api::DEV_USER_HEADER, "mallory")],
        )
        .await;
        assert_eq!(r.status, StatusCode::OK);
        assert_eq!(
            serde_json::from_slice::<api::Account>(&r.body)
                .unwrap()
                .user,
            "alice"
        );
    }
}

#[tokio::test]
async fn the_flag_does_not_apply_to_service_credentials() {
    let on = state();
    let (_, service) = secrets(&router(on.clone())).await;
    // Still refused on a user route, with the flag on and with a header naming an administrator.
    for (state, extra) in [
        (on.clone(), vec![]),
        (on.clone(), vec![(api::DEV_USER_HEADER, "root")]),
        (without_dev(&on), vec![(api::DEV_USER_HEADER, "root")]),
    ] {
        let r = send(
            &router(state),
            &Cred::service(&service),
            "GET",
            "/v1/me",
            None,
            &extra,
        )
        .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN);
        assert_eq!(error_code(&r.body), "service_credential_not_allowed");
    }
}

#[tokio::test]
async fn healthz_states_whether_development_auth_is_on() {
    let on = state();
    let off = without_dev(&on);
    for (state, expect) in [(on, "on"), (off, "off")] {
        let r = send(
            &router(state),
            &Cred::SignedOut,
            "GET",
            "/healthz",
            None,
            &[],
        )
        .await;
        assert_eq!(r.status, StatusCode::OK);
        assert_eq!(r.headers[pyn_server::DEV_AUTH_HEADER], expect);
        assert_eq!(r.body, b"ok");
    }
}

#[test]
fn development_auth_is_refused_beyond_loopback_unless_acknowledged() {
    for addr in ["127.0.0.1:7878", "[::1]:7878", "localhost:7878"] {
        assert!(check_dev_auth_bind(addr, true, false).is_ok(), "{addr}");
    }
    for addr in [
        "0.0.0.0:7878",
        "[::]:7878",
        "192.168.1.5:7878",
        "example.com:7878",
    ] {
        let err = check_dev_auth_bind(addr, true, false).unwrap_err();
        assert!(err.contains("PYN_DEV_AUTH_ALLOW_NON_LOOPBACK"), "{addr}");
        assert!(check_dev_auth_bind(addr, true, true).is_ok(), "{addr}");
        assert!(check_dev_auth_bind(addr, false, false).is_ok(), "{addr}");
    }
}
