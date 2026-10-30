//! The Git LFS Batch API with the basic transfer adapter (docs/concepts/lfs.md).

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
    AccessConfig, AccessService, AuthProvider, ChunkedStore, RegistrationMode, Repositories, Rules,
    SystemClock, UserId,
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
    alice: String,
    bob: String,
    carol: String,
    dave: String,
}

fn oid(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

async fn dev(app: &Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Vec<u8>) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header(api::DEV_USER_HEADER, "root")
        .body(Body::from(body.map(|b| b.to_string()).unwrap_or_default()))
        .unwrap();
    let r = app.clone().oneshot(req).await.unwrap();
    let status = r.status();
    (
        status,
        r.into_body().collect().await.unwrap().to_bytes().to_vec(),
    )
}

async fn token_for(app: &Router, name: &str) -> String {
    let body = json!({"username": name, "password": PASSWORD});
    let (status, _) = dev(app, "POST", "/v1/register", Some(body.clone())).await;
    assert_eq!(status, StatusCode::CREATED);
    let (_, bytes) = dev(app, "POST", "/v1/login", Some(body)).await;
    serde_json::from_slice::<api::CreatedToken>(&bytes)
        .unwrap()
        .token
}

async fn world_with(trust_forwarded: bool, visibility: &str) -> World {
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
        Rules::from_toml("[meta]\ndefault = \"shared\"\n").unwrap(),
    ));
    let app = router(AppState {
        repos,
        objects,
        access: access.clone(),
        auth: Arc::new(BearerAuth { access }),
        dev_auth: Some(Arc::new(DevHeaderAuth) as Arc<dyn AuthProvider>),
        trust_forwarded,
    });
    let alice = token_for(&app, "alice").await;
    let bob = token_for(&app, "bob").await;
    let carol = token_for(&app, "carol").await;
    let dave = token_for(&app, "dave").await;
    let created = call(
        &app,
        "POST",
        "/v1/repos",
        &alice,
        Some(json!({"name": "game", "visibility": visibility})),
    )
    .await;
    assert_eq!(created.0, StatusCode::CREATED);
    for (user, role) in [("bob", "writer"), ("carol", "reader")] {
        let r = call(
            &app,
            "PUT",
            &format!("/v1/repos/alice/game/members/{user}"),
            &alice,
            Some(json!({"role": role})),
        )
        .await;
        assert!(r.0.is_success(), "{r:?}");
    }
    World {
        app,
        alice,
        bob,
        carol,
        dave,
    }
}

async fn world() -> World {
    world_with(false, "private").await
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    token: &str,
    body: Option<Value>,
) -> (StatusCode, Vec<u8>) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from(body.map(|b| b.to_string()).unwrap_or_default()))
        .unwrap();
    let r = app.clone().oneshot(req).await.unwrap();
    let status = r.status();
    (
        status,
        r.into_body().collect().await.unwrap().to_bytes().to_vec(),
    )
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

