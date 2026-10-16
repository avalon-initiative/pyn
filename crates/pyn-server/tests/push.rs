use std::collections::BTreeMap;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use http_body_util::BodyExt;
use pyn_core::commit_record::{TreeChange, tree_of};
use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore,
};
use pyn_core::{
    AccessConfig, AccessService, AuthProvider, FileMode, ObjectId, REPO_ALGORITHM,
    RegistrationMode, RepoPath, Repositories, Rules, SystemClock,
};
use pyn_object::{Algorithm, Kind, Signature, hash_object};
use pyn_proto as api;
use pyn_server::auth::{BearerAuth, DevHeaderAuth};
use pyn_server::{AppState, router};
use serde_json::json;
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";
const RULES: &str = "[meta]\ndefault = \"shared\"\n[exclusive]\npaths = [\"Content/\"]\n";
const T0: i64 = 1_790_000_000;

fn app() -> Router {
    let clock = Arc::new(SystemClock);
    let objects = Arc::new(pyn_core::ChunkedStore::new(Arc::new(
        MemoryObjectStore::new(),
    )));
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
    let repos = Arc::new(Repositories::new(
        meta,
        objects.clone(),
        audit,
        access.clone(),
        clock,
        Rules::from_toml(RULES).unwrap(),
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

struct Reply {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: Vec<u8>,
}

impl Reply {
    fn json<T: serde::de::DeserializeOwned>(&self) -> T {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&self.body)))
    }

    fn code(&self) -> String {
        self.json::<api::ErrorBody>().code
    }
}

async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    token: &str,
    extra: Option<(&str, &str)>,
    body: Option<serde_json::Value>,
) -> Reply {
    let mut b = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {token}"));
    if let Some((k, v)) = extra {
        b = b.header(k, v);
    }
    let req = match body {
        Some(j) => b
            .header("content-type", "application/json")
            .body(Body::from(j.to_string())),
        None => b.body(Body::empty()),
    }
    .unwrap();
    let r = app.clone().oneshot(req).await.unwrap();
    let (parts, body) = r.into_parts();
    Reply {
        status: parts.status,
        headers: parts.headers,
        body: body.collect().await.unwrap().to_bytes().to_vec(),
    }
}

async fn sign_up(app: &Router, name: &str) -> String {
    let body = json!({"username": name, "password": PASSWORD});
    for uri in ["/v1/register", "/v1/login"] {
        let req = Request::post(uri)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let r = app.clone().oneshot(req).await.unwrap();
        if uri == "/v1/login" {
            let bytes = r.into_body().collect().await.unwrap().to_bytes();
            return serde_json::from_slice::<api::CreatedToken>(&bytes)
                .unwrap()
                .token;
        }
    }
    unreachable!()
}

/// `alice` owns `game`; `bob` can write and `rita` can only read.
struct World {
    app: Router,
    alice: String,
    bob: String,
    rita: String,
}

