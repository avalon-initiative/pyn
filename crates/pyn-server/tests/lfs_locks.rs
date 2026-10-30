//! The Git LFS Locks API over pyn locks (docs/concepts/lfs.md).

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine;
use http_body_util::BodyExt;
use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore,
};
use pyn_core::{
    AccessConfig, AccessService, Credential, Identity, MetadataStore, RegistrationMode, RepoId,
    RepoRecord, RepoSettings, Repositories, Role, Rules, SystemClock, UserId, Visibility,
};
use pyn_server::auth::BearerAuth;
use pyn_server::{AppState, router};
use serde_json::{Value, json};
use tower::ServiceExt;

const RULES: &str = "[meta]\ndefault = \"shared\"\n[exclusive]\npaths = [\"Content/\"]\n";
const REPO_ID: &str = "t";
const BASE: &str = "/v1/repos/owner/game/info/lfs";
const LFS: &str = "application/vnd.git-lfs+json";

struct World {
    app: axum::Router,
    wendy: String,
    wanda: String,
    maya: String,
    ruth: String,
}

fn session(user: &str) -> Identity {
    Identity {
        user: UserId::new(user),
        credential: Credential::Session,
    }
}

async fn member(st: &AppState, name: &str, role: Role) -> String {
    let root = st
        .access
        .principal(&RepoId::new(REPO_ID), &UserId::new("root"))
        .await
        .unwrap();
    st.access
        .set_user_role(&root, &RepoId::new(REPO_ID), &UserId::new(name), role)
        .await
        .unwrap();
    let who = st
        .access
        .principal(&RepoId::new(REPO_ID), &UserId::new(name))
        .await
        .unwrap();
    st.access
        .create_token(
            &session(name),
            name,
            who.permissions.clone(),
            vec![RepoId::new(REPO_ID)],
            None,
        )
        .await
        .unwrap()
        .1
}

