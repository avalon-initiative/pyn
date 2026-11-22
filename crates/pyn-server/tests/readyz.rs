//! `GET /readyz`: ready on a healthy server, not ready while draining or when the object store failed its probe;
//! `/healthz` stays a plain liveness check throughout.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore,
};
use pyn_core::{AccessConfig, AccessService, Repositories, Rules, SystemClock};
use pyn_proto as api;
use pyn_s3::S3ObjectStore;
use pyn_s3::testing::{BUCKET, Server};
use pyn_server::auth::BearerAuth;
use pyn_server::ready::Readiness;
use pyn_server::shutdown::Drain;
use pyn_server::storage::{StorageReport, spawn_monitor};
use pyn_server::{AppState, router};
use tower::ServiceExt;

fn app(storage: Arc<StorageReport>, ready: Readiness) -> Router {
    let clock = Arc::new(SystemClock);
    let objects = Arc::new(MemoryObjectStore::new());
    let audit = Arc::new(MemoryAuditStore::new());
    let access = Arc::new(
        AccessService::new(
            Arc::new(MemoryAccessStore::new()),
            clock.clone(),
            Arc::new(pyn_core::MemoryLimitEvents::new()),
        )
        .with_config(AccessConfig::default())
        .with_audit(audit.clone())
        .with_setup_token("0123456789abcdef0123456789abcdef")
        .unwrap(),
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
        dev_auth: None,
        trust_forwarded: false,
        trusted_proxies: 1,
        updates: None,
        storage,
        ready,
    })
}

async fn get(app: &Router, uri: &str) -> (StatusCode, Vec<u8>) {
    let r = app
        .clone()
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = r.status();
    (
        status,
        r.into_body().collect().await.unwrap().to_bytes().to_vec(),
    )
}

fn report(body: &[u8]) -> api::ReadinessReport {
    serde_json::from_slice(body).unwrap()
}

#[tokio::test]
async fn a_healthy_memory_store_is_ready_before_and_without_setup() {
    let app = app(Default::default(), Default::default());
    let (status, body) = get(&app, "/readyz").await;
    assert_eq!(status, StatusCode::OK);
    let report = report(&body);
    assert_eq!(report.status, api::ReadinessStatus::Ok);
    let names: Vec<_> = report.checks.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["metadata_store", "object_store"]);
    assert!(
        report
            .checks
            .iter()
            .all(|c| c.status == api::ReadinessCheckStatus::Ok && c.detail.is_none())
    );
    assert!(!String::from_utf8_lossy(&body).contains("detail"));
}

#[tokio::test]
async fn draining_is_503_at_once_and_health_stays_ok() {
    let drain = Drain::default();
    let app = app(Default::default(), Readiness::new(drain.clone()));
    assert_eq!(get(&app, "/readyz").await.0, StatusCode::OK);
    drain.start();
    let (status, body) = get(&app, "/readyz").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let report = report(&body);
    assert_eq!(report.status, api::ReadinessStatus::Draining);
    assert!(report.checks.is_empty());
    assert_eq!(
        get(&app, "/healthz").await,
        (StatusCode::OK, b"ok".to_vec())
    );
}

#[tokio::test]
async fn a_degraded_object_store_is_503_naming_the_failing_check() {
    let storage = Arc::new(StorageReport::new(api::StorageKind::S3, Some("b".into())));
    let app = app(storage.clone(), Default::default());
    storage.set_degraded(true);
    let (status, body) = get(&app, "/readyz").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let report = report(&body);
    assert_eq!(report.status, api::ReadinessStatus::Unavailable);
    let failing: Vec<_> = report
        .checks
        .iter()
        .filter(|c| c.status == api::ReadinessCheckStatus::Failing)
        .collect();
    assert_eq!(failing.len(), 1);
    assert_eq!(failing[0].name, "object_store");
    assert!(failing[0].detail.is_some());
    assert_eq!(get(&app, "/healthz").await.0, StatusCode::OK);
}

#[tokio::test]
async fn a_stopped_s3_server_turns_readiness_to_503_through_the_real_probe() {
    let s3 = Server::start();
    let store = Arc::new(S3ObjectStore::open(s3.settings()).await.unwrap());
    let storage = Arc::new(StorageReport::new(
        api::StorageKind::S3,
        Some(BUCKET.into()),
    ));
    spawn_monitor(
        store,
        storage.clone(),
        Duration::from_millis(50),
        Default::default(),
    );
    let app = app(storage, Default::default());
    assert_eq!(get(&app, "/readyz").await.0, StatusCode::OK);

    drop(s3);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let (status, body) = get(&app, "/readyz").await;
        if status == StatusCode::SERVICE_UNAVAILABLE {
            let text = String::from_utf8_lossy(&body).to_string();
            assert!(
                text.contains("object_store") && !text.contains("127.0.0.1"),
                "{text}"
            );
            break;
        }
        assert!(Instant::now() < deadline, "readiness never went to 503");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
