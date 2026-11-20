//! The update check: channel and version rules, the cache, silent failure, and the admin-only endpoint. No network.

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
    AccessConfig, AccessService, AuthProvider, ManualClock, RegistrationMode, Repositories, Rules,
};
use pyn_proto as api;
use pyn_server::auth::{BearerAuth, DevHeaderAuth};
use pyn_server::update_check::{
    FakeReleaseSource, RETRY_AFTER, Release, TTL, UpdateChecker, newer_release,
};
use pyn_server::{AppState, router};
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";

fn server(updates: Option<Arc<UpdateChecker>>) -> Router {
    let clock = Arc::new(ManualClock::new(chrono::Utc::now()));
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
            public_url: "https://pyn.example".into(),
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
        clock.clone(),
        Rules::empty(),
    ));
    router(AppState {
        repos,
        objects,
        access: access.clone(),
        auth: Arc::new(BearerAuth {
            access: access.clone(),
        }),
        dev_auth: Some(Arc::new(DevHeaderAuth) as Arc<dyn AuthProvider>),
        trust_forwarded: false,
        updates,
    })
}

struct Reply {
    status: StatusCode,
    body: Vec<u8>,
}

impl Reply {
    fn json<T: serde::de::DeserializeOwned>(&self) -> T {
        serde_json::from_slice(&self.body).unwrap_or_else(|e| {
            panic!("{e}: {}", String::from_utf8_lossy(&self.body));
        })
    }

