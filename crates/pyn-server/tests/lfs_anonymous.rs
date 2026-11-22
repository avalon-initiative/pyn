//! Anonymous Git LFS download: a signed-out caller downloads the objects of a public repository on a public server;
//! everything else on the LFS routes keeps needing a credential (docs/concepts/repositories.md).

#[path = "common/cred.rs"]
mod cred;
use cred::{Cred, WithCred};
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine;
use http_body_util::BodyExt;
use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore,
};
use pyn_core::{
    AccessConfig, AccessMode, AccessService, AuthProvider, ChunkedStore, Credential, Identity,
    RegistrationMode, Repositories, Rules, SystemClock, UserId, Visibility,
};
use pyn_proto as api;
use pyn_server::auth::{BearerAuth, DevHeaderAuth};
use pyn_server::{AppState, router};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tower::ServiceExt;

const LFS: &str = "application/vnd.git-lfs+json";
const BYTES: &[u8] = b"public binary asset";

struct World {
    app: Router,
    alice: String,
}

struct Reply {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: Vec<u8>,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&self.body)))
    }
}

fn oid() -> String {
    hex::encode(Sha256::digest(BYTES))
}

fn base(repo: &str) -> String {
    format!("/v1/repos/alice/{repo}/info/lfs")
}

async fn send(app: &Router, req: Request<Body>) -> Reply {
    let r = app.clone().oneshot(req).await.unwrap();
    Reply {
        status: r.status(),
        headers: r.headers().clone(),
        body: r.into_body().collect().await.unwrap().to_bytes().to_vec(),
    }
}

async fn world(mode: AccessMode) -> World {
    let clock = Arc::new(SystemClock);
    let objects = Arc::new(ChunkedStore::new(Arc::new(MemoryObjectStore::new())));
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
        Rules::from_toml("[meta]\ndefault = \"shared\"\n").unwrap(),
    ));
    let state = AppState {
        repos: repos.clone(),
        objects,
        access: access.clone(),
        auth: Arc::new(BearerAuth {
            access: access.clone(),
        }),
        dev_auth: Some(Arc::new(DevHeaderAuth) as Arc<dyn AuthProvider>),
        trust_forwarded: false,
        trusted_proxies: 1,
        updates: None,
        storage: Default::default(),
        ready: Default::default(),
    };
    let app = router(state);
    let body = json!({"username": "alice", "password": "correct horse battery"}).to_string();
    let mut alice = String::new();
    for uri in ["/v1/register", "/v1/login"] {
        let r = send(
            &app,
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .cred(&Cred::SignedOut)
                .body(Body::from(body.clone()))
                .unwrap(),
        )
        .await;
        if uri == "/v1/login" {
            alice = serde_json::from_slice::<api::CreatedToken>(&r.body)
                .unwrap()
                .token;
        }
    }
    let who = UserId::new("alice");
    let as_alice = Identity {
        user: who.clone(),
        credential: Credential::Session,
    };
    for (name, visibility) in [("open", Visibility::Public), ("game", Visibility::Private)] {
        repos
            .create(&as_alice, &who, name, Some(visibility), None)
            .await
            .unwrap();
    }
    let w = World { app, alice };
    for repo in ["open", "game"] {
        let r = put(&w, repo, &Cred::bearer(&w.alice)).await;
        assert_eq!(r.status, StatusCode::OK, "{repo}");
    }
    w
}

async fn put(w: &World, repo: &str, cred: &Cred) -> Reply {
    send(
        &w.app,
        Request::builder()
            .method("PUT")
            .uri(format!("{}/objects/{}", base(repo), oid()))
            .cred(cred)
            .header("content-type", "application/octet-stream")
            .header("content-length", BYTES.len().to_string())
            .body(Body::from(BYTES.to_vec()))
            .unwrap(),
    )
    .await
}

