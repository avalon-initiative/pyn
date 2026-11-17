//! Each credential kind, sent through the shared helper, is resolved by the server as itself: the identity and the
//! credential class come back from the API, not only a status code.

use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
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

#[path = "common/cred.rs"]
mod cred;
use cred::{Cred, Sent, send};

const PASSWORD: &str = "correct horse battery";
const GRANTED: [&str; 3] = ["checkin", "lock", "read"];
/// Contents write carries both check-in and restore.
const EFFECTIVE: [&str; 4] = ["checkin", "lock", "read", "restore"];

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

fn json<T: serde::de::DeserializeOwned>(r: &Sent) -> T {
    serde_json::from_slice(&r.body)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&r.body)))
}

fn text(r: &Sent) -> String {
    String::from_utf8_lossy(&r.body).into_owned()
}

struct World {
    app: Router,
    session: Cred,
    full: Cred,
    scoped: Cred,
    service: Cred,
}

async fn world() -> World {
    let app = server().await;
    let signed_out = Cred::SignedOut;
    let body = json!({"username": "alice", "password": PASSWORD});
    let r = send(
        &app,
        &signed_out,
        "POST",
        "/v1/register",
        Some(body.clone()),
        &[],
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", text(&r));
    let r = send(
        &app,
        &signed_out,
        "POST",
        "/v1/login",
        Some(body.clone()),
        &[],
    )
    .await;
    let full = Cred::token(&json::<api::CreatedToken>(&r).token);
    let r = send(&app, &signed_out, "POST", "/v1/session", Some(body), &[]).await;
    assert_eq!(r.status, StatusCode::OK, "{}", text(&r));
    let set = r.headers["set-cookie"].to_str().unwrap();
    let cookie = set
        .split(';')
        .next()
        .and_then(|c| c.strip_prefix("pyn_session="))
        .unwrap();
    let session = Cred::session(cookie, &json::<api::SessionInfo>(&r).csrf_token);

    let r = send(
        &app,
        &full,
        "POST",
        "/v1/repos",
        Some(json!({"owner": "alice", "name": "game"})),
        &[],
    )
    .await;
    assert!(r.status.is_success(), "{}", text(&r));
    let body = cred::token_body("ci", ["alice/game"], GRANTED);
    let r = send(&app, &full, "POST", "/v1/tokens", Some(body), &[]).await;
    let scoped = Cred::token(&json::<api::CreatedToken>(&r).token);
    let body = json!({"name": "billing", "scopes": ["manage_accounts"]});
    let r = send(
        &app,
        &Cred::dev("root"),
        "POST",
        "/v1/admin/service-credentials",
        Some(body),
        &[],
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", text(&r));
    let service = Cred::service(&json::<api::CreatedServiceCredential>(&r).secret);
    World {
        app,
        session,
        full,
        scoped,
        service,
    }
}

async fn account(w: &World, cred: &Cred) -> Sent {
    send(&w.app, cred, "GET", "/v1/me", None, &[]).await
}

#[tokio::test]
async fn a_session_and_a_full_token_resolve_as_alice_with_the_full_view() {
    let w = world().await;
    for (label, cred) in [("session", &w.session), ("token", &w.full)] {
        let r = account(&w, cred).await;
        assert_eq!(r.status, StatusCode::OK, "{label}: {}", text(&r));
        let me: api::Account = json(&r);
        assert_eq!(me.user, "alice", "{label}");
        assert!(!me.admin, "{label}");
        assert_eq!(me.has_password, Some(true), "{label}: full scope");
    }
}

#[tokio::test]
async fn a_repository_scoped_token_resolves_as_alice_with_the_reduced_view() {
    let w = world().await;
    let r = account(&w, &w.scoped).await;
    assert_eq!(r.status, StatusCode::OK, "{}", text(&r));
    let me: api::Account = json(&r);
    assert_eq!(me.user, "alice");
    assert_eq!(me.has_password, None, "not full scope");

    let uri = "/v1/repos/alice/game/me";
    let r = send(&w.app, &w.scoped, "GET", uri, None, &[]).await;
    assert_eq!(r.status, StatusCode::OK, "{}", text(&r));
    let me: api::Me = json(&r);
    assert_eq!(me.user, "alice");
    let mut got = me.permissions;
    got.sort();
    assert_eq!(
        got, EFFECTIVE,
        "the token's own permissions, not all of them"
    );
}

#[tokio::test]
async fn the_dev_header_resolves_as_the_named_unrestricted_user() {
    let w = world().await;
    let r = account(&w, &Cred::dev("root")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", text(&r));
    let me: api::Account = json(&r);
    assert_eq!((me.user.as_str(), me.admin), ("root", true));
    assert_eq!(me.has_password, Some(false), "a dev user has no password");

    let r = send(
        &w.app,
        &Cred::dev("alice"),
        "GET",
        "/v1/repos/alice/game/me",
        None,
        &[],
    )
    .await;
    let me: api::Me = json(&r);
    assert_eq!(me.user, "alice");
    assert!(me.permissions.len() > GRANTED.len(), "every permission");
}

#[tokio::test]
async fn a_service_credential_is_not_a_user_and_not_a_dev_user() {
    let w = world().await;
    let secret = match &w.service {
        Cred::Service(s) => s.clone(),
        other => panic!("{other:?}"),
    };
    let r = account(&w, &w.service).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", text(&r));
    assert_eq!(
        json::<api::ErrorBody>(&r).code,
        "service_credential_not_allowed"
    );
    assert!(!text(&r).contains(&secret), "the secret is never echoed");

    // The same secret in the dev header is what an unrestricted dev user looks like.
    let wrong = send(
        &w.app,
        &Cred::SignedOut,
        "GET",
        "/v1/me",
        None,
        &[(api::DEV_USER_HEADER, &secret)],
    )
    .await;
    assert_eq!(wrong.status, StatusCode::OK, "{}", text(&wrong));
    assert_eq!(json::<api::Account>(&wrong).user, secret);
}

#[tokio::test]
async fn signed_out_resolves_as_nobody() {
    let w = world().await;
    let r = account(&w, &Cred::SignedOut).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", text(&r));
    assert_eq!(json::<api::ErrorBody>(&r).code, "unauthenticated");
}

#[test]
fn who_classifies_by_secret_prefix() {
    assert!(matches!(Cred::who(""), Cred::SignedOut));
    assert!(matches!(Cred::who("alice"), Cred::Dev(_)));
    assert!(matches!(Cred::who("pyn_abc"), Cred::Token(_)));
    assert!(matches!(Cred::who("pyns_abc"), Cred::Service(_)));
    assert!(matches!(Cred::bearer("pyns_abc"), Cred::Service(_)));
}

fn apply(cred: Cred) {
    cred.apply(axum::http::Request::builder());
}

#[test]
#[should_panic(expected = "dev header must carry a user name")]
fn a_service_secret_as_a_dev_user_panics() {
    apply(Cred::Dev("pyns_abc_def".into()));
}

#[test]
#[should_panic(expected = "dev header must carry a user name")]
fn a_token_secret_as_a_dev_user_panics() {
    apply(Cred::Dev("pyn_abc_def".into()));
}

#[test]
#[should_panic(expected = "a user token starts with")]
fn a_service_secret_as_a_token_panics() {
    apply(Cred::Token("pyns_abc_def".into()));
}

#[test]
#[should_panic(expected = "a service credential starts with")]
fn an_unknown_prefix_as_a_service_credential_panics() {
    apply(Cred::Service("pyn_abc_def".into()));
}

#[test]
#[should_panic(expected = "a user token starts with")]
fn an_unknown_bearer_prefix_panics() {
    apply(Cred::bearer("whsec_abc"));
}

#[test]
#[should_panic(expected = "a session cookie is an opaque value")]
fn a_secret_as_a_session_cookie_panics() {
    apply(Cred::session_read("pyn_abc_def"));
}
