//! The tus transfer adapter of the Git LFS Batch API (docs/concepts/lfs.md).

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use chrono::{Duration, TimeZone, Utc};
use http_body_util::BodyExt;
use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore,
};
use pyn_core::{
    AccessConfig, AccessService, AuthProvider, ChunkedStore, ManualClock, RegistrationMode,
    Repositories, Rules, UserId,
};
use pyn_proto as api;
use pyn_server::auth::{BearerAuth, DevHeaderAuth};
use pyn_server::{AppState, router};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";
const BASE: &str = "/v1/repos/alice/game/info/lfs";
const LFS: &str = "application/vnd.git-lfs+json";

struct World {
    app: Router,
    clock: Arc<ManualClock>,
    alice: String,
    bob: String,
    carol: String,
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl Reply {
    fn header(&self, name: &str) -> &str {
        self.headers
            .get(name)
            .unwrap_or_else(|| panic!("no {name} in {:?}", self.headers))
            .to_str()
            .unwrap()
    }

    fn offset(&self) -> u64 {
        self.header("upload-offset").parse().unwrap()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap()
    }
}

fn oid(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

async fn send(app: &Router, req: Request<Body>) -> Reply {
    let r = app.clone().oneshot(req).await.unwrap();
    Reply {
        status: r.status(),
        headers: r.headers().clone(),
        body: r.into_body().collect().await.unwrap().to_bytes().to_vec(),
    }
}

async fn json_call(
    app: &Router,
    method: &str,
    uri: &str,
    auth: (&str, &str),
    body: Option<Value>,
) -> Reply {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header(auth.0, auth.1)
        .header("content-type", "application/json")
        .body(Body::from(body.map(|b| b.to_string()).unwrap_or_default()))
        .unwrap();
    send(app, req).await
}

async fn token_for(app: &Router, name: &str) -> String {
    let body = json!({"username": name, "password": PASSWORD});
    let dev = (api::DEV_USER_HEADER, "root");
    let r = json_call(app, "POST", "/v1/register", dev, Some(body.clone())).await;
    assert_eq!(r.status, StatusCode::CREATED);
    let r = json_call(app, "POST", "/v1/login", dev, Some(body)).await;
    serde_json::from_slice::<api::CreatedToken>(&r.body)
        .unwrap()
        .token
}

async fn world() -> World {
    let clock = Arc::new(ManualClock::new(
        Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
    ));
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
        clock.clone(),
        Rules::from_toml("[meta]\ndefault = \"shared\"\n").unwrap(),
    ));
    let app = router(AppState {
        repos,
        objects,
        access: access.clone(),
        auth: Arc::new(BearerAuth { access }),
        dev_auth: Some(Arc::new(DevHeaderAuth) as Arc<dyn AuthProvider>),
        trust_forwarded: false,
    });
    let alice = token_for(&app, "alice").await;
    let bob = token_for(&app, "bob").await;
    let carol = token_for(&app, "carol").await;
    let bearer = |t: &str| ("authorization", format!("Bearer {t}"));
    let created = json_call(
        &app,
        "POST",
        "/v1/repos",
        (bearer(&alice).0, &bearer(&alice).1),
        Some(json!({"name": "game", "visibility": "private"})),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED);
    for (user, role) in [("bob", "writer"), ("carol", "reader")] {
        let r = json_call(
            &app,
            "PUT",
            &format!("/v1/repos/alice/game/members/{user}"),
            (bearer(&alice).0, &bearer(&alice).1),
            Some(json!({"role": role})),
        )
        .await;
        assert!(r.status.is_success());
    }
    World {
        app,
        clock,
        alice,
        bob,
        carol,
    }
}

fn tus_url(bytes: &[u8]) -> String {
    format!("{BASE}/objects/{}/tus", oid(bytes))
}

async fn tus(w: &World, method: &str, url: &str, token: &str, h: &[(&str, String)]) -> Reply {
    tus_body(w, method, url, token, h, Body::empty()).await
}