async fn world() -> World {
    let app = app();
    let alice = sign_up(&app, "alice").await;
    let bob = sign_up(&app, "bob").await;
    let rita = sign_up(&app, "rita").await;
    let r = send(
        &app,
        "POST",
        "/v1/repos",
        &alice,
        None,
        Some(json!({"name": "game"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED);
    for (who, role) in [("bob", "writer"), ("rita", "reader")] {
        let r = send(
            &app,
            "PUT",
            &format!("/v1/repos/alice/game/members/{who}"),
            &alice,
            None,
            Some(json!({"role": role})),
        )
        .await;
        assert_eq!(r.status, StatusCode::NO_CONTENT);
    }
    World {
        app,
        alice,
        bob,
        rita,
    }
}

async fn upload(w: &World, text: &str) -> ObjectId {
    let req = Request::put("/v1/repos/alice/game/objects")
        .header("authorization", format!("Bearer {}", w.bob))
        .body(Body::from(text.to_string()))
        .unwrap();
    let r = w.app.clone().oneshot(req).await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let bytes = r.into_body().collect().await.unwrap().to_bytes();
    let id = serde_json::from_slice::<api::PutObjectResponse>(&bytes)
        .unwrap()
        .content;
    ObjectId::from_hex(REPO_ALGORITHM, &id).unwrap()
}

/// A commit with its snapshot trees, as a client builds them.
struct Built {
    id: ObjectId,
    raw: Vec<u8>,
    trees: Vec<(ObjectId, Vec<u8>)>,
}

fn signed(name: &str, at: i64) -> Signature {
    Signature {
        name: name.as_bytes().to_vec(),
        email: format!("{name}@example.test").into_bytes(),
        time: at,
        offset_minutes: 0,
    }
}

fn build(
    files: &BTreeMap<&str, ObjectId>,
    parent: Option<ObjectId>,
    message: &str,
    n: i64,
) -> Built {
    let tree = tree_of(
        files
            .iter()
            .map(|(p, id)| TreeChange {
                path: RepoPath::new(*p).unwrap(),
                mode: FileMode::Regular,
                id: *id,
                removed: false,
            })
            .collect(),
    )
    .unwrap();
    let raw = pyn_object::Commit {
        tree: tree.root,
        parents: parent.into_iter().collect(),
        author: signed("carol", T0 + n),
        committer: signed("carol", T0 + n),
        extra_headers: vec![],
        message: format!("{message}\n").into_bytes(),
    }
    .encode(Algorithm::Sha256)
    .unwrap();
    let id = hash_object(Algorithm::Sha256, Kind::Commit, &raw).unwrap();
    Built {
        id,
        raw,
        trees: tree.trees,
    }
}

fn object(id: &ObjectId, body: &[u8]) -> serde_json::Value {
    json!({"id": id.to_hex(), "data": STANDARD.encode(body)})
}

fn push_body(parts: &[&Built]) -> serde_json::Value {
    json!({
        "commits": parts.iter().map(|b| object(&b.id, &b.raw)).collect::<Vec<_>>(),
        "trees": parts
            .iter()
            .flat_map(|b| b.trees.iter())
            .map(|(id, body)| object(id, body))
            .collect::<Vec<_>>(),
    })
}

async fn push(w: &World, token: &str, body: serde_json::Value) -> Reply {
    send(
        &w.app,
        "POST",
        "/v1/repos/alice/game/push",
        token,
        None,
        Some(body),
    )
    .await
}

async fn commit_count(w: &World) -> usize {
    let r = send(
        &w.app,
        "GET",
        "/v1/repos/alice/game/commits",
        &w.alice,
        None,
        None,
    )
    .await;
    r.json::<api::CommitPage>().commits.len()
}

#[tokio::test]
async fn a_push_lands_commits_under_their_ids_and_audits_the_pusher() {
    let w = world().await;
    let a = upload(&w, "alpha").await;
    let first = build(&BTreeMap::from([("Source/a.txt", a)]), None, "first", 1);
    let b = upload(&w, "beta").await;
    let second = build(
        &BTreeMap::from([("Source/a.txt", a), ("Source/b.txt", b)]),
        Some(first.id),
        "second",
        2,
    );

    let r = push(&w, &w.bob, push_body(&[&first, &second])).await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let done: api::PushResponse = r.json();
    assert_eq!(done.branch, "main");
    assert_eq!(done.head, second.id.to_hex());
    assert_eq!(done.revisions.len(), 2);
    assert!(done.revisions.iter().all(|r| r.author == "bob"));

    let r = send(
        &w.app,
        "GET",
        &format!("/v1/repos/alice/game/commits/{}", second.id.to_hex()),
        &w.alice,
        None,
        None,
    )
    .await;
    let commit: api::Commit = r.json();
    assert_eq!(commit.parents, vec![first.id.to_hex()]);
    assert_eq!(commit.author.name, "carol");

    let r = send(
        &w.app,
        "GET",
        "/v1/repos/alice/game/audit",
        &w.alice,
        None,
        None,
    )
    .await;
    let page: api::AuditPage = r.json();
    let pushed: Vec<_> = page
        .entries
        .iter()
        .filter(|e| e.action == "checkin")
        .collect();
    assert_eq!(pushed.len(), 2);
    assert!(
        pushed
            .iter()
            .all(|e| e.actor == "bob" && e.detail.contains("(push)"))
    );
}

#[tokio::test]
async fn an_exclusive_path_in_a_push_needs_the_pushers_live_lock() {
    let w = world().await;
    let c = upload(&w, "map").await;
    let commit = build(&BTreeMap::from([("Content/x.umap", c)]), None, "map", 1);

    let r = push(&w, &w.bob, push_body(&[&commit])).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::CONFLICT, "lock_required")
    );

    let lock = |token: String| {
        let app = w.app.clone();
        async move {
            send(
                &app,
                "POST",
                "/v1/repos/alice/game/checkout",
                &token,
                None,
                Some(json!({"path": "Content/x.umap", "base_revision": null})),
            )
            .await
        }
    };
    assert_eq!(lock(w.alice.clone()).await.status, StatusCode::OK);
    let r = push(&w, &w.bob, push_body(&[&commit])).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::CONFLICT, "lock_held")
    );
    assert_eq!(commit_count(&w).await, 0);

    send(
        &w.app,
        "POST",
        "/v1/repos/alice/game/release",
        &w.alice,
        None,
        Some(json!({"path": "Content/x.umap"})),
    )
    .await;
    assert_eq!(lock(w.bob.clone()).await.status, StatusCode::OK);
    let r = push(&w, &w.bob, push_body(&[&commit])).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(commit_count(&w).await, 1);
}

