//! The server access mode: a private server shows nothing signed out, a public one acts as it always did, and an
//! administrator switches between them on a running server.

#[path = "common/cred.rs"]
mod cred;
use cred::{Cred, WithCred};
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore,
};
use pyn_core::{
    AccessConfig, AccessMode, AccessService, AuthProvider, Identity, Permission, RegistrationMode,
    Repositories, Rules, SystemClock, UserId, Visibility,
};
use pyn_proto as api;
use pyn_server::auth::{BearerAuth, DevHeaderAuth};
use pyn_server::{AppState, router};
use serde_json::{Value, json};
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";

struct World {
    app: Router,
    state: AppState,
    alice_token: String,
}

async fn world(mode: AccessMode) -> World {
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
            ..AccessConfig::default()
        })
        .with_registry(meta.clone())
        .with_audit(audit.clone()),
    );
    let repos = Arc::new(Repositories::new(
        meta,
        objects.clone(),
        audit,
        access.clone(),
        clock,
        Rules::empty(),
    ));
    let state = AppState {
        repos,
        objects,
        access: access.clone(),
        auth: Arc::new(BearerAuth {
            access: access.clone(),
        }),
        dev_auth: Some(Arc::new(DevHeaderAuth) as Arc<dyn AuthProvider>),
        trust_forwarded: false,
        updates: None,
    };
    access.admin_for_tests(&UserId::new("root")).await.unwrap();
    let app = router(state.clone());
    let body = json!({"username": "alice", "password": PASSWORD}).to_string();
    let r = call(
        &app,
        &Cred::SignedOut,
        Method::POST,
        "/v1/register",
        Some(body.clone()),
    )
    .await;
    assert_eq!(r.0, StatusCode::CREATED);
    let r = call(
        &app,
        &Cred::SignedOut,
        Method::POST,
        "/v1/login",
        Some(body),
    )
    .await;
    let alice_token = serde_json::from_slice::<api::CreatedToken>(&r.1)
        .unwrap()
        .token;
    let alice = UserId::new("alice");
    let as_alice = Identity {
        user: alice.clone(),
        credential: pyn_core::Credential::Session,
    };
    for (name, visibility) in [("open", Visibility::Public), ("game", Visibility::Private)] {
        state
            .repos
            .create(&as_alice, &alice, name, Some(visibility), None)
            .await
            .unwrap();
    }
    World {
        app,
        state,
        alice_token,
    }
}