async fn tus_body(
    w: &World,
    method: &str,
    url: &str,
    token: &str,
    h: &[(&str, String)],
    body: Body,
) -> Reply {
    let mut b = Request::builder()
        .method(method)
        .uri(url)
        .header("authorization", format!("Bearer {token}"))
        .header("host", "pyn.example")
        .header("tus-resumable", "1.0.0");
    for (k, v) in h {
        b = b.header(*k, v);
    }
    send(&w.app, b.body(body).unwrap()).await
}

async fn create(w: &World, token: &str, bytes: &[u8]) -> Reply {
    let len = bytes.len().to_string();
    tus(w, "POST", &tus_url(bytes), token, &[("upload-length", len)]).await
}

fn patch_headers(offset: u64, len: usize) -> Vec<(&'static str, String)> {
    vec![
        ("content-type", "application/offset+octet-stream".into()),
        ("upload-offset", offset.to_string()),
        ("content-length", len.to_string()),
    ]
}

async fn patch(w: &World, token: &str, url: &str, offset: u64, bytes: &[u8]) -> Reply {
    let h = patch_headers(offset, bytes.len());
    tus_body(w, "PATCH", url, token, &h, Body::from(bytes.to_vec())).await
}

async fn download(w: &World, token: &str, bytes: &[u8]) -> Reply {
    let req = Request::builder()
        .uri(format!("{BASE}/objects/{}", oid(bytes)))
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    send(&w.app, req).await
}

async fn batch(w: &World, token: &str, body: Value) -> Reply {
    let req = Request::builder()
        .method("POST")
        .uri(format!("{BASE}/objects/batch"))
        .header("content-type", LFS)
        .header("host", "pyn.example")
        .header("authorization", format!("Bearer {token}"))
        .body(Body::from(body.to_string()))
        .unwrap();
    send(&w.app, req).await
}

fn content(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i * 7 % 251) as u8).collect()
}

#[tokio::test]
async fn the_batch_negotiates_tus_for_uploads_and_basic_otherwise() {
    let w = world().await;
    let bytes = b"negotiated";
    let object = json!({"oid": oid(bytes), "size": bytes.len()});
    let ask = |op: &str, transfers: Value| {
        batch(
            &w,
            &w.bob,
            json!({"operation": op, "transfers": transfers, "objects": [object]}),
        )
    };

    let r = ask("upload", json!(["tus", "basic"])).await;
    assert_eq!(r.status, StatusCode::OK);
    let j = r.json();
    assert_eq!(j["transfer"], "tus");
    let href = format!(
        "http://pyn.example/v1/repos/alice/game/info/lfs/objects/{}/tus",
        oid(bytes)
    );
    assert_eq!(j["objects"][0]["actions"]["upload"]["href"], href);
    assert!(j["objects"][0]["actions"]["verify"]["href"].is_string());

    let basic = ask("upload", json!(["basic"])).await.json();
    assert_eq!(basic["transfer"], "basic");
    assert!(
        !basic["objects"][0]["actions"]["upload"]["href"]
            .as_str()
            .unwrap()
            .ends_with("/tus")
    );
    assert_eq!(ask("upload", json!([])).await.json()["transfer"], "basic");
    assert_eq!(
        ask("download", json!(["tus", "basic"])).await.json()["transfer"],
        "basic"
    );
    assert_eq!(
        ask("download", json!(["tus"])).await.status,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        ask("upload", json!(["tus"])).await.json()["transfer"],
        "tus"
    );
}