async fn send(app: &Router, req: Request<Body>) -> Reply {
    let r = app.clone().oneshot(req).await.unwrap();
    Reply {
        status: r.status(),
        headers: r.headers().clone(),
        body: r.into_body().collect().await.unwrap().to_bytes().to_vec(),
    }
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

async fn batch_at(w: &World, base: &str, auth: Option<String>, body: Value) -> Reply {
    let mut b = Request::builder()
        .method("POST")
        .uri(format!("{base}/objects/batch"))
        .header("content-type", LFS)
        .header("host", "pyn.example");
    if let Some(a) = auth {
        b = b.header("authorization", a);
    }
    send(&w.app, b.body(Body::from(body.to_string())).unwrap()).await
}

async fn batch(w: &World, token: &str, body: Value) -> Reply {
    batch_at(w, BASE, Some(bearer(token)), body).await
}

async fn put_at(
    w: &World,
    base: &str,
    token: &str,
    oid: &str,
    declared: usize,
    bytes: Vec<u8>,
) -> Reply {
    send(
        &w.app,
        Request::builder()
            .method("PUT")
            .uri(format!("{base}/objects/{oid}"))
            .header("authorization", bearer(token))
            .header("content-type", "application/octet-stream")
            .header("content-length", declared.to_string())
            .body(Body::from(bytes))
            .unwrap(),
    )
    .await
}

async fn put(w: &World, token: &str, bytes: &[u8]) -> Reply {
    put_at(w, BASE, token, &oid(bytes), bytes.len(), bytes.to_vec()).await
}

async fn get_with(w: &World, token: &str, oid: &str, range: Option<&str>) -> Reply {
    let mut b = Request::builder()
        .method("GET")
        .uri(format!("{BASE}/objects/{oid}"))
        .header("authorization", bearer(token));
    if let Some(r) = range {
        b = b.header("range", r);
    }
    send(&w.app, b.body(Body::empty()).unwrap()).await
}

fn spec(bytes: &[u8]) -> Value {
    json!({"oid": oid(bytes), "size": bytes.len()})
}

fn upload_batch(objects: Vec<Value>) -> Value {
    json!({"operation": "upload", "transfers": ["basic"], "objects": objects, "hash_algo": "sha256"})
}

fn download_batch(objects: Vec<Value>) -> Value {
    json!({"operation": "download", "transfers": ["basic"], "objects": objects})
}

#[tokio::test]
async fn an_upload_batch_names_the_upload_and_verify_actions_with_absolute_urls() {
    let w = world().await;
    let bytes = b"binary asset";
    let r = batch(&w, &w.bob, upload_batch(vec![spec(bytes)])).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.headers["content-type"], LFS);
    let body = r.json();
    assert_eq!(body["transfer"], "basic");
    assert_eq!(body["hash_algo"], "sha256");
    let o = &body["objects"][0];
    assert_eq!(o["oid"], oid(bytes));
    assert_eq!(o["size"], bytes.len());
    assert_eq!(o["authenticated"], true);
    assert_eq!(
        o["actions"]["upload"]["href"],
        format!("http://pyn.example{BASE}/objects/{}", oid(bytes))
    );
    assert_eq!(
        o["actions"]["verify"]["href"],
        format!("http://pyn.example{BASE}/objects/verify")
    );
    assert!(o["actions"].get("download").is_none() && o.get("error").is_none());
}