async fn batch(w: &World, repo: &str, cred: &Cred, operation: &str) -> Reply {
    send(
        &w.app,
        Request::builder()
            .method("POST")
            .uri(format!("{}/objects/batch", base(repo)))
            .header("content-type", LFS)
            .header("host", "pyn.example")
            .cred(cred)
            .body(Body::from(
                json!({
                    "operation": operation,
                    "transfers": ["basic"],
                    "objects": [{"oid": oid(), "size": BYTES.len()}],
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await
}

async fn get(w: &World, repo: &str, cred: &Cred) -> Reply {
    send(
        &w.app,
        Request::builder()
            .uri(format!("{}/objects/{}", base(repo), oid()))
            .cred(cred)
            .body(Body::empty())
            .unwrap(),
    )
    .await
}

async fn plain(w: &World, method: &str, uri: String, extra: &[(&str, &str)], body: &str) -> Reply {
    let mut b = Request::builder().method(method).uri(uri);
    for (k, v) in extra {
        b = b.header(*k, *v);
    }
    send(&w.app, b.body(Body::from(body.to_string())).unwrap()).await
}

fn is_lfs_refusal(r: &Reply, status: StatusCode) {
    assert_eq!(r.status, status, "{}", String::from_utf8_lossy(&r.body));
    assert_eq!(r.headers["content-type"], LFS);
    assert!(r.json()["message"].is_string());
}

fn is_sign_in_demand(r: &Reply) {
    is_lfs_refusal(r, StatusCode::UNAUTHORIZED);
    assert_eq!(r.headers["www-authenticate"], "Basic realm=\"pyn\"");
}

#[tokio::test]
async fn a_signed_out_caller_downloads_from_a_public_repository_on_a_public_server() {
    let w = world(AccessMode::Public).await;
    let r = batch(&w, "open", &Cred::SignedOut, "download").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.headers["content-type"], LFS);
    let j = r.json();
    assert!(j.get("authenticated").is_none() && j["objects"][0].get("authenticated").is_none());
    let action = &j["objects"][0]["actions"]["download"];
    assert_eq!(
        action["href"],
        format!(
            "http://pyn.example/v1/repos/alice/open/info/lfs/objects/{}",
            oid()
        )
    );
    assert!(action.get("header").is_none(), "nothing identity-bound");
    assert!(j["objects"][0]["actions"].get("upload").is_none());

    let r = get(&w, "open", &Cred::SignedOut).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body, BYTES);
    let r = plain(
        &w,
        "GET",
        format!("{}/objects/{}", base("open"), oid()),
        &[("range", "bytes=0-5")],
        "",
    )
    .await;
    assert_eq!(
        (r.status, r.body.as_slice()),
        (StatusCode::PARTIAL_CONTENT, &BYTES[..6])
    );
}

#[tokio::test]
async fn a_signed_out_caller_still_cannot_upload_verify_lock_or_use_tus() {
    let w = world(AccessMode::Public).await;
    let open = base("open");
    is_sign_in_demand(&batch(&w, "open", &Cred::SignedOut, "upload").await);
    is_sign_in_demand(&put(&w, "open", &Cred::SignedOut).await);
    let verify = plain(
        &w,
        "POST",
        format!("{open}/objects/verify"),
        &[("content-type", LFS)],
        &json!({"oid": oid(), "size": BYTES.len()}).to_string(),
    )
    .await;
    is_sign_in_demand(&verify);
    let list = plain(&w, "GET", format!("{open}/locks"), &[], "").await;
    is_sign_in_demand(&list);
    let create = plain(
        &w,
        "POST",
        format!("{open}/locks"),
        &[("content-type", LFS)],
        r#"{"path":"a.bin"}"#,
    )
    .await;
    is_sign_in_demand(&create);
    let locks_verify = plain(
        &w,
        "POST",
        format!("{open}/locks/verify"),
        &[("content-type", LFS)],
        "{}",
    )
    .await;
    is_sign_in_demand(&locks_verify);
    let unlock = plain(
        &w,
        "POST",
        format!("{open}/locks/abc/unlock"),
        &[("content-type", LFS)],
        "{}",
    )
    .await;
    is_sign_in_demand(&unlock);
    let tus_url = format!("{open}/objects/{}/tus", oid());
    for method in ["HEAD", "POST", "PATCH", "DELETE"] {
        let r = plain(
            &w,
            method,
            tus_url.clone(),
            &[
                ("tus-resumable", "1.0.0"),
                ("upload-length", "1"),
                ("content-type", "application/offset+octet-stream"),
                ("upload-offset", "0"),
            ],
            "x",
        )
        .await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "tus {method}");
    }
    let unknown = batch(&w, "open", &Cred::SignedOut, "mirror").await;
    assert_eq!(unknown.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn a_private_repository_answers_a_signed_out_download_as_missing() {
    let w = world(AccessMode::Public).await;
    let missing = batch(&w, "ghost", &Cred::SignedOut, "download").await;
    let hidden = batch(&w, "game", &Cred::SignedOut, "download").await;
    is_lfs_refusal(&hidden, StatusCode::NOT_FOUND);
    assert_eq!(
        String::from_utf8_lossy(&hidden.body).replace("game", "ghost"),
        String::from_utf8_lossy(&missing.body)
    );
    let r = get(&w, "game", &Cred::SignedOut).await;
    is_lfs_refusal(&r, StatusCode::NOT_FOUND);
    // An upload batch is told to sign in whichever repository it names, as before.
    is_sign_in_demand(&batch(&w, "game", &Cred::SignedOut, "upload").await);
}

#[tokio::test]
async fn a_private_server_keeps_asking_a_signed_out_download_to_sign_in() {
    let w = world(AccessMode::Private).await;
    for repo in ["open", "game"] {
        is_sign_in_demand(&batch(&w, repo, &Cred::SignedOut, "download").await);
        is_sign_in_demand(&batch(&w, repo, &Cred::SignedOut, "upload").await);
        is_sign_in_demand(&get(&w, repo, &Cred::SignedOut).await);
    }
    // Signed in, on either kind of repository, downloads as before, with a bearer token or git-lfs's Basic.
    for repo in ["open", "game"] {
        assert_eq!(
            batch(&w, repo, &Cred::bearer(&w.alice), "download")
                .await
                .status,
            StatusCode::OK
        );
        let r = get(&w, repo, &Cred::bearer(&w.alice)).await;
        assert_eq!((r.status, r.body.as_slice()), (StatusCode::OK, BYTES));
        let auth = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("alice:{}", w.alice))
        );
        let r = plain(
            &w,
            "GET",
            format!("{}/objects/{}", base(repo), oid()),
            &[("authorization", &auth)],
            "",
        )
        .await;
        assert_eq!(r.status, StatusCode::OK, "Basic on {repo}");
    }
}

#[tokio::test]
async fn a_signed_in_caller_and_a_bad_credential_behave_as_before_on_a_public_server() {
    let w = world(AccessMode::Public).await;
    for repo in ["open", "game"] {
        let r = batch(&w, repo, &Cred::bearer(&w.alice), "download").await;
        assert_eq!(r.status, StatusCode::OK, "{repo}");
        let r = batch(&w, repo, &Cred::bearer(&w.alice), "upload").await;
        assert_eq!(r.status, StatusCode::OK, "{repo}");
        assert_eq!(
            get(&w, repo, &Cred::bearer(&w.alice)).await.status,
            StatusCode::OK
        );
    }
    let bad = Cred::Token("pyn_wrong".into());
    is_sign_in_demand(&batch(&w, "open", &bad, "download").await);
    is_sign_in_demand(&get(&w, "open", &bad).await);
}
