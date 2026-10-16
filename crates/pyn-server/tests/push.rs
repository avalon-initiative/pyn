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
    build_with(files, parent.into_iter().collect(), message, n)
}

fn build_with(
    files: &BTreeMap<&str, ObjectId>,
    parents: Vec<ObjectId>,
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
        parents,
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

const A_EXCLUSIVE: &str = "[meta]\ndefault = \"shared\"\n[exclusive]\npaths = [\"Source/a.txt\"]\n";

async fn checkout(w: &World, token: &str, path: &str, base: serde_json::Value) -> StatusCode {
    send(
        &w.app,
        "POST",
        "/v1/repos/alice/game/checkout",
        token,
        None,
        Some(json!({"path": path, "base_revision": base})),
    )
    .await
    .status
}

fn branch_body(branch: &str, body: serde_json::Value) -> serde_json::Value {
    let mut body = body;
    body["branch"] = json!(branch);
    body
}

#[tokio::test]
async fn the_policy_file_cannot_be_deleted_by_a_push_to_the_default_branch() {
    let w = world().await;
    let a = upload(&w, "alpha").await;
    let policy = upload(&w, A_EXCLUSIVE).await;
    let files = BTreeMap::from([(".pyn/pyn.toml", policy), ("Source/b.txt", a)]);
    let with = build(&files, None, "policy", 1);
    assert_eq!(
        push(&w, &w.alice, push_body(&[&with])).await.status,
        StatusCode::OK
    );
    assert_eq!(
        checkout(&w, &w.alice, ".pyn/pyn.toml", json!(1)).await,
        StatusCode::OK
    );
    let without = build(
        &BTreeMap::from([("Source/b.txt", a)]),
        Some(with.id),
        "drop",
        2,
    );
    let r = push(&w, &w.alice, push_body(&[&without])).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "push_unsupported")
    );
    assert_eq!(commit_count(&w).await, 1);
}

#[tokio::test]
async fn a_push_deletes_a_shared_path() {
    let w = world().await;
    let (a, b) = (upload(&w, "alpha").await, upload(&w, "beta").await);
    let both = BTreeMap::from([("Source/a.txt", a), ("Source/dir/b.txt", b)]);
    let first = build(&both, None, "first", 1);
    assert_eq!(
        push(&w, &w.bob, push_body(&[&first])).await.status,
        StatusCode::OK
    );

    let only_a = BTreeMap::from([("Source/a.txt", a)]);
    let gone = build(&only_a, Some(first.id), "remove dir", 2);
    let r = push(&w, &w.bob, push_body(&[&gone])).await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let done: api::PushResponse = r.json();
    assert_eq!(done.revisions.len(), 1);
    let rev = &done.revisions[0];
    assert!(rev.deleted && rev.path == "Source/dir/b.txt");
    assert_eq!((rev.id, rev.base), (2, Some(1)));
    assert!(done.stranded.is_empty());

    let r = send(
        &w.app,
        "GET",
        "/v1/repos/alice/game/snapshot",
        &w.alice,
        None,
        None,
    )
    .await;
    let snap: api::SnapshotPage = r.json();
    let paths: Vec<_> = snap.entries.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(paths, ["Source/a.txt"]);

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
    assert!(
        page.entries
            .iter()
            .any(|e| e.action == "delete" && e.detail.contains("(push)"))
    );
}

#[tokio::test]
async fn deleting_an_exclusive_path_in_a_push_needs_the_pushers_live_lock() {
    let w = world().await;
    let (m, a) = (upload(&w, "map").await, upload(&w, "alpha").await);
    let both = BTreeMap::from([("Content/x.umap", m), ("Source/a.txt", a)]);
    assert_eq!(
        checkout(&w, &w.bob, "Content/x.umap", json!(null)).await,
        StatusCode::OK
    );
    let first = build(&both, None, "first", 1);
    assert_eq!(
        push(&w, &w.bob, push_body(&[&first])).await.status,
        StatusCode::OK
    );
    let gone = build(
        &BTreeMap::from([("Source/a.txt", a)]),
        Some(first.id),
        "delete the map",
        2,
    );

    let r = push(&w, &w.bob, push_body(&[&gone])).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::CONFLICT, "lock_required")
    );
    assert_eq!(
        checkout(&w, &w.alice, "Content/x.umap", json!(1)).await,
        StatusCode::OK
    );
    let r = push(&w, &w.bob, push_body(&[&gone])).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::CONFLICT, "lock_held")
    );
    assert_eq!(commit_count(&w).await, 1);

    send(
        &w.app,
        "POST",
        "/v1/repos/alice/game/release",
        &w.alice,
        None,
        Some(json!({"path": "Content/x.umap"})),
    )
    .await;
    assert_eq!(
        checkout(&w, &w.bob, "Content/x.umap", json!(1)).await,
        StatusCode::OK
    );
    let r = push(&w, &w.bob, push_body(&[&gone])).await;
    assert_eq!(r.status, StatusCode::OK);
    let done: api::PushResponse = r.json();
    assert!(done.revisions[0].deleted);
    assert_eq!(commit_count(&w).await, 2);
}

