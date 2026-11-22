//! The access mode change is written to the server log.

#[path = "common/cred.rs"]
mod cred;
use cred::{Cred, WithCred};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::Request;
use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore,
};
use pyn_core::{
    AccessConfig, AccessService, AuthProvider, Repositories, Rules, SystemClock, UserId,
};
use pyn_server::auth::{BearerAuth, DevHeaderAuth};
use pyn_server::observe::{LogFormat, subscriber};
use pyn_server::{AppState, router};
use tower::ServiceExt;

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = Capture;

    fn make_writer(&'a self) -> Capture {
        self.clone()
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_change_of_mode_is_logged() {
    let cap = Capture::default();
    let _guard = tracing::subscriber::set_default(subscriber(
        LogFormat::Json,
        tracing_subscriber::EnvFilter::new("info"),
        cap.clone(),
    ));
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
        .with_config(AccessConfig::default())
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
    let app = router(AppState {
        repos,
        objects,
        access: access.clone(),
        auth: Arc::new(BearerAuth { access }),
        dev_auth: Some(Arc::new(DevHeaderAuth) as Arc<dyn AuthProvider>),
        trust_forwarded: false,
        trusted_proxies: 1,
        updates: None,
        storage: Default::default(),
        certificate: Default::default(),
        ready: Default::default(),
    });
    let r = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/v1/admin/access")
                .header("content-type", "application/json")
                .cred(&Cred::dev("root"))
                .body(Body::from(r#"{"access_mode":"public"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let log = String::from_utf8(cap.0.lock().unwrap().clone()).unwrap();
    assert!(log.contains("server access mode changed"), "{log}");
    assert!(log.contains("\"to\":\"public\""), "{log}");
}
