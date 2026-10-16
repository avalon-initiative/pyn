//! Upload admission, quota reservation and cleanup through the HTTP API.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use pyn_core::memory::{MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore};
use pyn_core::{
    AccessConfig, AccessService, AuthProvider, ChunkedStore, ObjectStore, RegistrationMode,
    Repositories, Rules, SystemClock, UserId,
};
use pyn_fs::FsObjectStore;
use pyn_proto as api;
use pyn_server::auth::{BearerAuth, DevHeaderAuth};
use pyn_server::{AppState, router};
use serde_json::json;
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";

struct Rig {
    app: Router,
    root: PathBuf,
    _dir: tempfile::TempDir,
}

async fn rig(cap: u64, per_user: u32, per_server: u32) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("objects");
    let objects: Arc<dyn ObjectStore> = Arc::new(ChunkedStore::new(Arc::new(
        FsObjectStore::open(&root).await.unwrap(),
    )));
    let clock = Arc::new(SystemClock);
    let audit = Arc::new(MemoryAuditStore::new());
    let meta = Arc::new(MemoryMetadataStore::new());
    let access = Arc::new(
        AccessService::new(Arc::new(MemoryAccessStore::new()), clock.clone())
            .with_config(AccessConfig {
                registration: RegistrationMode::Open,
                require_email_verification: false,
                ..AccessConfig::default()
            })
            .with_registry(meta.clone())
            .with_audit(audit.clone()),
    );
    access.admin_for_tests(&UserId::new("root")).await.unwrap();
    let repos = Repositories::new(
        meta,
        objects.clone(),
        audit,
        access.clone(),
        clock,
        Rules::from_toml("[meta]\ndefault = \"shared\"\n").unwrap(),
    )
    .with_unreferenced_upload_cap(cap)
    .with_upload_limits(per_user, per_server);
    let app = router(AppState {
        repos: Arc::new(repos),
        objects,
        access: access.clone(),
        auth: Arc::new(BearerAuth { access }),
        dev_auth: Some(Arc::new(DevHeaderAuth) as Arc<dyn AuthProvider>),
        trust_forwarded: false,
    });
    Rig {
        app,
        root,
        _dir: dir,
    }
}

async fn json_call(
    app: &Router,
    who: &str,
    method: &str,
    uri: &str,
    body: serde_json::Value,
) -> (StatusCode, Vec<u8>) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header(api::DEV_USER_HEADER, who)
        .body(Body::from(body.to_string()))
        .unwrap();
    let r = app.clone().oneshot(req).await.unwrap();
    let status = r.status();
    (
        status,
        r.into_body().collect().await.unwrap().to_bytes().to_vec(),
    )
}

async fn user_with_repo(app: &Router, name: &str) -> String {
    let body = json!({"username": name, "password": PASSWORD});
    let (status, _) = json_call(app, "root", "POST", "/v1/register", body.clone()).await;
    assert_eq!(status, StatusCode::CREATED);
    let (_, login) = json_call(app, "root", "POST", "/v1/login", body).await;
    let token = serde_json::from_slice::<api::CreatedToken>(&login)
        .unwrap()
        .token;
    let req = Request::builder()
        .method("POST")
        .uri("/v1/repos")
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {token}"))
        .body(Body::from(json!({"name": "game"}).to_string()))
        .unwrap();
    assert_eq!(
        app.clone().oneshot(req).await.unwrap().status(),
        StatusCode::CREATED
    );
    token
}

fn put(token: &str, repo: &str, body: Body, len: usize) -> Request<Body> {
    Request::builder()
        .method("PUT")
        .uri(format!("/v1/repos/{repo}/objects"))
        .header("authorization", format!("Bearer {token}"))
        .header("content-length", len.to_string())
        .body(body)
        .unwrap()
}

fn stalled(first: &'static [u8]) -> Body {
    let head = futures_util::stream::iter([Ok::<_, std::io::Error>(Bytes::from_static(first))]);
    Body::from_stream(futures_util::StreamExt::chain(
        head,
        futures_util::stream::pending(),
    ))
}

fn files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
            } else {
                out.push(path);
            }
        }
    }
    out
}

#[tokio::test]
async fn parallel_uploads_past_the_budget_store_only_what_fits() {
    let rig = rig(3000, 32, 64).await;
    let alice = user_with_repo(&rig.app, "alice").await;
    let tasks: Vec<_> = (0..10u8)
        .map(|n| {
            let (app, token) = (rig.app.clone(), alice.clone());
            tokio::spawn(async move {
                let body = vec![b'a' + n; 1000];
                let r = app
                    .oneshot(put(&token, "alice/game", Body::from(body), 1000))
                    .await
                    .unwrap();
                r.status()
            })
        })
        .collect();
    let mut ok = 0;
    for task in tasks {
        match task.await.unwrap() {
            StatusCode::OK => ok += 1,
            other => assert_eq!(other, StatusCode::CONFLICT),
        }
    }
    assert_eq!(ok, 3);
    assert_eq!(files(&rig.root).len(), 3);
}

#[tokio::test]
async fn a_user_over_the_upload_limit_is_told_to_retry_and_a_dropped_upload_frees_the_slot() {
    let rig = rig(1 << 20, 1, 64).await;
    let alice = user_with_repo(&rig.app, "alice").await;
    let held = tokio::spawn({
        let (app, token) = (rig.app.clone(), alice.clone());
        async move {
            app.oneshot(put(&token, "alice/game", stalled(b"start"), 100))
                .await
        }
    });
    tokio::time::sleep(Duration::from_millis(150)).await;

    let r = rig
        .app
        .clone()
        .oneshot(put(&alice, "alice/game", Body::from("abc"), 3))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(r.headers().contains_key("retry-after"));
    let body = r.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        serde_json::from_slice::<api::ErrorBody>(&body)
            .unwrap()
            .code,
        "uploads_busy"
    );

    held.abort();
    let _ = held.await;
    for _ in 0..50 {
        let r = rig
            .app
            .clone()
            .oneshot(put(&alice, "alice/game", Body::from("abc"), 3))
            .await
            .unwrap();
        if r.status() == StatusCode::OK {
            let leftovers: Vec<_> = files(&rig.root)
                .into_iter()
                .filter(|p| {
                    p.file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with(".tmp-")
                })
                .collect();
            assert!(leftovers.is_empty(), "{leftovers:?}");
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the slot was not freed");
}

#[tokio::test]
async fn the_server_wide_upload_limit_applies_across_users() {
    let rig = rig(1 << 20, 8, 1).await;
    let alice = user_with_repo(&rig.app, "alice").await;
    let bob = user_with_repo(&rig.app, "bob").await;
    let held = tokio::spawn({
        let (app, token) = (rig.app.clone(), alice.clone());
        async move {
            app.oneshot(put(&token, "alice/game", stalled(b"start"), 100))
                .await
        }
    });
    tokio::time::sleep(Duration::from_millis(150)).await;
    let r = rig
        .app
        .clone()
        .oneshot(put(&bob, "bob/game", Body::from("abc"), 3))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
    held.abort();
}