#[tokio::test]
async fn a_policy_commit_on_the_default_branch_is_applied_and_an_invalid_one_refused_whole() {
    let w = world().await;
    let a = upload(&w, "alpha").await;
    let b = upload(&w, "beta").await;
    let base = build(&BTreeMap::from([("Source/a.txt", a)]), None, "base", 1);
    assert_eq!(
        push(&w, &w.bob, push_body(&[&base])).await.status,
        StatusCode::OK
    );

    let bad = upload(&w, "this is not [valid toml").await;
    let files = BTreeMap::from([
        ("Source/a.txt", a),
        ("Source/b.txt", b),
        (".pyn/pyn.toml", bad),
    ]);
    let invalid = build(&files, Some(base.id), "bad policy", 2);
    let r = push(&w, &w.alice, push_body(&[&invalid])).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::BAD_REQUEST, "invalid_rules")
    );
    assert_eq!(commit_count(&w).await, 1);

    let policy = upload(&w, A_EXCLUSIVE).await;
    let files = BTreeMap::from([
        ("Source/a.txt", a),
        ("Source/b.txt", b),
        (".pyn/pyn.toml", policy),
    ]);
    let good = build(&files, Some(base.id), "policy", 3);
    let r = push(&w, &w.bob, push_body(&[&good])).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::FORBIDDEN, "forbidden"),
        "a writer cannot change the policy"
    );
    let r = push(&w, &w.alice, push_body(&[&good])).await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let done: api::PushResponse = r.json();
    assert_eq!(done.revisions.len(), 2);

    // The new policy is in force: a.txt now needs a lock.
    let edit = upload(&w, "alpha 2").await;
    let files = BTreeMap::from([
        ("Source/a.txt", edit),
        ("Source/b.txt", b),
        (".pyn/pyn.toml", policy),
    ]);
    let r = send(
        &w.app,
        "GET",
        "/v1/repos/alice/game/snapshot",
        &w.alice,
        None,
        None,
    )
    .await;
    let head = r.json::<api::SnapshotPage>().commit.unwrap();
    let after = build(
        &files,
        Some(ObjectId::from_hex(REPO_ALGORITHM, &head).unwrap()),
        "edit a",
        4,
    );
    let r = push(&w, &w.bob, push_body(&[&after])).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::CONFLICT, "lock_required")
    );
}