#[tokio::test]
async fn the_forwarded_headers_shape_the_urls_only_when_trusted() {
    let body = upload_batch(vec![spec(b"x")]).to_string();
    let ask = |w: &World| {
        let req = Request::builder()
            .method("POST")
            .uri(format!("{BASE}/objects/batch"))
            .header("content-type", LFS)
            .header("authorization", bearer(&w.bob))
            .header("host", "internal:7878")
            .header("x-forwarded-host", "git.example.com")
            .header("x-forwarded-proto", "https")
            .body(Body::from(body.clone()))
            .unwrap();
        let app = w.app.clone();
        async move { send(&app, req).await }
    };
    let trusting = world_with(true, "private").await;
    let href = ask(&trusting).await.json()["objects"][0]["actions"]["upload"]["href"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        href.starts_with("https://git.example.com/v1/repos/"),
        "{href}"
    );
    let plain = world().await;
    let href = ask(&plain).await.json()["objects"][0]["actions"]["upload"]["href"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(href.starts_with("http://internal:7878/v1/repos/"), "{href}");
}

#[tokio::test]
async fn an_uploaded_object_downloads_and_batch_download_names_the_href() {
    let w = world().await;
    let bytes = b"the actual bytes";
    let up = put(&w, &w.bob, bytes).await;
    assert_eq!(up.status, StatusCode::OK);

    let r = batch(&w, &w.carol, download_batch(vec![spec(bytes)])).await;
    assert_eq!(r.status, StatusCode::OK);
    let o = &r.json()["objects"][0];
    assert_eq!(
        o["actions"]["download"]["href"],
        format!("http://pyn.example{BASE}/objects/{}", oid(bytes))
    );
    assert!(o["actions"].get("upload").is_none());

    let got = get_with(&w, &w.carol, &oid(bytes), None).await;
    assert_eq!(got.status, StatusCode::OK);
    assert_eq!(got.body, bytes);
    assert_eq!(got.headers["content-length"], bytes.len().to_string());
    assert_eq!(got.headers["accept-ranges"], "bytes");
}

#[tokio::test]
async fn a_batch_reports_each_objects_problem_without_failing_the_request() {
    let w = world().await;
    put(&w, &w.bob, b"known").await;
    let missing = spec(b"never uploaded");
    let r = batch(
        &w,
        &w.bob,
        download_batch(vec![
            spec(b"known"),
            missing.clone(),
            json!({"oid": "not-an-oid", "size": 3}),
            json!({"oid": oid(b"known"), "size": 99}),
            json!({"oid": oid(b"neg"), "size": -1}),
        ]),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    let objects = r.json()["objects"].as_array().unwrap().clone();
    assert!(objects[0].get("error").is_none());
    assert_eq!(objects[1]["error"]["code"], 404);
    assert!(objects[1].get("actions").is_none());
    assert_eq!(objects[2]["error"]["code"], 422);
    assert_eq!(
        objects[3]["error"]["code"], 422,
        "a known oid of another size"
    );
    assert_eq!(objects[4]["error"]["code"], 422);
}

#[tokio::test]
async fn an_upload_batch_skips_objects_the_server_has_and_refuses_huge_ones() {
    let w = world().await;
    put(&w, &w.bob, b"already here").await;
    let r = batch(
        &w,
        &w.bob,
        upload_batch(vec![
            spec(b"already here"),
            spec(b"new one"),
            json!({"oid": oid(b"enormous"), "size": 17u64 << 30}),
        ]),
    )
    .await;
    let objects = r.json()["objects"].as_array().unwrap().clone();
    assert!(objects[0].get("actions").is_none() && objects[0].get("error").is_none());
    assert!(objects[1]["actions"]["upload"].is_object());
    assert_eq!(objects[2]["error"]["code"], 413);
}

#[tokio::test]
async fn the_batch_request_itself_is_validated() {
    let w = world().await;
    let ask = |body: Value| batch(&w, &w.bob, body);
    let r = ask(json!({"operation": "erase", "objects": []})).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    let r = ask(json!({"operation": "download", "transfers": ["tus"], "objects": []})).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    let r = ask(json!({"operation": "download", "hash_algo": "sha1", "objects": []})).await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    let r = ask(json!({"operation": "download", "objects": []})).await;
    assert_eq!(r.status, StatusCode::OK, "no transfers means basic");
    assert_eq!(r.json()["objects"], json!([]));
    let r = ask(json!("nonsense")).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);

    let r = send(
        &w.app,
        Request::builder()
            .method("POST")
            .uri(format!("{BASE}/objects/batch"))
            .header("authorization", bearer(&w.bob))
            .header("content-type", "text/plain")
            .body(Body::from("{}"))
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
}

#[tokio::test]
async fn an_upload_whose_digest_differs_from_its_oid_is_refused_and_not_kept() {
    let w = world().await;
    let claimed = oid(b"what the client promised");
    let r = put_at(&w, BASE, &w.bob, &claimed, 5, b"other".to_vec()).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(r.json()["message"].as_str().unwrap().contains(&claimed));
    assert_eq!(
        get_with(&w, &w.bob, &claimed, None).await.status,
        StatusCode::NOT_FOUND
    );
    let r = batch(
        &w,
        &w.bob,
        download_batch(vec![json!({"oid": claimed, "size": 5})]),
    )
    .await;
    assert_eq!(r.json()["objects"][0]["error"]["code"], 404);
}

#[tokio::test]
async fn an_upload_of_another_length_than_declared_is_refused() {
    let w = world().await;
    let bytes = b"exactly twenty bytes";
    for (declared, sent) in [(25usize, bytes.to_vec()), (10, bytes.to_vec())] {
        let r = put_at(&w, BASE, &w.bob, &oid(bytes), declared, sent).await;
        assert_eq!(
            r.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "declared {declared}"
        );
    }
    assert_eq!(
        get_with(&w, &w.bob, &oid(bytes), None).await.status,
        StatusCode::NOT_FOUND
    );
    let r = send(
        &w.app,
        Request::builder()
            .method("PUT")
            .uri(format!("{BASE}/objects/{}", oid(bytes)))
            .header("authorization", bearer(&w.bob))
            .body(Body::from_stream(futures_util::stream::iter([Ok::<
                _,
                std::io::Error,
            >(
                axum::body::Bytes::from_static(bytes),
            )])))
            .unwrap(),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::BAD_REQUEST,
        "a Content-Length is required"
    );
}

#[tokio::test]
async fn a_multi_megabyte_object_goes_through_the_chunked_store_and_supports_ranges() {
    let w = world().await;
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let bytes: Vec<u8> = (0..5 * 1024 * 1024 + 123)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 24) as u8
        })
        .collect();
    let r = put_at(&w, BASE, &w.bob, &oid(&bytes), bytes.len(), bytes.clone()).await;
    assert_eq!(r.status, StatusCode::OK);

    let got = get_with(&w, &w.alice, &oid(&bytes), None).await;
    assert_eq!(got.status, StatusCode::OK);
    assert!(got.body == bytes, "the whole object round-trips");

    let from = 1_500_000usize;
    let part = get_with(
        &w,
        &w.alice,
        &oid(&bytes),
        Some(&format!("bytes={from}-{}", from + 999)),
    )
    .await;
    assert_eq!(part.status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(part.body, &bytes[from..from + 1000]);
    assert_eq!(
        part.headers["content-range"],
        format!("bytes {from}-{}/{}", from + 999, bytes.len())
    );
    assert_eq!(part.headers["content-length"], "1000");

    let tail = get_with(&w, &w.alice, &oid(&bytes), Some("bytes=-10")).await;
    assert_eq!(tail.body, &bytes[bytes.len() - 10..]);
    let past = get_with(
        &w,
        &w.alice,
        &oid(&bytes),
        Some(&format!("bytes={}-", bytes.len())),
    )
    .await;
    assert_eq!(past.status, StatusCode::RANGE_NOT_SATISFIABLE);
}