#[tokio::test]
async fn an_object_is_created_appended_in_chunks_and_verified_on_completion() {
    let w = world().await;
    let bytes = content(10_000);
    let url = tus_url(&bytes);

    let r = tus(&w, "HEAD", &url, &w.bob, &[]).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    let r = create(&w, &w.bob, &bytes).await;
    assert_eq!(r.status, StatusCode::CREATED);
    assert_eq!(r.header("tus-resumable"), "1.0.0");
    assert_eq!(r.header("location"), format!("http://pyn.example{url}"));
    assert_eq!(r.offset(), 0);

    let mut at = 0;
    for part in bytes.chunks(3_000) {
        let r = patch(&w, &w.bob, &url, at as u64, part).await;
        assert_eq!(r.status, StatusCode::NO_CONTENT, "{:?}", r.body);
        at += part.len();
        assert_eq!(r.offset(), at as u64);
        if at < bytes.len() {
            let head = tus(&w, "HEAD", &url, &w.bob, &[]).await;
            assert_eq!((head.status, head.offset()), (StatusCode::OK, at as u64));
            assert_eq!(head.header("upload-length"), "10000");
            assert!(head.headers.contains_key("upload-expires"));
        }
    }
    assert_eq!(download(&w, &w.bob, &bytes).await.body, bytes);
    let gone = tus(&w, "HEAD", &url, &w.bob, &[]).await;
    assert_eq!(
        gone.status,
        StatusCode::NOT_FOUND,
        "a finished upload is closed"
    );
}

#[tokio::test]
async fn a_dropped_connection_keeps_what_arrived_and_resumes_from_head() {
    let w = world().await;
    let bytes = content(100_000);
    let url = tus_url(&bytes);
    create(&w, &w.bob, &bytes).await;

    let torn = futures_util::stream::iter(vec![
        Ok::<_, std::io::Error>(axum::body::Bytes::copy_from_slice(&bytes[..40_000])),
        Err(std::io::Error::other("connection reset")),
    ]);
    let h = patch_headers(0, bytes.len());
    let _ = tus_body(&w, "PATCH", &url, &w.bob, &h, Body::from_stream(torn)).await;

    let head = tus(&w, "HEAD", &url, &w.bob, &[]).await;
    assert_eq!(head.offset(), 40_000);

    let r = patch(&w, &w.bob, &url, 40_000, &bytes[40_000..]).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert_eq!(download(&w, &w.bob, &bytes).await.body, bytes);
}