#[tokio::test]
async fn a_policy_push_reports_the_versions_it_strands() {
    let w = world().await;
    let a = upload(&w, "alpha").await;
    let base = build(&BTreeMap::from([("Source/a.txt", a)]), None, "base", 1);
    assert_eq!(
        push(&w, &w.bob, push_body(&[&base])).await.status,
        StatusCode::OK
    );
    let r = send(
        &w.app,
        "POST",
        "/v1/repos/alice/game/branches",
        &w.bob,
        None,
        Some(json!({"name": "feature"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    let edit = upload(&w, "alpha on feature").await;
    let feature = build(
        &BTreeMap::from([("Source/a.txt", edit)]),
        Some(base.id),
        "feature edit",
        2,
    );
    let r = push(&w, &w.bob, branch_body("feature", push_body(&[&feature]))).await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&r.body)
    );

    let policy = upload(&w, A_EXCLUSIVE).await;
    let files = BTreeMap::from([("Source/a.txt", a), (".pyn/pyn.toml", policy)]);
    let commit = build(&files, Some(base.id), "policy", 3);
    let r = push(&w, &w.alice, push_body(&[&commit])).await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let done: api::PushResponse = r.json();
    assert_eq!(done.stranded.len(), 1);
    let v = &done.stranded[0];
    assert_eq!(
        (v.path.as_str(), v.branch.as_str(), v.author.as_str()),
        ("Source/a.txt", "feature", "bob")
    );
    assert!(v.conversion.is_some());
    assert_eq!(v.commit, feature.id.to_hex());
}

#[tokio::test]
async fn a_policy_file_pushed_to_another_branch_is_just_a_file() {
    let w = world().await;
    let a = upload(&w, "alpha").await;
    let base = build(&BTreeMap::from([("Source/a.txt", a)]), None, "base", 1);
    assert_eq!(
        push(&w, &w.bob, push_body(&[&base])).await.status,
        StatusCode::OK
    );
    let r = send(
        &w.app,
        "POST",
        "/v1/repos/alice/game/branches",
        &w.bob,
        None,
        Some(json!({"name": "feature"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);

    let junk = upload(&w, "this is not [valid toml").await;
    let files = BTreeMap::from([("Source/a.txt", a), (".pyn/pyn.toml", junk)]);
    let commit = build(&files, Some(base.id), "junk policy", 2);
    assert_eq!(
        checkout_on(&w, &w.bob, "feature", ".pyn/pyn.toml").await,
        StatusCode::OK
    );
    let r = push(&w, &w.bob, branch_body("feature", push_body(&[&commit]))).await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let done: api::PushResponse = r.json();
    assert_eq!(done.head, commit.id.to_hex());
    assert!(done.stranded.is_empty());
}

async fn checkout_on(w: &World, token: &str, branch: &str, path: &str) -> StatusCode {
    send(
        &w.app,
        "POST",
        "/v1/repos/alice/game/checkout",
        token,
        None,
        Some(json!({"path": path, "base_revision": null, "branch": branch})),
    )
    .await
    .status
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

async fn fetch(w: &World, token: &str, body: serde_json::Value) -> Reply {
    send(
        &w.app,
        "POST",
        "/v1/repos/alice/game/fetch",
        token,
        None,
        Some(body),
    )
    .await
}

fn fetched(r: &Reply) -> api::FetchResponse {
    assert_eq!(
        r.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    r.json()
}

fn fetched_ids(page: &api::FetchResponse) -> Vec<String> {
    page.commits.iter().map(|c| c.id.clone()).collect()
}

/// Two commits over nested folders, pushed by bob.
async fn two_commits(w: &World) -> (Built, Built) {
    let (a, b, c) = (
        upload(w, "alpha").await,
        upload(w, "beta").await,
        upload(w, "gamma").await,
    );
    let first = build(
        &BTreeMap::from([("Source/a.txt", a), ("Docs/b.txt", b)]),
        None,
        "first",
        1,
    );
    let second = build(
        &BTreeMap::from([("Source/a.txt", c), ("Docs/b.txt", b)]),
        Some(first.id),
        "second",
        2,
    );
    assert_eq!(
        push(w, &w.bob, push_body(&[&first, &second])).await.status,
        StatusCode::OK
    );
    (first, second)
}

#[tokio::test]
async fn a_fetch_lists_commits_with_their_pointers_and_trees_and_honours_haves() {
    let w = world().await;
    let (first, second) = two_commits(&w).await;

    let page = fetched(&fetch(&w, &w.rita, json!({})).await);
    assert_eq!(fetched_ids(&page), [first.id.to_hex(), second.id.to_hex()]);
    assert_eq!(page.next, None);
    assert_eq!(
        (page.refs[0].branch.as_str(), page.refs[0].head.clone()),
        ("main", Some(second.id.to_hex()))
    );
    let bytes = STANDARD.decode(&page.commits[1].data).unwrap();
    assert_eq!(bytes, second.raw);
    let tree_ids: Vec<_> = page.commits.iter().map(|c| c.trees.len()).collect();
    assert_eq!(tree_ids, [3, 2]);
    for t in page.commits.iter().flat_map(|c| &c.trees) {
        let body = STANDARD.decode(&t.data).unwrap();
        let id = hash_object(Algorithm::Sha256, Kind::Tree, &body).unwrap();
        assert_eq!(id.to_hex(), t.id);
    }
    let pointer = &page.commits[1].pointers;
    assert_eq!(
        pointer
            .iter()
            .map(|r| (r.path.as_str(), r.id))
            .collect::<Vec<_>>(),
        [("Source/a.txt", 2)]
    );

    let stray = "0".repeat(64);
    let page = fetched(
        &fetch(
            &w,
            &w.rita,
            json!({"branches": ["main"], "haves": [first.id.to_hex(), stray]}),
        )
        .await,
    );
    assert_eq!(fetched_ids(&page), [second.id.to_hex()]);
    let page = fetched(&fetch(&w, &w.rita, json!({"haves": [second.id.to_hex()]})).await);
    assert!(page.commits.is_empty());
}

#[tokio::test]
async fn a_fetch_pages_with_an_opaque_cursor_and_a_server_cap() {
    let w = world().await;
    let (first, second) = two_commits(&w).await;
    let one = fetched(&fetch(&w, &w.rita, json!({"limit": 1})).await);
    assert_eq!(fetched_ids(&one), [first.id.to_hex()]);
    let cursor = one.next.expect("more to come");
    let rest = fetched(&fetch(&w, &w.rita, json!({"cursor": cursor, "limit": 1})).await);
    assert_eq!(fetched_ids(&rest), [second.id.to_hex()]);
    assert_eq!(rest.next, None);

    let r = fetch(&w, &w.rita, json!({"cursor": "garbage"})).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::BAD_REQUEST, "invalid_request")
    );
    let page = fetched(&fetch(&w, &w.rita, json!({"limit": 1_000_000})).await);
    assert_eq!(page.commits.len(), 2);
    let r = fetch(&w, &w.rita, json!({"limit": 0})).await;
    assert_eq!(fetched(&r).commits.len(), 1, "a zero limit still pages");
}

#[tokio::test]
async fn a_fetch_carries_the_pointers_a_merge_commit_adopted() {
    let w = world().await;
    let c1 = upload(&w, "v1").await;
    let lock = |token: String, base: serde_json::Value| {
        let app = w.app.clone();
        async move {
            send(
                &app,
                "POST",
                "/v1/repos/alice/game/checkout",
                &token,
                None,
                Some(json!({"path": "Content/x.umap", "base_revision": base})),
            )
            .await
        }
    };
    assert_eq!(
        lock(w.bob.clone(), json!(null)).await.status,
        StatusCode::OK
    );
    let base = build(&BTreeMap::from([("Content/x.umap", c1)]), None, "v1", 1);
    assert_eq!(
        push(&w, &w.bob, push_body(&[&base])).await.status,
        StatusCode::OK
    );
    let r = send(
        &w.app,
        "POST",
        "/v1/repos/alice/game/branches",
        &w.alice,
        None,
        Some(json!({"name": "feature"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(lock(w.bob.clone(), json!(1)).await.status, StatusCode::OK);
    let c2 = upload(&w, "v2").await;
    let side = build(
        &BTreeMap::from([("Content/x.umap", c2)]),
        Some(base.id),
        "v2",
        2,
    );
    let mut body = push_body(&[&side]);
    body["branch"] = json!("feature");
    assert_eq!(push(&w, &w.bob, body).await.status, StatusCode::OK);
    let other = upload(&w, "other").await;
    let main2 = build(
        &BTreeMap::from([("Content/x.umap", c1), ("Source/o.txt", other)]),
        Some(base.id),
        "other",
        3,
    );
    assert_eq!(
        push(&w, &w.bob, push_body(&[&main2])).await.status,
        StatusCode::OK
    );
    let r = send(
        &w.app,
        "POST",
        "/v1/repos/alice/game/merge",
        &w.alice,
        None,
        Some(json!({"source": "feature", "target": "main"})),
    )
    .await;
    let merged: api::MergeReport = r.json();
    let merge_id = merged.commit.unwrap();

    let page = fetched(&fetch(&w, &w.rita, json!({"branches": ["main"]})).await);
    assert_eq!(page.commits.len(), 4);
    let last = page.commits.last().unwrap();
    assert_eq!(last.id, merge_id);
    let at = |id: &str| page.commits.iter().position(|c| c.id == id).unwrap();
    assert!(at(&side.id.to_hex()) < at(&merge_id) && at(&main2.id.to_hex()) < at(&merge_id));
    assert_eq!(
        last.pointers
            .iter()
            .map(|r| (r.path.as_str(), r.id))
            .collect::<Vec<_>>(),
        [("Content/x.umap", 2)]
    );

    let page = fetched(
        &fetch(
            &w,
            &w.rita,
            json!({"branches": ["main"], "haves": [side.id.to_hex()]}),
        )
        .await,
    );
    assert_eq!(page.commits.len(), 2, "main's own commit and the merge");
}

#[tokio::test]
async fn a_fetch_of_an_unknown_branch_is_branch_not_found() {
    let w = world().await;
    let r = fetch(&w, &w.rita, json!({"branches": ["nope"]})).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::NOT_FOUND, "branch_not_found")
    );
    let r = fetch(&w, &w.rita, json!({"haves": ["not-an-id"]})).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::BAD_REQUEST, "invalid_request")
    );
}

#[tokio::test]
async fn a_fetch_follows_the_repositorys_read_access() {
    let w = world().await;
    let (first, _) = two_commits(&w).await;
    let anonymous = || {
        Request::post("/v1/repos/alice/game/fetch")
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap()
    };
    let r = w.app.clone().oneshot(anonymous()).await.unwrap();
    assert_eq!(
        r.status(),
        StatusCode::NOT_FOUND,
        "a private repository does not exist to a stranger"
    );

    let eve = sign_up(&w.app, "eve").await;
    let r = fetch(&w, &eve, json!({})).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    let r = send(
        &w.app,
        "PATCH",
        "/v1/repos/alice/game",
        &w.alice,
        None,
        Some(json!({"visibility": "public"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    let r = w.app.clone().oneshot(anonymous()).await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let bytes = r.into_body().collect().await.unwrap().to_bytes();
    let page: api::FetchResponse = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(page.commits[0].id, first.id.to_hex());
    assert_eq!(fetched(&fetch(&w, &eve, json!({})).await).commits.len(), 2);
}

async fn lock(w: &World, token: &str, path: &str, base: serde_json::Value) -> StatusCode {
    checkout(w, token, path, base).await
}

#[tokio::test]
async fn a_rejected_push_is_resolved_by_pushing_a_merge_commit() {
    let w = world().await;
    let (a0, b0, x0) = (
        upload(&w, "a0").await,
        upload(&w, "b0").await,
        upload(&w, "x0").await,
    );
    let all = BTreeMap::from([
        ("Source/a.txt", a0),
        ("Source/b.txt", b0),
        ("Content/x.umap", x0),
    ]);
    let base = build(&all, None, "base", 1);
    assert_eq!(
        lock(&w, &w.alice, "Content/x.umap", json!(null)).await,
        StatusCode::OK
    );
    assert_eq!(
        push(&w, &w.alice, push_body(&[&base])).await.status,
        StatusCode::OK
    );

    let (b1, x1) = (upload(&w, "b1").await, upload(&w, "x1").await);
    let theirs = BTreeMap::from([
        ("Source/a.txt", a0),
        ("Source/b.txt", b1),
        ("Content/x.umap", x1),
    ]);
    let server = build(&theirs, Some(base.id), "server", 2);
    assert_eq!(
        lock(&w, &w.bob, "Content/x.umap", json!(1)).await,
        StatusCode::OK
    );
    assert_eq!(
        push(&w, &w.bob, push_body(&[&server])).await.status,
        StatusCode::OK
    );

    let a1 = upload(&w, "a1").await;
    let ours = BTreeMap::from([
        ("Source/a.txt", a1),
        ("Source/b.txt", b0),
        ("Content/x.umap", x0),
    ]);
    let local = build(&ours, Some(base.id), "local", 3);
    let r = push(&w, &w.alice, push_body(&[&local])).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::CONFLICT, "not_fast_forward")
    );

    let merged = BTreeMap::from([
        ("Source/a.txt", a1),
        ("Source/b.txt", b1),
        ("Content/x.umap", x1),
    ]);
    let merge = build_with(&merged, vec![local.id, server.id], "Merge", 4);
    let r = push(&w, &w.alice, push_body(&[&local, &merge])).await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let done: api::PushResponse = r.json();
    assert_eq!(done.head, merge.id.to_hex());
    assert_eq!(commit_count(&w).await, 4);

    let page = fetched(&fetch(&w, &w.rita, json!({"branches": ["main"]})).await);
    let last = page.commits.last().unwrap();
    assert_eq!(last.id, merge.id.to_hex());
    assert_eq!(
        last.pointers
            .iter()
            .map(|r| (r.path.as_str(), r.id))
            .collect::<Vec<_>>(),
        [("Content/x.umap", 2), ("Source/b.txt", 2)]
    );
}

#[tokio::test]
async fn a_pushed_merge_of_diverged_exclusive_pointers_needs_a_resolution_under_the_lock() {
    let w = world().await;
    let (a0, x0) = (upload(&w, "a0").await, upload(&w, "x0").await);
    let all = BTreeMap::from([("Source/a.txt", a0), ("Content/x.umap", x0)]);
    let base = build(&all, None, "base", 1);
    assert_eq!(
        lock(&w, &w.alice, "Content/x.umap", json!(null)).await,
        StatusCode::OK
    );
    assert_eq!(
        push(&w, &w.alice, push_body(&[&base])).await.status,
        StatusCode::OK
    );
    let r = send(
        &w.app,
        "POST",
        "/v1/repos/alice/game/branches",
        &w.alice,
        None,
        Some(json!({"name": "feature"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);

    let x_feature = upload(&w, "x feature").await;
    let side = build(
        &BTreeMap::from([("Source/a.txt", a0), ("Content/x.umap", x_feature)]),
        Some(base.id),
        "feature x",
        2,
    );
    assert_eq!(
        lock(&w, &w.bob, "Content/x.umap", json!(1)).await,
        StatusCode::OK
    );
    assert_eq!(
        push(&w, &w.bob, branch_body("feature", push_body(&[&side])))
            .await
            .status,
        StatusCode::OK
    );
    let x_main = upload(&w, "x main").await;
    let main = build(
        &BTreeMap::from([("Source/a.txt", a0), ("Content/x.umap", x_main)]),
        Some(base.id),
        "main x",
        3,
    );
    assert_eq!(
        lock(&w, &w.alice, "Content/x.umap", json!(1)).await,
        StatusCode::OK
    );
    assert_eq!(
        push(&w, &w.alice, push_body(&[&main])).await.status,
        StatusCode::OK
    );

    let keep = build_with(
        &BTreeMap::from([("Source/a.txt", a0), ("Content/x.umap", x_main)]),
        vec![main.id, side.id],
        "keep main's",
        4,
    );
    let r = push(&w, &w.alice, push_body(&[&keep])).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::CONFLICT, "merge_conflict")
    );
    assert_eq!(commit_count(&w).await, 2);

    let chosen = upload(&w, "x chosen").await;
    let resolved = build_with(
        &BTreeMap::from([("Source/a.txt", a0), ("Content/x.umap", chosen)]),
        vec![main.id, side.id],
        "resolve",
        5,
    );
    let r = push(&w, &w.alice, push_body(&[&resolved])).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::CONFLICT, "lock_required")
    );

    assert_eq!(
        lock(&w, &w.alice, "Content/x.umap", json!(3)).await,
        StatusCode::OK
    );
    let r = push(&w, &w.alice, push_body(&[&resolved])).await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let done: api::PushResponse = r.json();
    assert_eq!(done.revisions.len(), 1);
    assert_eq!((done.revisions[0].id, done.revisions[0].base), (4, Some(3)));
}

#[tokio::test]
async fn a_merge_commit_naming_an_unknown_parent_or_three_parents_is_refused() {
    let w = world().await;
    let a0 = upload(&w, "a0").await;
    let files = BTreeMap::from([("Source/a.txt", a0)]);
    let base = build(&files, None, "base", 1);
    assert_eq!(
        push(&w, &w.bob, push_body(&[&base])).await.status,
        StatusCode::OK
    );
    let a1 = upload(&w, "a1").await;
    let next = BTreeMap::from([("Source/a.txt", a1)]);
    let stranger = build(&files, None, "elsewhere", 9);
    let lost = build_with(&next, vec![base.id, stranger.id], "lost", 2);
    let r = push(&w, &w.bob, push_body(&[&lost])).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::BAD_REQUEST, "push_invalid")
    );
    let octopus = build_with(&next, vec![base.id, stranger.id, lost.id], "octopus", 3);
    let r = push(&w, &w.bob, push_body(&[&octopus])).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, "push_unsupported")
    );
    assert_eq!(commit_count(&w).await, 1);
}