#[tokio::test]
async fn uploading_an_object_again_changes_nothing() {
    let w = world().await;
    let bytes = b"repeat me";
    assert_eq!(put(&w, &w.bob, bytes).await.status, StatusCode::OK);
    assert_eq!(put(&w, &w.alice, bytes).await.status, StatusCode::OK);
    let r = batch(&w, &w.bob, upload_batch(vec![spec(bytes)])).await;
    assert!(
        r.json()["objects"][0].get("actions").is_none(),
        "nothing left to do"
    );
    assert_eq!(get_with(&w, &w.bob, &oid(bytes), None).await.body, bytes);
    let r = put_at(&w, BASE, &w.bob, &oid(bytes), 3, bytes[..3].to_vec()).await;
    assert_eq!(
        r.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "another size for a known oid"
    );
}

#[tokio::test]
async fn verify_confirms_what_was_stored() {
    let w = world().await;
    let bytes = b"verify me";
    let verify = |token: String, body: Value| {
        let app = w.app.clone();
        async move {
            send(
                &app,
                Request::builder()
                    .method("POST")
                    .uri(format!("{BASE}/objects/verify"))
                    .header("authorization", bearer(&token))
                    .header("content-type", LFS)
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
        }
    };
    assert_eq!(
        verify(w.bob.clone(), spec(bytes)).await.status,
        StatusCode::NOT_FOUND
    );
    put(&w, &w.bob, bytes).await;
    assert_eq!(
        verify(w.bob.clone(), spec(bytes)).await.status,
        StatusCode::OK
    );
    let wrong = json!({"oid": oid(bytes), "size": 1});
    assert_eq!(
        verify(w.bob.clone(), wrong).await.status,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        verify(w.carol.clone(), spec(bytes)).await.status,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn roles_decide_who_can_download_and_who_can_upload() {
    let w = world().await;
    let bytes = b"role check";
    put(&w, &w.bob, bytes).await;

    let r = batch(&w, &w.carol, upload_batch(vec![spec(b"x")])).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "a reader cannot upload");
    assert_eq!(put(&w, &w.carol, b"x").await.status, StatusCode::FORBIDDEN);
    assert_eq!(
        get_with(&w, &w.carol, &oid(bytes), None).await.status,
        StatusCode::OK
    );
    let r = batch(&w, &w.carol, download_batch(vec![spec(bytes)])).await;
    assert_eq!(r.status, StatusCode::OK);

    for r in [
        batch(&w, &w.dave, download_batch(vec![spec(bytes)])).await,
        get_with(&w, &w.dave, &oid(bytes), None).await,
        put(&w, &w.dave, b"x").await,
    ] {
        assert_eq!(
            r.status,
            StatusCode::NOT_FOUND,
            "a private repository is not revealed"
        );
    }
}

#[tokio::test]
async fn credentials_are_required_even_for_a_public_repository() {
    let w = world_with(false, "public").await;
    let bytes = b"public bytes";
    put(&w, &w.bob, bytes).await;
    let anon = batch_at(&w, BASE, None, download_batch(vec![spec(bytes)])).await;
    assert_eq!(anon.status, StatusCode::UNAUTHORIZED);
    assert!(
        anon.headers["www-authenticate"]
            .to_str()
            .unwrap()
            .starts_with("Basic")
    );
    let anon_get = send(
        &w.app,
        Request::builder()
            .uri(format!("{BASE}/objects/{}", oid(bytes)))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(anon_get.status, StatusCode::UNAUTHORIZED);
    let bad = batch_at(&w, BASE, Some(bearer("pyn_nope")), download_batch(vec![])).await;
    assert_eq!(bad.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn basic_authentication_carries_the_token_as_the_password() {
    let w = world().await;
    let basic = |user: &str, token: &str| {
        format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("{user}:{token}"))
        )
    };
    let bytes = b"basic auth";
    let r = batch_at(
        &w,
        BASE,
        Some(basic("anything", &w.bob)),
        upload_batch(vec![spec(bytes)]),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    let r = send(
        &w.app,
        Request::builder()
            .method("PUT")
            .uri(format!("{BASE}/objects/{}", oid(bytes)))
            .header("authorization", basic("bob", &w.bob))
            .header("content-length", bytes.len().to_string())
            .body(Body::from(bytes.to_vec()))
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    let r = send(
        &w.app,
        Request::builder()
            .uri(format!("{BASE}/objects/{}", oid(bytes)))
            .header("authorization", basic("bob", &w.bob))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(
        (r.status, r.body.as_slice()),
        (StatusCode::OK, bytes.as_slice())
    );
    let r = batch_at(
        &w,
        BASE,
        Some(basic("bob", "wrong")),
        download_batch(vec![]),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_git_suffix_addresses_the_same_repository() {
    let w = world().await;
    let bytes = b"suffix";
    let suffixed = "/v1/repos/alice/game.git/info/lfs";
    let r = put_at(
        &w,
        suffixed,
        &w.bob,
        &oid(bytes),
        bytes.len(),
        bytes.to_vec(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    let r = batch_at(
        &w,
        suffixed,
        Some(bearer(&w.bob)),
        download_batch(vec![spec(bytes)]),
    )
    .await;
    assert_eq!(
        r.json()["objects"][0]["actions"]["download"]["href"],
        format!("http://pyn.example{suffixed}/objects/{}", oid(bytes))
    );
    assert_eq!(get_with(&w, &w.bob, &oid(bytes), None).await.body, bytes);
}

#[tokio::test]
async fn unknown_and_malformed_oids_on_the_object_routes() {
    let w = world().await;
    let missing = get_with(&w, &w.bob, &oid(b"nothing"), None).await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    assert_eq!(missing.headers["content-type"], LFS);
    assert_eq!(
        get_with(&w, &w.bob, "ABC", None).await.status,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let r = put_at(&w, BASE, &w.bob, "nothex", 1, b"x".to_vec()).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn lfs_objects_are_not_visible_to_other_repositories() {
    let w = world().await;
    let r = call(
        &w.app,
        "POST",
        "/v1/repos",
        &w.alice,
        Some(json!({"name": "other"})),
    )
    .await;
    assert_eq!(r.0, StatusCode::CREATED);
    let bytes = b"only in game";
    put(&w, &w.bob, bytes).await;
    let r = send(
        &w.app,
        Request::builder()
            .uri(format!(
                "/v1/repos/alice/other/info/lfs/objects/{}",
                oid(bytes)
            ))
            .header("authorization", bearer(&w.alice))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn natively_stored_files_are_not_served_through_lfs() {
    let w = world().await;
    let bytes = b"checked in the pyn way";
    let sent = send(
        &w.app,
        Request::builder()
            .method("PUT")
            .uri("/v1/repos/alice/game/objects")
            .header("authorization", bearer(&w.alice))
            .body(Body::from(bytes.to_vec()))
            .unwrap(),
    )
    .await;
    let content = sent.json()["content"].as_str().unwrap().to_string();
    let (status, _) = call(
        &w.app,
        "POST",
        "/v1/repos/alice/game/checkin",
        &w.alice,
        Some(json!({"path": "a.bin", "content": content, "base_revision": null, "message": "m"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let r = batch(&w, &w.alice, download_batch(vec![spec(bytes)])).await;
    assert_eq!(r.json()["objects"][0]["error"]["code"], 404);
}

#[tokio::test]
async fn a_repository_cap_refuses_lfs_uploads_per_object() {
    let w = world().await;
    let r = call(
        &w.app,
        "PUT",
        "/v1/repos/alice/game/storage-cap",
        &w.alice,
        Some(json!({"storage_bytes": 10})),
    )
    .await;
    assert_eq!(r.0, StatusCode::OK, "{}", String::from_utf8_lossy(&r.1));
    assert_eq!(put(&w, &w.bob, b"123456").await.status, StatusCode::OK);

    let r = batch(&w, &w.bob, upload_batch(vec![spec(b"abcdef"), spec(b"xy")])).await;
    let objects = r.json()["objects"].as_array().unwrap().clone();
    assert_eq!(objects[0]["error"]["code"], 413);
    assert!(objects[1]["actions"]["upload"].is_object());
    let r = put(&w, &w.bob, b"abcdef").await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert!(r.json()["message"].as_str().unwrap().contains("alice/game"));
    assert_eq!(
        get_with(&w, &w.bob, &oid(b"abcdef"), None).await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn a_frozen_owner_cannot_upload_but_can_still_download() {
    let w = world().await;
    let bytes = b"before the freeze";
    put(&w, &w.bob, bytes).await;
    let (status, _) = dev(
        &w.app,
        "PUT",
        "/v1/admin/owners/alice/freeze",
        Some(json!({"reason": "plan lapsed"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let r = batch(&w, &w.bob, upload_batch(vec![spec(b"new")])).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert!(r.json()["message"].as_str().unwrap().contains("frozen"));
    assert_eq!(put(&w, &w.bob, b"new").await.status, StatusCode::FORBIDDEN);
    assert_eq!(
        get_with(&w, &w.bob, &oid(b"new"), None).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(get_with(&w, &w.bob, &oid(bytes), None).await.body, bytes);
    let r = batch(&w, &w.bob, download_batch(vec![spec(bytes)])).await;
    assert_eq!(r.status, StatusCode::OK);

    dev(&w.app, "DELETE", "/v1/admin/owners/alice/freeze", None).await;
    assert_eq!(put(&w, &w.bob, b"new").await.status, StatusCode::OK);
}

fn git_lfs_available() -> bool {
    std::process::Command::new("git")
        .args(["lfs", "version"])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Stock `git lfs` pushing and fetching through a live server; skipped when it is not installed.
#[tokio::test(flavor = "multi_thread")]
async fn stock_git_lfs_pushes_and_fetches_objects() {
    if !git_lfs_available() {
        eprintln!("git lfs is not installed; skipping");
        return;
    }
    let w = world().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = w.app.clone();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let dir = tempfile::tempdir().unwrap();
    let url = format!("http://bob:{}@{addr}/v1/repos/alice/game", w.bob);
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir.path())
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .output()
            .unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned()
                + &String::from_utf8_lossy(&out.stderr),
        )
    };
    let bytes: Vec<u8> = (0..300_000u32).flat_map(|i| i.to_le_bytes()).collect();
    for args in [
        vec!["init", "-q"],
        vec!["remote", "add", "origin", url.as_str()],
        vec!["lfs", "install", "--local"],
        vec!["lfs", "track", "*.bin"],
    ] {
        let (ok, out) = git(&args);
        assert!(ok, "{args:?}: {out}");
    }
    std::fs::write(dir.path().join("asset.bin"), &bytes).unwrap();
    for args in [
        vec!["add", ".gitattributes", "asset.bin"],
        vec!["commit", "-q", "-m", "add"],
        vec!["lfs", "push", "--all", "origin"],
    ] {
        let (ok, out) = git(&args);
        assert!(ok, "{args:?}: {out}");
    }
    assert_eq!(
        get_with(&w, &w.bob, &oid(&bytes), None).await.body.len(),
        bytes.len()
    );

    std::fs::remove_dir_all(dir.path().join(".git/lfs/objects")).unwrap();
    std::fs::remove_file(dir.path().join("asset.bin")).unwrap();
    for args in [
        vec!["lfs", "fetch", "--all", "origin"],
        vec!["lfs", "checkout"],
    ] {
        let (ok, out) = git(&args);
        assert!(ok, "{args:?}: {out}");
    }
    assert_eq!(std::fs::read(dir.path().join("asset.bin")).unwrap(), bytes);
}