async fn call(
    app: &Router,
    cred: &Cred,
    method: Method,
    uri: &str,
    body: Option<String>,
) -> (StatusCode, Vec<u8>) {
    let r = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .cred(cred)
                .body(Body::from(body.unwrap_or_default()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = r.status();
    (
        status,
        r.into_body().collect().await.unwrap().to_bytes().to_vec(),
    )
}

fn code(body: &[u8]) -> Option<String> {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| v["code"].as_str().map(str::to_string))
}

async fn mode_of(w: &World) -> String {
    let (status, body) = call(&w.app, &Cred::SignedOut, Method::GET, "/v1/setup", None).await;
    assert_eq!(status, StatusCode::OK);
    serde_json::from_slice::<api::SetupStatus>(&body)
        .unwrap()
        .access_mode
}

async fn set_mode(w: &World, cred: &Cred, mode: &str) -> (StatusCode, Vec<u8>) {
    call(
        &w.app,
        cred,
        Method::PUT,
        "/v1/admin/access",
        Some(json!({"access_mode": mode}).to_string()),
    )
    .await
}

#[tokio::test]
async fn a_private_server_refuses_what_a_signed_out_visitor_could_read() {
    let w = world(AccessMode::Private).await;
    assert_eq!(mode_of(&w).await, "private");
    for uri in [
        "/v1/users",
        "/v1/users/alice",
        "/v1/users/alice/avatar",
        "/v1/orgs/acme/avatar",
        "/v1/explore/repos",
    ] {
        let (status, body) = call(&w.app, &Cred::SignedOut, Method::GET, uri, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri}");
        assert_eq!(code(&body).as_deref(), Some("unauthenticated"), "{uri}");
    }
    for uri in [
        "/v1/repos/alice/open",
        "/v1/repos/alice/open/tree",
        "/v1/repos/alice/ghost/tree",
    ] {
        let (status, body) = call(&w.app, &Cred::SignedOut, Method::GET, uri, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
        assert_eq!(code(&body).as_deref(), Some("repo_not_found"), "{uri}");
    }
    let alice = Cred::token(&w.alice_token);
    for uri in [
        "/v1/users",
        "/v1/users/alice",
        "/v1/repos/alice/open/tree",
        "/v1/explore/repos",
    ] {
        let (status, _) = call(&w.app, &alice, Method::GET, uri, None).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
    }
    let (status, _) = call(
        &w.app,
        &Cred::token("pyn_wrongsecret"),
        Method::GET,
        "/v1/users/alice",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_private_server_keeps_the_sign_in_and_setup_flows_open() {
    let w = world(AccessMode::Private).await;
    let body = json!({"username": "bob", "password": PASSWORD}).to_string();
    for (method, uri, payload, expected) in [
        (Method::GET, "/healthz", None, StatusCode::OK),
        (Method::GET, "/openapi.json", None, StatusCode::OK),
        (Method::GET, "/v1/capabilities", None, StatusCode::OK),
        (Method::GET, "/v1/setup", None, StatusCode::OK),
        (Method::GET, "/v1/registration", None, StatusCode::OK),
        (Method::GET, "/v1/sign-in-options", None, StatusCode::OK),
        (
            Method::POST,
            "/v1/register",
            Some(body.clone()),
            StatusCode::CREATED,
        ),
        (
            Method::POST,
            "/v1/login",
            Some(body.clone()),
            StatusCode::OK,
        ),
        (
            Method::POST,
            "/v1/setup",
            Some("{}".to_string()),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
    ] {
        let (status, reply) = call(&w.app, &Cred::SignedOut, method.clone(), uri, payload).await;
        if status != expected && uri == "/v1/setup" {
            assert_eq!(status, StatusCode::CONFLICT, "{method} {uri}");
            continue;
        }
        assert_eq!(
            status,
            expected,
            "{method} {uri}: {}",
            String::from_utf8_lossy(&reply)
        );
    }
    let (status, _) = call(
        &w.app,
        &Cred::SignedOut,
        Method::GET,
        "/v1/oidc/authorize",
        None,
    )
    .await;
    assert_ne!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = call(
        &w.app,
        &Cred::SignedOut,
        Method::DELETE,
        "/v1/session",
        None,
    )
    .await;
    assert_ne!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn anonymous_clone_and_lfs_download_of_a_public_repository_follow_the_mode() {
    let w = world(AccessMode::Private).await;
    let fetch = |w: &World| {
        let app = w.app.clone();
        async move {
            call(
                &app,
                &Cred::SignedOut,
                Method::POST,
                "/v1/repos/alice/open/fetch",
                Some("{}".into()),
            )
            .await
        }
    };
    let lfs = "/v1/repos/alice/open/info/lfs/objects/0000000000000000000000000000000000000000000000000000000000000000";
    let (status, body) = fetch(&w).await;
    assert_eq!(
        (status, code(&body).as_deref()),
        (StatusCode::NOT_FOUND, Some("repo_not_found"))
    );
    let (status, _) = call(&w.app, &Cred::SignedOut, Method::GET, lfs, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    set_mode(&w, &Cred::dev("root"), "public").await;
    let (status, _) = fetch(&w).await;
    assert_eq!(status, StatusCode::OK);
    // A public repository's LFS download no longer needs a credential here; the object is just not stored.
    let (status, _) = call(&w.app, &Cred::SignedOut, Method::GET, lfs, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, body) = call(
        &w.app,
        &Cred::SignedOut,
        Method::GET,
        "/v1/repos/alice/game/tree",
        None,
    )
    .await;
    assert_eq!(
        (status, code(&body).as_deref()),
        (StatusCode::NOT_FOUND, Some("repo_not_found"))
    );
}

#[tokio::test]
async fn an_administrator_switches_the_mode_on_a_running_server_and_it_applies_at_once() {
    let w = world(AccessMode::Private).await;
    let read = || {
        call(
            &w.app,
            &Cred::SignedOut,
            Method::GET,
            "/v1/users/alice",
            None,
        )
    };
    assert_eq!(read().await.0, StatusCode::UNAUTHORIZED);

    let (status, body) = set_mode(&w, &Cred::dev("root"), "public").await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert_eq!(
        serde_json::from_slice::<api::ServerAccess>(&body)
            .unwrap()
            .access_mode,
        "public"
    );
    assert_eq!(read().await.0, StatusCode::OK);
    assert_eq!(mode_of(&w).await, "public");

    assert_eq!(
        set_mode(&w, &Cred::dev("root"), "private").await.0,
        StatusCode::OK
    );
    assert_eq!(read().await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(mode_of(&w).await, "private");

    let (status, body) = set_mode(&w, &Cred::dev("root"), "sideways").await;
    assert_eq!(
        (status, code(&body).as_deref()),
        (StatusCode::BAD_REQUEST, Some("invalid_request"))
    );
    assert_eq!(mode_of(&w).await, "private");
}

#[tokio::test]
async fn only_a_full_scope_administrator_changes_the_mode() {
    let w = world(AccessMode::Private).await;
    let access = w.state.access.clone();
    let r = call(
        &w.app,
        &Cred::dev("root"),
        Method::POST,
        "/v1/admin/service-credentials",
        Some(json!({"name": "svc", "scopes": ["manage_accounts"]}).to_string()),
    )
    .await;
    let service = serde_json::from_slice::<api::CreatedServiceCredential>(&r.1)
        .unwrap()
        .secret;
    for (label, cred, status) in [
        ("signed out", Cred::SignedOut, StatusCode::UNAUTHORIZED),
        (
            "an ordinary account",
            Cred::token(&w.alice_token),
            StatusCode::FORBIDDEN,
        ),
        (
            "a service credential",
            Cred::service(&service),
            StatusCode::FORBIDDEN,
        ),
    ] {
        let (got, body) = set_mode(&w, &cred, "public").await;
        assert_eq!(got, status, "{label}: {}", String::from_utf8_lossy(&body));
    }

    let root = Identity {
        user: UserId::new("root"),
        credential: pyn_core::Credential::Session,
    };
    let alice = UserId::new("alice");
    access.grant_admin(&root, &alice).await.unwrap();
    let as_alice = Identity {
        user: alice.clone(),
        credential: pyn_core::Credential::Session,
    };
    let repo = w
        .state
        .repos
        .open_visible("alice", "game", Some(&as_alice))
        .await
        .unwrap()
        .0
        .record
        .id;
    let (_, scoped) = access
        .create_token(
            &as_alice,
            pyn_core::fixtures::new_token_for(
                &as_alice,
                "narrow",
                [Permission::Read],
                vec![repo],
                None,
            ),
        )
        .await
        .unwrap();
    let (got, body) = set_mode(&w, &Cred::token(&scoped), "public").await;
    assert_eq!(
        got,
        StatusCode::FORBIDDEN,
        "{}",
        String::from_utf8_lossy(&body)
    );
    assert_eq!(mode_of(&w).await, "private");

    let (got, _) = set_mode(&w, &Cred::token(&w.alice_token), "public").await;
    assert_eq!(got, StatusCode::OK, "an unscoped token of an administrator");
    assert_eq!(mode_of(&w).await, "public");
}

#[tokio::test]
async fn a_change_is_audited_and_setting_the_same_mode_is_not() {
    let w = world(AccessMode::Private).await;
    assert_eq!(
        set_mode(&w, &Cred::dev("root"), "private").await.0,
        StatusCode::OK
    );
    assert_eq!(
        set_mode(&w, &Cred::dev("root"), "public").await.0,
        StatusCode::OK
    );
    let (status, body) = call(
        &w.app,
        &Cred::dev("root"),
        Method::GET,
        "/v1/admin/audit",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let events: Value = serde_json::from_slice(&body).unwrap();
    let changes: Vec<&Value> = events["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["action"] == "server_access_mode_changed")
        .collect();
    assert_eq!(changes.len(), 1, "{events}");
    assert_eq!(changes[0]["actor"], "root");
    assert_eq!(
        changes[0]["detail"],
        "access mode changed from private to public"
    );
}