    fn code(&self) -> String {
        self.json::<api::ErrorBody>().code
    }
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    auth: Option<(&str, &str)>,
    body: Option<serde_json::Value>,
) -> Reply {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    match auth {
        Some(("bearer", token)) => req = req.cred(&Cred::bearer(token)),
        Some((_, user)) => req = req.cred(&Cred::dev(user)),
        None => {}
    }
    let body = body.map(|b| b.to_string()).unwrap_or_default();
    let r = app
        .clone()
        .oneshot(req.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    Reply {
        status: r.status(),
        body: r.into_body().collect().await.unwrap().to_bytes().to_vec(),
    }
}

async fn sign_up(app: &Router, name: &str) {
    let r = call(
        app,
        "POST",
        "/v1/register",
        None,
        Some(serde_json::json!({"username": name, "password": PASSWORD})),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
}

async fn login(app: &Router, name: &str) -> Reply {
    call(
        app,
        "POST",
        "/v1/login",
        None,
        Some(serde_json::json!({"username": name, "password": PASSWORD})),
    )
    .await
}

async fn token(app: &Router, name: &str) -> String {
    let r = login(app, name).await;
    assert_eq!(r.status, StatusCode::OK);
    r.json::<api::CreatedToken>().token
}

fn release(tag: &str, prerelease: bool) -> Release {
    Release {
        tag: tag.into(),
        prerelease,
        url: format!("https://example.test/{tag}"),
    }
}

fn tag_of(found: Option<Release>) -> Option<String> {
    found.map(|r| r.tag)
}

#[test]
fn a_stable_server_only_hears_of_stable_releases() {
    let list = [
        release("v0.2.0-alpha", true),
        release("v0.1.5", false),
        release("v0.1.6-rc.1", true),
    ];
    assert_eq!(
        tag_of(newer_release("0.1.4", &list)).as_deref(),
        Some("v0.1.5")
    );
}

#[test]
fn a_pre_release_server_hears_of_both_and_takes_the_highest() {
    let list = [
        release("v0.1.5-alpha", true),
        release("v0.1.5-beta", true),
        release("v0.1.4", false),
    ];
    assert_eq!(
        tag_of(newer_release("0.1.4-alpha", &list)).as_deref(),
        Some("v0.1.5-beta")
    );
    let with_stable = [release("v0.1.5-alpha", true), release("v0.1.5", false)];
    assert_eq!(
        tag_of(newer_release("0.1.4-alpha", &with_stable)).as_deref(),
        Some("v0.1.5")
    );
}

#[test]
fn equal_older_and_garbage_are_not_newer() {
    let list = [
        release("v0.1.4", false),
        release("v0.1.3", false),
        release("nightly", false),
        release("v1", false),
        release("", false),
        release("release-2026-11", true),
    ];
    assert_eq!(newer_release("0.1.4", &list), None);
    assert_eq!(
        newer_release("0.1.4-alpha", &list).map(|r| r.tag),
        Some("v0.1.4".into())
    );
    assert_eq!(newer_release("not a version", &list), None);
}

fn checker(
    answer: Result<Vec<Release>, String>,
) -> (Arc<UpdateChecker>, Arc<FakeReleaseSource>, Arc<ManualClock>) {
    let source = Arc::new(FakeReleaseSource::new(answer));
    let clock = Arc::new(ManualClock::new(chrono::Utc::now()));
    let c = Arc::new(UpdateChecker::new(source.clone(), clock.clone(), "0.1.4"));
    (c, source, clock)
}

#[tokio::test]
async fn a_fresh_answer_is_served_from_the_cache_until_a_day_passes() {
    let (c, source, clock) = checker(Ok(vec![release("v0.1.5", false)]));
    assert!(c.status().latest.is_none() && c.status().checked_at.is_none());
    assert_eq!(
        source.calls(),
        0,
        "reading the status never asks the source"
    );

    assert!(c.refresh_if_due().await);
    assert!(!c.refresh_if_due().await);
    clock.advance(TTL - chrono::Duration::minutes(1));
    assert!(!c.refresh_if_due().await);
    assert_eq!(source.calls(), 1);
    let status = c.status();
    assert_eq!(status.latest.as_deref(), Some("0.1.5"));
    assert_eq!(status.prerelease, Some(false));
    assert_eq!(status.url.as_deref(), Some("https://example.test/v0.1.5"));
    assert!(status.checked_at.is_some() && status.enabled);

    clock.advance(chrono::Duration::minutes(2));
    assert!(c.refresh_if_due().await);
    assert_eq!(source.calls(), 2);
}

#[tokio::test]
async fn a_failed_lookup_keeps_the_old_answer_and_waits_before_trying_again() {
    let (c, source, clock) = checker(Ok(vec![release("v0.1.5", false)]));
    c.refresh_if_due().await;
    clock.advance(TTL);
    source.set(Err("offline".into()));
    assert!(c.refresh_if_due().await);
    assert_eq!(
        c.status().latest.as_deref(),
        Some("0.1.5"),
        "the last answer stays"
    );

    for _ in 0..5 {
        assert!(!c.refresh_if_due().await, "no retry inside the pause");
    }
    assert_eq!(source.calls(), 2);
    clock.advance(RETRY_AFTER);
    source.set(Ok(vec![release("v0.1.6", false)]));
    assert!(c.refresh_if_due().await);
    assert_eq!(c.status().latest.as_deref(), Some("0.1.6"));
}

#[tokio::test]
async fn a_failure_before_any_answer_leaves_the_status_unknown() {
    let (c, source, _) = checker(Err("offline".into()));
    c.refresh_if_due().await;
    let status = c.status();
    assert!(status.enabled && status.latest.is_none() && status.checked_at.is_none());
    assert_eq!(source.calls(), 1);
}

#[tokio::test]
async fn the_endpoint_is_for_administrators_and_says_disabled_when_the_check_is_off() {
    let app = server(None);
    sign_up(&app, "bob").await;
    let bob = token(&app, "bob").await;
    let denied = call(
        &app,
        "GET",
        "/v1/admin/update",
        Some(("bearer", &bob)),
        None,
    )
    .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    assert_eq!(denied.code(), "server_admin_required");
    let anon = call(&app, "GET", "/v1/admin/update", None, None).await;
    assert_eq!(anon.status, StatusCode::UNAUTHORIZED);

    let off = call(&app, "GET", "/v1/admin/update", Some(("dev", "root")), None).await;
    assert_eq!(off.status, StatusCode::OK);
    let doc: serde_json::Value = off.json();
    assert_eq!(doc["enabled"], false);
    assert_eq!(doc["current"], env!("CARGO_PKG_VERSION"));
    for absent in ["latest", "prerelease", "url", "checked_at"] {
        assert!(doc.get(absent).is_none(), "{absent}");
    }
}

#[tokio::test]
async fn the_endpoint_reports_the_cached_answer_without_asking_the_source() {
    let (c, source, _) = checker(Ok(vec![release("v0.1.5", false)]));
    let app = server(Some(c.clone()));
    let before = call(&app, "GET", "/v1/admin/update", Some(("dev", "root")), None).await;
    let before: api::UpdateStatus = before.json();
    assert!(before.enabled && before.latest.is_none() && before.checked_at.is_none());

    c.refresh_if_due().await;
    let after: api::UpdateStatus =
        call(&app, "GET", "/v1/admin/update", Some(("dev", "root")), None)
            .await
            .json();
    assert_eq!(after.latest.as_deref(), Some("0.1.5"));
    assert_eq!(after.current, "0.1.4");
    assert!(after.checked_at.is_some());
    assert_eq!(source.calls(), 1, "requests never reach the source");
}