#[tokio::test]
async fn a_push_must_extend_the_branch_and_a_refusal_leaves_nothing() {
    let w = world().await;
    let a = upload(&w, "alpha").await;
    let b = upload(&w, "beta").await;
    let first = build(&BTreeMap::from([("Source/a.txt", a)]), None, "first", 1);
    let rival = build(&BTreeMap::from([("Source/b.txt", b)]), None, "rival", 2);
    assert_eq!(
        push(&w, &w.bob, push_body(&[&first])).await.status,
        StatusCode::OK
    );

    let r = push(&w, &w.bob, push_body(&[&rival])).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::CONFLICT, "not_fast_forward")
    );
    assert_eq!(commit_count(&w).await, 1);

    let mut body = push_body(&[&build(
        &BTreeMap::from([("Source/a.txt", a), ("Source/b.txt", b)]),
        Some(first.id),
        "second",
        3,
    )]);
    body["commits"][0]["id"] = json!(rival.id.to_hex());
    let r = push(&w, &w.bob, body).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::BAD_REQUEST, "push_invalid")
    );

    let mut body = push_body(&[&rival]);
    body["commits"][0]["data"] = json!("not base64!");
    let r = push(&w, &w.bob, body).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::BAD_REQUEST, "invalid_request")
    );

    let r = push(&w, &w.bob, json!({"commits": [], "trees": []})).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::BAD_REQUEST, "invalid_request")
    );

    let r = send(
        &w.app,
        "POST",
        "/v1/repos/alice/game/push",
        &w.bob,
        None,
        Some(json!({"branch": "nope", "commits": push_body(&[&rival])["commits"], "trees": push_body(&[&rival])["trees"]})),
    )
    .await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::NOT_FOUND, "branch_not_found")
    );
    assert_eq!(commit_count(&w).await, 1);
}

#[tokio::test]
async fn what_a_push_cannot_record_is_refused_whole_with_a_stable_code() {
    let w = world().await;
    let a = upload(&w, "alpha").await;
    let policy = upload(&w, "[meta]\ndefault = \"shared\"\n").await;
    let commit = build(
        &BTreeMap::from([(".pyn/pyn.toml", policy), ("Source/a.txt", a)]),
        None,
        "policy",
        1,
    );
    let r = push(&w, &w.bob, push_body(&[&commit])).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "push_unsupported")
    );
    assert_eq!(commit_count(&w).await, 0);
}

#[tokio::test]
async fn a_push_needs_the_checkin_permission() {
    let w = world().await;
    let a = upload(&w, "alpha").await;
    let commit = build(&BTreeMap::from([("Source/a.txt", a)]), None, "first", 1);
    let r = push(&w, &w.rita, push_body(&[&commit])).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::FORBIDDEN, "forbidden")
    );
    let r = push(&w, "pyn_unknown", push_body(&[&commit])).await;
    assert!(r.status.is_client_error());
    assert_eq!(commit_count(&w).await, 0);
}

#[tokio::test]
async fn a_blob_is_served_by_id_with_ranges_to_readers_of_its_repository_only() {
    let w = world().await;
    let a = upload(&w, "hello world").await;
    let uri = format!("/v1/repos/alice/game/objects/{}", a.to_hex());

    let r = send(&w.app, "GET", &uri, &w.rita, None, None).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::NOT_FOUND, "object_not_found")
    );

    let commit = build(&BTreeMap::from([("Source/a.txt", a)]), None, "first", 1);
    assert_eq!(
        push(&w, &w.bob, push_body(&[&commit])).await.status,
        StatusCode::OK
    );

    let r = send(&w.app, "GET", &uri, &w.rita, None, None).await;
    assert_eq!(
        (r.status, r.body.as_slice()),
        (StatusCode::OK, &b"hello world"[..])
    );
    assert_eq!(r.headers[api::CONTENT_HASH_HEADER], a.to_hex());
    assert_eq!(r.headers["accept-ranges"], "bytes");

    let r = send(
        &w.app,
        "GET",
        &uri,
        &w.rita,
        Some(("range", "bytes=6-")),
        None,
    )
    .await;
    assert_eq!(
        (r.status, r.body.as_slice()),
        (StatusCode::PARTIAL_CONTENT, &b"world"[..])
    );
    assert_eq!(r.headers["content-range"], "bytes 6-10/11");
    let r = send(
        &w.app,
        "GET",
        &uri,
        &w.rita,
        Some(("range", "bytes=50-")),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(r.headers["content-range"], "bytes */11");

    let r = send(
        &w.app,
        "GET",
        "/v1/repos/alice/game/objects/not-an-id",
        &w.rita,
        None,
        None,
    )
    .await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::BAD_REQUEST, "invalid_request")
    );

    let eve = sign_up(&w.app, "eve").await;
    let r = send(
        &w.app,
        "POST",
        "/v1/repos",
        &eve,
        None,
        Some(json!({"name": "mine"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED);
    let r = send(
        &w.app,
        "GET",
        &format!("/v1/repos/eve/mine/objects/{}", a.to_hex()),
        &eve,
        None,
        None,
    )
    .await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::NOT_FOUND, "object_not_found")
    );
    let r = send(&w.app, "GET", &uri, &eve, None, None).await;
    assert_eq!(
        r.status,
        StatusCode::NOT_FOUND,
        "a private repository does not exist to eve"
    );
}