#[tokio::test]
async fn a_wrong_offset_or_a_malformed_request_is_refused_and_changes_nothing() {
    let w = world().await;
    let bytes = content(1_000);
    let url = tus_url(&bytes);
    create(&w, &w.bob, &bytes).await;
    patch(&w, &w.bob, &url, 0, &bytes[..400]).await;

    for offset in [0, 399, 401, 1_000] {
        let r = patch(&w, &w.bob, &url, offset, &bytes[400..500]).await;
        assert_eq!(r.status, StatusCode::CONFLICT, "offset {offset}");
    }
    let r = patch(&w, &w.bob, &url, 400, &bytes[400..]).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn protocol_preconditions_are_enforced() {
    let w = world().await;
    let bytes = content(500);
    let url = tus_url(&bytes);

    let no_length = tus(&w, "POST", &url, &w.bob, &[]).await;
    assert_eq!(no_length.status, StatusCode::UNPROCESSABLE_ENTITY);

    let req = Request::builder()
        .method("POST")
        .uri(&url)
        .header("authorization", format!("Bearer {}", w.bob))
        .header("upload-length", "500")
        .body(Body::empty())
        .unwrap();
    let r = send(&w.app, req).await;
    assert_eq!(r.status, StatusCode::PRECONDITION_FAILED);
    assert_eq!(r.header("tus-version"), "1.0.0");

    create(&w, &w.bob, &bytes).await;
    let h = vec![
        ("content-type", "application/octet-stream".to_string()),
        ("upload-offset", "0".to_string()),
        ("content-length", "500".to_string()),
    ];
    let r = tus_body(&w, "PATCH", &url, &w.bob, &h, Body::from(bytes.clone())).await;
    assert_eq!(r.status, StatusCode::UNSUPPORTED_MEDIA_TYPE);

    let r = patch(&w, &w.bob, &url, 0, &content(501)).await;
    assert_eq!(
        r.status,
        StatusCode::PAYLOAD_TOO_LARGE,
        "past Upload-Length"
    );
    assert_eq!(tus(&w, "HEAD", &url, &w.bob, &[]).await.offset(), 0);

    let o = tus(&w, "OPTIONS", &url, &w.bob, &[]).await;
    assert_eq!(o.status, StatusCode::NO_CONTENT);
    assert_eq!(o.header("tus-extension"), "creation,termination");

    let huge = tus(
        &w,
        "POST",
        &tus_url(b"enormous"),
        &w.bob,
        &[("upload-length", (17u64 << 30).to_string())],
    )
    .await;
    assert_eq!(huge.status, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn completion_checks_the_digest_and_keeps_nothing_on_a_mismatch() {
    let w = world().await;
    let bytes = content(2_000);
    let url = tus_url(&bytes);
    create(&w, &w.bob, &bytes).await;
    let mut wrong = bytes.clone();
    wrong[1_999] ^= 1;
    let r = patch(&w, &w.bob, &url, 0, &wrong).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        download(&w, &w.bob, &bytes).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        tus(&w, "HEAD", &url, &w.bob, &[]).await.status,
        StatusCode::NOT_FOUND,
        "the failed upload is closed so it can start over"
    );

    create(&w, &w.bob, &bytes).await;
    let r = patch(&w, &w.bob, &url, 0, &bytes).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert_eq!(download(&w, &w.bob, &bytes).await.body, bytes);
}

#[tokio::test]
async fn an_abandoned_upload_expires() {
    let w = world().await;
    let bytes = content(1_000);
    let url = tus_url(&bytes);
    create(&w, &w.bob, &bytes).await;
    patch(&w, &w.bob, &url, 0, &bytes[..300]).await;

    w.clock.advance(Duration::hours(23));
    assert_eq!(tus(&w, "HEAD", &url, &w.bob, &[]).await.offset(), 300);
    w.clock.advance(Duration::hours(2));
    assert_eq!(
        tus(&w, "HEAD", &url, &w.bob, &[]).await.status,
        StatusCode::NOT_FOUND
    );
    let r = patch(&w, &w.bob, &url, 300, &bytes[300..]).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn an_activity_extends_the_expiry() {
    let w = world().await;
    let bytes = content(1_000);
    let url = tus_url(&bytes);
    create(&w, &w.bob, &bytes).await;
    for (i, part) in bytes.chunks(250).enumerate() {
        w.clock.advance(Duration::hours(20));
        let r = patch(&w, &w.bob, &url, (i * 250) as u64, part).await;
        assert_eq!(r.status, StatusCode::NO_CONTENT, "chunk {i}");
    }
    assert_eq!(download(&w, &w.bob, &bytes).await.body, bytes);
}

#[tokio::test]
async fn permissions_and_ownership_apply_and_an_upload_can_be_terminated() {
    let w = world().await;
    let bytes = content(600);
    let url = tus_url(&bytes);
    assert_eq!(
        create(&w, &w.carol, &bytes).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        tus(&w, "HEAD", &url, &w.carol, &[]).await.status,
        StatusCode::FORBIDDEN
    );
    create(&w, &w.bob, &bytes).await;
    let r = patch(&w, &w.alice, &url, 0, &bytes).await;
    assert_eq!(
        r.status,
        StatusCode::NOT_FOUND,
        "another user's upload is not theirs"
    );

    let r = create(&w, &w.bob, &bytes).await;
    assert_eq!(
        r.status,
        StatusCode::CREATED,
        "creating again returns the open upload"
    );
    let r = create(&w, &w.bob, &content(601)).await;
    assert_eq!(
        r.status,
        StatusCode::CREATED,
        "another oid is another upload"
    );

    let r = tus(&w, "DELETE", &url, &w.bob, &[]).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let r = tus(&w, "HEAD", &url, &w.bob, &[]).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = tus(&w, "DELETE", &url, &w.bob, &[]).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn cleaning_up_the_parts_keeps_an_object_that_chunks_to_one_piece() {
    let w = world().await;
    let bytes = content(3 << 20);
    let url = tus_url(&bytes);
    create(&w, &w.bob, &bytes).await;
    let r = patch(&w, &w.bob, &url, 0, &bytes).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert_eq!(download(&w, &w.bob, &bytes).await.body, bytes);
}