async fn world() -> World {
    let clock = Arc::new(SystemClock);
    let objects = Arc::new(pyn_core::ChunkedStore::new(Arc::new(
        MemoryObjectStore::new(),
    )));
    let audit = Arc::new(MemoryAuditStore::new());
    let meta = Arc::new(MemoryMetadataStore::new());
    meta.create_repo(RepoRecord {
        id: RepoId::new(REPO_ID),
        owner: UserId::new("owner"),
        name: "game".into(),
        visibility: Visibility::Private,
        settings: RepoSettings::default(),
        storage_cap: None,
        created_at: chrono::Utc::now(),
    })
    .await
    .unwrap();
    let access = Arc::new(
        AccessService::new(
            Arc::new(MemoryAccessStore::new()),
            clock.clone(),
            Arc::new(pyn_core::MemoryLimitEvents::new()),
        )
        .with_config(AccessConfig {
            registration: RegistrationMode::InviteOnly,
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
    let state = AppState {
        repos,
        objects,
        access: access.clone(),
        auth: Arc::new(BearerAuth { access }),
        dev_auth: None,
        trust_forwarded: true,
    };
    let root = UserId::new("root");
    state.access.admin_for_tests(&root).await.unwrap();
    state
        .access
        .add_creator(&RepoId::new(REPO_ID), &root)
        .await
        .unwrap();
    World {
        wendy: member(&state, "wendy", Role::Writer).await,
        wanda: member(&state, "wanda", Role::Writer).await,
        maya: member(&state, "maya", Role::Maintainer).await,
        ruth: member(&state, "ruth", Role::Reader).await,
        app: router(state.clone()),
    }
}

fn req(method: &str, uri: &str, token: &str, body: Option<Value>) -> Request<Body> {
    let b = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {token}"))
        .header("accept", LFS);
    match body {
        Some(v) => b
            .header("content-type", LFS)
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => b.body(Body::empty()).unwrap(),
    }
}

async fn call(w: &World, r: Request<Body>) -> (StatusCode, Value) {
    let res = w.app.clone().oneshot(r).await.unwrap();
    let status = res.status();
    assert_eq!(
        res.headers().get("content-type").unwrap(),
        LFS,
        "every LFS response uses the LFS media type"
    );
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

async fn lock(w: &World, token: &str, path: &str) -> (StatusCode, Value) {
    call(
        w,
        req(
            "POST",
            &format!("{BASE}/locks"),
            token,
            Some(json!({"path": path})),
        ),
    )
    .await
}

#[tokio::test]
async fn creating_a_lock_returns_the_lfs_shape_and_is_a_pyn_lock() {
    let w = world().await;
    let (status, body) = lock(&w, &w.wendy, "Content/a.umap").await;
    assert_eq!(status, StatusCode::CREATED);
    let l = &body["lock"];
    assert_eq!(l["path"], "Content/a.umap");
    assert_eq!(l["owner"]["name"], "wendy");
    assert!(!l["id"].as_str().unwrap().is_empty());
    assert!(l["locked_at"].as_str().unwrap().ends_with('Z'));

    let pyn = w
        .app
        .clone()
        .oneshot(req("GET", "/v1/repos/owner/game/locks", &w.wendy, None))
        .await
        .unwrap();
    let locks: Value =
        serde_json::from_slice(&pyn.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(locks[0]["path"], "Content/a.umap");
    assert_eq!(locks[0]["owner"], "wendy");
}

#[tokio::test]
async fn a_held_path_is_a_409_carrying_the_lock() {
    let w = world().await;
    let (_, first) = lock(&w, &w.wendy, "Content/a.umap").await;
    for token in [&w.wanda, &w.wendy] {
        let (status, body) = lock(&w, token, "Content/a.umap").await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["lock"]["id"], first["lock"]["id"]);
        assert_eq!(body["lock"]["owner"]["name"], "wendy");
        assert!(body["message"].as_str().unwrap().contains("wendy"));
    }
}

#[tokio::test]
async fn a_pyn_checkout_is_visible_to_lfs_and_the_reverse() {
    let w = world().await;
    let checkout = json!({"path": "Content/b.umap", "base_revision": null});
    let r = w
        .app
        .clone()
        .oneshot(
            Request::post("/v1/repos/owner/game/checkout")
                .header("authorization", format!("Bearer {}", w.wendy))
                .header("content-type", "application/json")
                .body(Body::from(checkout.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let (status, body) = lock(&w, &w.wanda, "Content/b.umap").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["lock"]["owner"]["name"], "wendy");
    let (_, listed) = call(&w, req("GET", &format!("{BASE}/locks"), &w.wanda, None)).await;
    assert_eq!(listed["locks"][0]["path"], "Content/b.umap");
}

#[tokio::test]
async fn a_non_exclusive_path_gets_an_advisory_lock() {
    let w = world().await;
    let (status, body) = lock(&w, &w.wendy, "docs/readme.txt").await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["lock"]["path"], "docs/readme.txt");
    let (status, body) = lock(&w, &w.wanda, "docs/readme.txt").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["lock"]["owner"]["name"], "wendy");

    let id = body["lock"]["id"].as_str().unwrap();
    let (status, _) = call(
        &w,
        req(
            "POST",
            &format!("{BASE}/locks/{id}/unlock"),
            &w.wendy,
            Some(json!({})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        lock(&w, &w.wanda, "docs/readme.txt").await.0,
        StatusCode::CREATED
    );
}

#[tokio::test]
async fn list_filters_by_path_and_id_and_pages_with_a_cursor() {
    let w = world().await;
    for p in [
        "Content/e",
        "Content/b",
        "Content/d",
        "Content/a",
        "Content/c",
    ] {
        assert_eq!(lock(&w, &w.wendy, p).await.0, StatusCode::CREATED);
    }
    let (_, one) = call(
        &w,
        req(
            "GET",
            &format!("{BASE}/locks?path=Content/c"),
            &w.ruth,
            None,
        ),
    )
    .await;
    assert_eq!(one["locks"].as_array().unwrap().len(), 1);
    assert!(one.get("next_cursor").is_none());
    let id = one["locks"][0]["id"].as_str().unwrap();
    let (_, by_id) = call(
        &w,
        req("GET", &format!("{BASE}/locks?id={id}"), &w.ruth, None),
    )
    .await;
    assert_eq!(by_id["locks"][0]["path"], "Content/c");

    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    let mut pages = 0;
    loop {
        let q = cursor
            .as_ref()
            .map_or(String::new(), |c| format!("&cursor={c}"));
        let (status, page) = call(
            &w,
            req("GET", &format!("{BASE}/locks?limit=2{q}"), &w.ruth, None),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        pages += 1;
        for l in page["locks"].as_array().unwrap() {
            seen.push(l["path"].as_str().unwrap().to_string());
        }
        match page["next_cursor"].as_str() {
            Some(c) => cursor = Some(c.to_string()),
            None => break,
        }
    }
    assert_eq!(pages, 3);
    assert_eq!(
        seen,
        [
            "Content/a",
            "Content/b",
            "Content/c",
            "Content/d",
            "Content/e"
        ]
    );

    let (status, err) = call(
        &w,
        req("GET", &format!("{BASE}/locks?cursor=***"), &w.ruth, None),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(err["message"].is_string());
}

#[tokio::test]
async fn verify_splits_ours_from_theirs_and_pages() {
    let w = world().await;
    lock(&w, &w.wendy, "Content/a").await;
    lock(&w, &w.wanda, "Content/b").await;
    lock(&w, &w.wendy, "Content/c").await;
    let (status, v) = call(
        &w,
        req(
            "POST",
            &format!("{BASE}/locks/verify"),
            &w.wendy,
            Some(json!({"ref": {"name": "refs/heads/main"}})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let paths = |k: &str| -> Vec<String> {
        v[k].as_array()
            .unwrap()
            .iter()
            .map(|l| l["path"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(paths("ours"), ["Content/a", "Content/c"]);
    assert_eq!(paths("theirs"), ["Content/b"]);

    let (_, first) = call(
        &w,
        req(
            "POST",
            &format!("{BASE}/locks/verify"),
            &w.wendy,
            Some(json!({"limit": 2})),
        ),
    )
    .await;
    assert_eq!(first["ours"].as_array().unwrap().len(), 1);
    assert_eq!(first["theirs"].as_array().unwrap().len(), 1);
    let cursor = first["next_cursor"].as_str().unwrap();
    let (_, rest) = call(
        &w,
        req(
            "POST",
            &format!("{BASE}/locks/verify"),
            &w.wendy,
            Some(json!({"limit": 2, "cursor": cursor})),
        ),
    )
    .await;
    assert_eq!(rest["ours"][0]["path"], "Content/c");
    assert!(rest.get("next_cursor").is_none());
}

#[tokio::test]
async fn unlock_needs_the_holder_or_a_forcing_permission_and_is_audited() {
    let w = world().await;
    let (_, made) = lock(&w, &w.wendy, "Content/a").await;
    let id = made["lock"]["id"].as_str().unwrap().to_string();
    let unlock = |token: &str, body: Value| {
        req(
            "POST",
            &format!("{BASE}/locks/{id}/unlock"),
            token,
            Some(body),
        )
    };

    let (status, err) = call(&w, unlock(&w.wanda, json!({}))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(err["message"].is_string());
    let (status, _) = call(&w, unlock(&w.wanda, json!({"force": true}))).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "a writer cannot force");
    let (status, _) = call(&w, unlock(&w.maya, json!({}))).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "force is explicit");

    let (status, body) = call(&w, unlock(&w.maya, json!({"force": true}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["lock"]["id"], made["lock"]["id"]);
    assert_eq!(body["lock"]["owner"]["name"], "wendy");
    let (status, _) = call(&w, unlock(&w.maya, json!({"force": true}))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    lock(&w, &w.wendy, "Content/a").await;
    let (_, again) = call(
        &w,
        req(
            "GET",
            &format!("{BASE}/locks?path=Content/a"),
            &w.wendy,
            None,
        ),
    )
    .await;
    let id2 = again["locks"][0]["id"].as_str().unwrap();
    let (status, _) = call(
        &w,
        req(
            "POST",
            &format!("{BASE}/locks/{id2}/unlock"),
            &w.wendy,
            Some(json!({})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let audit = w
        .app
        .clone()
        .oneshot(req("GET", "/v1/repos/owner/game/audit", &w.maya, None))
        .await
        .unwrap();
    let page: Value =
        serde_json::from_slice(&audit.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let actions: Vec<(&str, &str)> = page["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| (e["action"].as_str().unwrap(), e["actor"].as_str().unwrap()))
        .filter(|(action, _)| matches!(*action, "checkout" | "release" | "force_unlock"))
        .collect();
    assert_eq!(
        actions,
        [
            ("release", "wendy"),
            ("checkout", "wendy"),
            ("force_unlock", "maya"),
            ("checkout", "wendy"),
        ]
    );
}

#[tokio::test]
async fn credentials_basic_with_a_token_password_or_bearer() {
    let w = world().await;
    let basic = |user: &str, pw: &str| {
        let raw = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pw}"));
        Request::get(format!("{BASE}/locks"))
            .header("authorization", format!("Basic {raw}"))
            .body(Body::empty())
            .unwrap()
    };
    let res = w
        .app
        .clone()
        .oneshot(basic("wendy", &w.wendy))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let res = w.app.clone().oneshot(basic("wendy", "nope")).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert!(res.headers().contains_key("www-authenticate"));
    let res = w
        .app
        .clone()
        .oneshot(
            Request::get(format!("{BASE}/locks"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        res.headers().get("www-authenticate").unwrap(),
        "Basic realm=\"pyn\""
    );
    let body: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert!(body["message"].is_string());
}

#[tokio::test]
async fn permissions_malformed_bodies_and_the_git_suffix() {
    let w = world().await;
    let (status, _) = lock(&w, &w.ruth, "Content/a").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "a reader cannot lock");
    let (status, _) = call(&w, req("GET", &format!("{BASE}/locks"), &w.ruth, None)).await;
    assert_eq!(status, StatusCode::OK, "a reader can list");

    let bad = Request::post(format!("{BASE}/locks"))
        .header("authorization", format!("Bearer {}", w.wendy))
        .header("content-type", LFS)
        .body(Body::from("{not json"))
        .unwrap();
    assert_eq!(call(&w, bad).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = lock(&w, &w.wendy, "../escape").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, body) = call(
        &w,
        req(
            "POST",
            "/v1/repos/owner/game.git/info/lfs/locks",
            &w.wendy,
            Some(json!({"path": "Content/z"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["lock"]["path"], "Content/z");
    let (status, _) = call(
        &w,
        req(
            "GET",
            "/v1/repos/owner/missing/info/lfs/locks",
            &w.wendy,
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

fn git_lfs_available() -> bool {
    std::process::Command::new("git")
        .args(["lfs", "version"])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Stock `git lfs` against a live server; skipped when it is not installed.
#[tokio::test(flavor = "multi_thread")]
async fn stock_git_lfs_locks_and_unlocks() {
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
    let url = format!(
        "http://wendy:{}@{addr}/v1/repos/owner/game/info/lfs",
        w.wendy
    );
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir.path())
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned()
                + &String::from_utf8_lossy(&out.stderr),
        )
    };
    assert!(git(&["init", "-q"]).0);
    assert!(git(&["config", "lfs.url", &url]).0);
    assert!(git(&["config", "lfs.locksverify", "true"]).0);
    let (ok, out) = git(&["lfs", "lock", "Content/a.umap"]);
    assert!(ok, "{out}");
    let (ok, out) = git(&["lfs", "locks"]);
    assert!(
        ok && out.contains("Content/a.umap") && out.contains("wendy"),
        "{out}"
    );
    let (ok, out) = git(&["lfs", "unlock", "Content/a.umap"]);
    assert!(ok, "{out}");
    let (ok, out) = git(&["lfs", "locks"]);
    assert!(ok && !out.contains("Content/a.umap"), "{out}");
}
