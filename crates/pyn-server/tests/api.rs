use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore,
};
use pyn_core::{
    AccessConfig, AccessService, AuthProvider, Credential, Identity, MetadataStore,
    RegistrationMode, RepoId, RepoRecord, RepoSettings, Repositories, Rules, SystemClock, UserId,
    Visibility,
};
use pyn_proto as api;
use pyn_server::auth::{BearerAuth, DevHeaderAuth};
use pyn_server::{AppState, router};
use tower::ServiceExt;

const RULES: &str = "[meta]\ndefault = \"shared\"\n[exclusive]\npaths = [\"Content/\"]\n";

/// The repository every test works in, registered as `owner/game`.
const REPO_ID: &str = "t";
const REPO: &str = "owner/game";

async fn state(dev: bool) -> AppState {
    state_with(dev, RegistrationMode::InviteOnly).await
}

async fn state_with(dev: bool, mode: RegistrationMode) -> AppState {
    let clock = Arc::new(SystemClock);
    let objects = Arc::new(MemoryObjectStore::new());
    let audit = Arc::new(MemoryAuditStore::new());
    let meta = Arc::new(MemoryMetadataStore::new());
    meta.create_repo(RepoRecord {
        id: RepoId::new(REPO_ID),
        owner: UserId::new("owner"),
        name: "game".into(),
        visibility: Visibility::Private,
        settings: RepoSettings::default(),
        created_at: chrono::Utc::now(),
    })
    .await
    .unwrap();
    let access = Arc::new(
        AccessService::new(Arc::new(MemoryAccessStore::new()), clock.clone())
            .with_config(AccessConfig {
                registration: mode,
                ..AccessConfig::default()
            })
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
    AppState {
        repos,
        objects,
        access: access.clone(),
        auth: Arc::new(BearerAuth { access }),
        dev_auth: dev.then(|| Arc::new(DevHeaderAuth) as Arc<dyn AuthProvider>),
    }
}

fn session(user: &str) -> Identity {
    Identity {
        user: UserId::new(user),
        credential: Credential::Session,
    }
}

/// An app with development auth on, as `make run` uses it.
async fn app() -> axum::Router {
    router(state(true).await)
}

fn json_post(uri: &str, user: &str, body: serde_json::Value) -> Request<Body> {
    Request::post(uri)
        .header("content-type", "application/json")
        .header(api::DEV_USER_HEADER, user)
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[tokio::test]
async fn lock_conflict_is_a_409_with_a_stable_code() {
    let app = app().await;
    let body = serde_json::json!({"path": "Content/a.umap", "base_revision": null});

    let r = app
        .clone()
        .oneshot(json_post(
            "/v1/repos/owner/game/checkout",
            "alice",
            body.clone(),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);

    let r = app
        .oneshot(json_post("/v1/repos/owner/game/checkout", "bob", body))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::CONFLICT);
    let err: api::ErrorBody =
        serde_json::from_slice(&r.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(err.code, "lock_held");
    assert!(err.message.contains("locked by alice until"));
    assert!(err.message.contains("Ask alice to release it"));
}

#[tokio::test]
async fn unauthenticated_requests_are_rejected() {
    let r = app()
        .await
        .oneshot(
            Request::post("/v1/repos/owner/game/checkout")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"path":"Content/a.umap","base_revision":null}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(r.status().is_client_error());
}

#[tokio::test]
async fn openapi_document_lists_the_lock_routes() {
    let r = app()
        .await
        .oneshot(Request::get("/openapi.json").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let doc: serde_json::Value =
        serde_json::from_slice(&r.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert!(doc["paths"]["/v1/repos/{owner}/{name}/checkout"].is_object());
    assert!(doc["components"]["schemas"]["ErrorBody"].is_object());
}

#[tokio::test]
async fn files_listing_shows_mode_and_lock() {
    let app = app().await;
    let body = serde_json::json!({"path": "Content/a.umap", "base_revision": null});
    app.clone()
        .oneshot(json_post("/v1/repos/owner/game/checkout", "alice", body))
        .await
        .unwrap();

    let r = app
        .oneshot(
            Request::get("/v1/repos/owner/game/files")
                .header(api::DEV_USER_HEADER, "alice")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let page: api::FilePage =
        serde_json::from_slice(&r.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(page.entries.len(), 1);
    let entry = &page.entries[0];
    assert_eq!(
        (entry.path.as_str(), entry.mode, entry.revision),
        ("Content/a.umap", api::Mode::Exclusive, None)
    );
    assert_eq!(entry.lock.as_ref().unwrap().owner, "alice");
    assert!(page.next_after.is_none());
}

async fn get_as(app: &axum::Router, uri: &str, user: &str) -> (StatusCode, Vec<u8>) {
    let r = app
        .clone()
        .oneshot(
            Request::get(uri)
                .header(api::DEV_USER_HEADER, user)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = r.status();
    (
        status,
        r.into_body().collect().await.unwrap().to_bytes().to_vec(),
    )
}

#[tokio::test]
async fn history_without_a_path_is_repository_wide_and_validates_its_parameters() {
    let app = app().await;
    let (status, body) = get_as(&app, "/v1/repos/owner/game/history", "alice").await;
    assert_eq!(status, StatusCode::OK);
    let page: api::HistoryPage = serde_json::from_slice(&body).unwrap();
    assert!(page.revisions.is_empty() && page.next_cursor.is_none());

    let (status, body) = get_as(&app, "/v1/repos/owner/game/history?path=a.txt", "alice").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        serde_json::from_slice::<api::HistoryPage>(&body)
            .unwrap()
            .revisions
            .is_empty()
    );

    for uri in [
        "/v1/repos/owner/game/history?filter=%5B",
        "/v1/repos/owner/game/history?before=garbage",
        "/v1/repos/owner/game/history?path=a.txt&filter=*.ts",
        "/v1/repos/owner/game/history?path=a.txt&limit=5",
    ] {
        let (status, body) = get_as(&app, uri, "alice").await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
        let err: api::ErrorBody = serde_json::from_slice(&body).unwrap();
        assert_eq!(err.code, "invalid_request", "{uri}");
    }
}

#[tokio::test]
async fn tree_and_summary_describe_the_landing_page() {
    let app = app().await;
    let body = serde_json::json!({"path": "Content/a.umap", "base_revision": null});
    app.clone()
        .oneshot(json_post("/v1/repos/owner/game/checkout", "alice", body))
        .await
        .unwrap();
    let put = Request::put("/v1/repos/owner/game/objects")
        .header(api::DEV_USER_HEADER, "bob")
        .body(Body::from("x"))
        .unwrap();
    let r = app.clone().oneshot(put).await.unwrap();
    let obj: api::PutObjectResponse =
        serde_json::from_slice(&r.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let body = serde_json::json!({"path": "Source/a.cpp", "content": obj.content, "base_revision": null, "message": "add a"});
    app.clone()
        .oneshot(json_post("/v1/repos/owner/game/checkin", "bob", body))
        .await
        .unwrap();

    let (status, bytes) = get_as(&app, "/v1/repos/owner/game/tree", "alice").await;
    assert_eq!(status, StatusCode::OK);
    let root: api::TreeListing = serde_json::from_slice(&bytes).unwrap();
    let shape: Vec<_> = root
        .entries
        .iter()
        .map(|e| (e.name.as_str(), e.kind, e.mode))
        .collect();
    assert_eq!(
        shape,
        [
            (
                "Content",
                api::TreeEntryKind::Folder,
                api::TreeMode::Exclusive
            ),
            ("Source", api::TreeEntryKind::Folder, api::TreeMode::Shared),
        ]
    );
    let source = root.entries[1].last_change.as_ref().unwrap();
    assert_eq!(
        (
            source.path.as_str(),
            source.author.as_str(),
            source.message.as_str()
        ),
        ("Source/a.cpp", "bob", "add a")
    );

    let (status, bytes) = get_as(&app, "/v1/repos/owner/game/tree?path=Content/", "alice").await;
    assert_eq!(status, StatusCode::OK);
    let content: api::TreeListing = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(content.path, "Content");
    assert_eq!(content.entries[0].lock.as_ref().unwrap().owner, "alice");

    let (status, bytes) = get_as(&app, "/v1/repos/owner/game/tree?path=Nope", "alice").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let err: api::ErrorBody = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(err.code, "path_not_found");
    let (status, _) = get_as(&app, "/v1/repos/owner/game/tree?path=Source/a.cpp", "alice").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = get_as(&app, "/v1/repos/owner/game/tree?path=../x", "alice").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, bytes) = get_as(&app, "/v1/repos/owner/game/summary?activity=2", "alice").await;
    assert_eq!(status, StatusCode::OK);
    let sum: api::RepoSummary = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        (sum.default_branch.as_str(), sum.branch_count, sum.files),
        ("main", 1, 1)
    );
    assert_eq!(sum.locks.len(), 1);
    let acts: Vec<_> = sum
        .activity
        .iter()
        .map(|a| (a.actor.as_str(), a.action.as_str()))
        .collect();
    assert_eq!(acts, [("bob", "checkin"), ("alice", "checkout")]);

    let anon = app
        .oneshot(
            Request::get("/v1/repos/owner/game/tree")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(anon.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn any_revision_can_be_fetched() {
    let app = app().await;
    let mut base = serde_json::Value::Null;
    for text in ["v1", "v2"] {
        let put = Request::put("/v1/repos/owner/game/objects")
            .header(api::DEV_USER_HEADER, "bob")
            .body(Body::from(text))
            .unwrap();
        let r = app.clone().oneshot(put).await.unwrap();
        let obj: api::PutObjectResponse =
            serde_json::from_slice(&r.into_body().collect().await.unwrap().to_bytes()).unwrap();
        let body = serde_json::json!({"path": "Source/a.cpp", "content": obj.content, "base_revision": base, "message": text});
        let r = app
            .clone()
            .oneshot(json_post("/v1/repos/owner/game/checkin", "bob", body))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        base = serde_json::json!(base.as_u64().unwrap_or(0) + 1);
    }

    let fetch = |query: &'static str| {
        let app = app.clone();
        async move {
            app.oneshot(
                Request::get(format!("/v1/repos/owner/game/content?{query}"))
                    .header(api::DEV_USER_HEADER, "alice")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
        }
    };
    let r = fetch("path=Source/a.cpp&revision=1").await;
    assert_eq!(r.headers()[api::REVISION_HEADER], "1");
    assert_eq!(
        r.into_body().collect().await.unwrap().to_bytes().as_ref(),
        b"v1"
    );

    let r = fetch("path=Source/a.cpp").await;
    assert_eq!(r.headers()[api::REVISION_HEADER], "2");
    assert_eq!(
        r.into_body().collect().await.unwrap().to_bytes().as_ref(),
        b"v2"
    );

    let r = fetch("path=Source/a.cpp&revision=9").await;
    assert_eq!(r.status(), StatusCode::NOT_FOUND);
    let err: api::ErrorBody =
        serde_json::from_slice(&r.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(err.code, "revision_not_found");
}

async fn body_json<T: serde::de::DeserializeOwned>(r: axum::response::Response) -> T {
    serde_json::from_slice(&r.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

fn with_token(
    method: &str,
    uri: &str,
    token: &str,
    body: Option<serde_json::Value>,
) -> Request<Body> {
    let builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {token}"));
    match body {
        Some(b) => builder
            .header("content-type", "application/json")
            .body(Body::from(b.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    }
}

async fn status(app: &axum::Router, req: Request<Body>) -> StatusCode {
    app.clone().oneshot(req).await.unwrap().status()
}

async fn admin_token(state: &AppState) -> String {
    let root = UserId::new("root");
    let token = state.access.bootstrap_admin(&root).await.unwrap();
    state
        .access
        .add_creator(&RepoId::new(REPO_ID), &root)
        .await
        .unwrap();
    token
}

#[tokio::test]
async fn the_dev_header_is_ignored_unless_dev_auth_is_on() {
    let app = router(state(false).await);
    let req = Request::get("/v1/repos/owner/game/files")
        .header(api::DEV_USER_HEADER, "alice")
        .body(Body::empty())
        .unwrap();
    assert_eq!(status(&app, req).await, StatusCode::UNAUTHORIZED);
    let bare = Request::get("/v1/repos/owner/game/files")
        .body(Body::empty())
        .unwrap();
    assert_eq!(status(&app, bare).await, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn health_and_openapi_need_no_credentials() {
    let app = router(state(false).await);
    for uri in ["/healthz", "/openapi.json"] {
        assert_eq!(
            status(&app, Request::get(uri).body(Body::empty()).unwrap()).await,
            StatusCode::OK,
            "{uri}"
        );
    }
}

#[tokio::test]
async fn a_read_only_token_can_read_but_not_lock_or_check_in() {
    let st = state(false).await;
    let admin = admin_token(&st).await;
    let app = router(st);

    let made: api::CreatedToken = body_json(
        app.clone()
            .oneshot(with_token(
                "POST",
                "/v1/tokens",
                &admin,
                Some(serde_json::json!({"name": "ci", "repos": [REPO], "permissions": ["read"]})),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert!(made.token.starts_with("pyn_"));
    assert_eq!(made.info.permissions, ["read"]);

    assert_eq!(
        status(
            &app,
            with_token("GET", "/v1/repos/owner/game/files", &made.token, None)
        )
        .await,
        StatusCode::OK
    );
    let checkout = serde_json::json!({"path": "Content/a.umap", "base_revision": null});
    let r = app
        .clone()
        .oneshot(with_token(
            "POST",
            "/v1/repos/owner/game/checkout",
            &made.token,
            Some(checkout),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    let err: api::ErrorBody = body_json(r).await;
    assert_eq!(err.code, "forbidden");
}

#[tokio::test]
async fn a_token_cannot_ask_for_more_than_its_owner_holds() {
    let st = state(false).await;
    let admin = admin_token(&st).await;
    st.access
        .set_user_role(
            &st.access
                .principal(&RepoId::new(REPO_ID), &UserId::new("root"))
                .await
                .unwrap(),
            &RepoId::new(REPO_ID),
            &UserId::new("wendy"),
            pyn_core::Role::Writer,
        )
        .await
        .unwrap();
    let wendy = st
        .access
        .principal(&RepoId::new(REPO_ID), &UserId::new("wendy"))
        .await
        .unwrap();
    let (_, wendy_token) = st
        .access
        .create_token(
            &session("wendy"),
            "w",
            wendy.permissions.clone(),
            vec![RepoId::new(REPO_ID)],
            None,
        )
        .await
        .unwrap();
    let app = router(st);
    let _ = admin;

    let body =
        serde_json::json!({"name": "sneaky", "repos": [REPO], "permissions": ["read", "restore"]});
    assert_eq!(
        status(
            &app,
            with_token("POST", "/v1/tokens", &wendy_token, Some(body))
        )
        .await,
        StatusCode::FORBIDDEN
    );
    let body = serde_json::json!({"name": "nope", "repos": [REPO], "permissions": ["flying"]});
    assert_eq!(
        status(
            &app,
            with_token("POST", "/v1/tokens", &wendy_token, Some(body))
        )
        .await,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn a_revoked_token_stops_working_and_whoami_reports_the_caller() {
    let st = state(false).await;
    let admin = admin_token(&st).await;
    let app = router(st);

    let account: api::Account = body_json(
        app.clone()
            .oneshot(with_token("GET", "/v1/me", &admin, None))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(account.user, "root");
    let me: api::Me = body_json(
        app.clone()
            .oneshot(with_token("GET", "/v1/repos/owner/game/me", &admin, None))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!((me.user.as_str(), me.permissions.len()), ("root", 9));

    let made: api::CreatedToken = body_json(
        app.clone()
            .oneshot(with_token(
                "POST",
                "/v1/tokens",
                &admin,
                Some(serde_json::json!({"name": "tmp", "repos": [REPO], "permissions": ["read"]})),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        status(&app, with_token("GET", "/v1/me", &made.token, None)).await,
        StatusCode::OK
    );
    let uri = format!("/v1/tokens/{}", made.info.id);
    assert_eq!(
        status(&app, with_token("DELETE", &uri, &admin, None)).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        status(&app, with_token("GET", "/v1/me", &made.token, None)).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        status(
            &app,
            with_token("DELETE", "/v1/tokens/ffffffffffff", &admin, None)
        )
        .await,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn admins_manage_members_and_roles_and_others_cannot() {
    let st = state(false).await;
    let admin = admin_token(&st).await;
    let app = router(st);

    let add = serde_json::json!({"role": "writer"});
    assert_eq!(
        status(
            &app,
            with_token(
                "PUT",
                "/v1/repos/owner/game/members/wendy",
                &admin,
                Some(add)
            )
        )
        .await,
        StatusCode::NO_CONTENT
    );
    let members: Vec<api::Member> = body_json(
        app.clone()
            .oneshot(with_token(
                "GET",
                "/v1/repos/owner/game/members",
                &admin,
                None,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert!(
        members
            .iter()
            .any(|m| m.user == "wendy" && m.role == "writer")
    );

    let grants: Vec<api::RoleGrant> = body_json(
        app.clone()
            .oneshot(with_token(
                "GET",
                "/v1/repos/owner/game/roles",
                &admin,
                None,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(grants.len(), 4);

    let narrow = serde_json::json!({"permissions": ["read"]});
    assert_eq!(
        status(
            &app,
            with_token(
                "PUT",
                "/v1/repos/owner/game/roles/writer",
                &admin,
                Some(narrow)
            )
        )
        .await,
        StatusCode::NO_CONTENT
    );
    let lock_out = serde_json::json!({"permissions": ["read"]});
    assert_eq!(
        status(
            &app,
            with_token(
                "PUT",
                "/v1/repos/owner/game/roles/admin",
                &admin,
                Some(lock_out)
            )
        )
        .await,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn restore_needs_the_permission_the_lock_and_the_confirmation() {
    let st = state(false).await;
    let admin = admin_token(&st).await;
    let root = st
        .access
        .principal(&RepoId::new(REPO_ID), &UserId::new("root"))
        .await
        .unwrap();
    st.access
        .set_user_role(
            &root,
            &RepoId::new(REPO_ID),
            &UserId::new("wendy"),
            pyn_core::Role::Writer,
        )
        .await
        .unwrap();
    let wendy = st
        .access
        .principal(&RepoId::new(REPO_ID), &UserId::new("wendy"))
        .await
        .unwrap();
    let (_, writer) = st
        .access
        .create_token(
            &session("wendy"),
            "w",
            wendy.permissions.clone(),
            vec![RepoId::new(REPO_ID)],
            None,
        )
        .await
        .unwrap();
    let app = router(st);

    for (i, text) in ["one", "two"].into_iter().enumerate() {
        let base = if i == 0 {
            serde_json::Value::Null
        } else {
            serde_json::json!(i)
        };
        let r = app
            .clone()
            .oneshot(with_token(
                "POST",
                "/v1/repos/owner/game/checkout",
                &admin,
                Some(serde_json::json!({"path": "Content/m.umap", "base_revision": base})),
            ))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        let put = Request::put("/v1/repos/owner/game/objects")
            .header("authorization", format!("Bearer {admin}"))
            .body(Body::from(text))
            .unwrap();
        let obj: api::PutObjectResponse = body_json(app.clone().oneshot(put).await.unwrap()).await;
        let checkin = serde_json::json!({"path": "Content/m.umap", "content": obj.content, "base_revision": base, "message": text});
        assert_eq!(
            status(
                &app,
                with_token(
                    "POST",
                    "/v1/repos/owner/game/checkin",
                    &admin,
                    Some(checkin)
                )
            )
            .await,
            StatusCode::OK
        );
    }

    let restore = |confirm: &str| serde_json::json!({"path": "Content/m.umap", "revision": 1, "base_revision": 2, "confirm": confirm});
    let r = app
        .clone()
        .oneshot(with_token(
            "POST",
            "/v1/repos/owner/game/restore",
            &writer,
            Some(restore("Content/m.umap@r2")),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    let err: api::ErrorBody = body_json(r).await;
    assert_eq!(err.code, "forbidden");

    let r = app
        .clone()
        .oneshot(with_token(
            "POST",
            "/v1/repos/owner/game/restore",
            &admin,
            Some(restore("Content/m.umap@r2")),
        ))
        .await
        .unwrap();
    let err: api::ErrorBody = body_json(r).await;
    assert_eq!(err.code, "lock_required");

    let lock = serde_json::json!({"path": "Content/m.umap", "base_revision": 2});
    assert_eq!(
        status(
            &app,
            with_token("POST", "/v1/repos/owner/game/checkout", &admin, Some(lock))
        )
        .await,
        StatusCode::OK
    );
    let r = app
        .clone()
        .oneshot(with_token(
            "POST",
            "/v1/repos/owner/game/restore",
            &admin,
            Some(restore("yes")),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::CONFLICT);
    let err: api::ErrorBody = body_json(r).await;
    assert_eq!(err.code, "confirmation_required");
    assert!(err.message.contains("Content/m.umap@r2"));

    let r = app
        .clone()
        .oneshot(with_token(
            "POST",
            "/v1/repos/owner/game/restore",
            &admin,
            Some(restore("Content/m.umap@r2")),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let rev: api::Revision = body_json(r).await;
    assert_eq!((rev.id, rev.restored_from), (3, Some(1)));
}

async fn member_token(st: &AppState, name: &str, role: pyn_core::Role) -> String {
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

#[tokio::test]
async fn force_unlock_needs_the_permission_and_a_reason_and_is_audited() {
    let st = state(false).await;
    admin_token(&st).await;
    let wendy = member_token(&st, "wendy", pyn_core::Role::Writer).await;
    let maya = member_token(&st, "maya", pyn_core::Role::Maintainer).await;
    let app = router(st);

    let lock = serde_json::json!({"path": "Content/a.umap", "base_revision": null});
    assert_eq!(
        status(
            &app,
            with_token("POST", "/v1/repos/owner/game/checkout", &wendy, Some(lock))
        )
        .await,
        StatusCode::OK
    );

    let unlock = |reason: &str| serde_json::json!({"path": "Content/a.umap", "reason": reason});
    let r = app
        .clone()
        .oneshot(with_token(
            "POST",
            "/v1/repos/owner/game/force-unlock",
            &wendy,
            Some(unlock("mine")),
        ))
        .await
        .unwrap();
    assert_eq!(
        r.status(),
        StatusCode::FORBIDDEN,
        "a writer cannot force an unlock"
    );
    assert_eq!(
        status(
            &app,
            with_token(
                "POST",
                "/v1/repos/owner/game/force-unlock",
                &maya,
                Some(unlock("  "))
            )
        )
        .await,
        StatusCode::BAD_REQUEST
    );

    let r = app
        .clone()
        .oneshot(with_token(
            "POST",
            "/v1/repos/owner/game/force-unlock",
            &maya,
            Some(unlock("wendy is away")),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let removed: api::Lock = body_json(r).await;
    assert_eq!(removed.owner, "wendy");
    let r = app
        .clone()
        .oneshot(with_token(
            "POST",
            "/v1/repos/owner/game/force-unlock",
            &maya,
            Some(unlock("again")),
        ))
        .await
        .unwrap();
    let err: api::ErrorBody = body_json(r).await;
    assert_eq!(err.code, "not_locked");

    assert_eq!(
        status(
            &app,
            with_token("GET", "/v1/repos/owner/game/audit", &wendy, None)
        )
        .await,
        StatusCode::FORBIDDEN
    );
    let page: api::AuditPage = body_json(
        app.clone()
            .oneshot(with_token("GET", "/v1/repos/owner/game/audit", &maya, None))
            .await
            .unwrap(),
    )
    .await;
    let actions: Vec<_> = page
        .entries
        .iter()
        .filter(|e| matches!(e.action.as_str(), "force_unlock" | "checkout"))
        .map(|e| (e.action.as_str(), e.actor.as_str()))
        .collect();
    assert_eq!(actions, [("force_unlock", "maya"), ("checkout", "wendy")]);
    assert!(page.entries[0].detail.contains("wendy is away"));

    let only: api::AuditPage = body_json(
        app.clone()
            .oneshot(with_token(
                "GET",
                "/v1/repos/owner/game/audit?action=checkout&limit=1",
                &maya,
                None,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(only.entries.len(), 1);
    assert_eq!(
        status(
            &app,
            with_token(
                "GET",
                "/v1/repos/owner/game/audit?action=bogus",
                &maya,
                None
            )
        )
        .await,
        StatusCode::BAD_REQUEST
    );
}

fn anon(method: &str, uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

const PASSWORD: &str = "correct horse battery";

#[tokio::test]
async fn registration_follows_the_servers_mode() {
    let closed = router(state_with(false, RegistrationMode::Closed).await);
    let r = closed
        .clone()
        .oneshot(
            Request::get("/v1/registration")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        body_json::<api::RegistrationInfo>(r).await.registration,
        "closed"
    );
    let r = closed
        .oneshot(anon(
            "POST",
            "/v1/register",
            serde_json::json!({"username": "alice", "password": PASSWORD}),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        body_json::<api::ErrorBody>(r).await.code,
        "registration_closed"
    );

    let open = router(state_with(false, RegistrationMode::Open).await);
    let join = serde_json::json!({"username": "alice", "password": PASSWORD});
    assert_eq!(
        status(&open, anon("POST", "/v1/register", join.clone())).await,
        StatusCode::CREATED
    );
    let r = open
        .clone()
        .oneshot(anon("POST", "/v1/register", join))
        .await
        .unwrap();
    assert_eq!(
        (
            r.status(),
            body_json::<api::ErrorBody>(r).await.code.as_str()
        ),
        (StatusCode::CONFLICT, "user_exists")
    );
    let weak = serde_json::json!({"username": "bob", "password": "short"});
    assert_eq!(
        status(&open, anon("POST", "/v1/register", weak)).await,
        StatusCode::BAD_REQUEST
    );

    let r = open
        .clone()
        .oneshot(anon(
            "POST",
            "/v1/login",
            serde_json::json!({"username": "alice", "password": PASSWORD}),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let session: api::CreatedToken = body_json(r).await;
    let me: api::Account = body_json(
        open.clone()
            .oneshot(with_token("GET", "/v1/me", &session.token, None))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(me.user, "alice");
    assert_eq!(
        status(
            &open,
            with_token("GET", "/v1/repos/owner/game/me", &session.token, None)
        )
        .await,
        StatusCode::NOT_FOUND,
        "registering gives no access to any repository"
    );
}

#[tokio::test]
async fn an_invite_only_server_admits_people_with_a_one_time_invitation() {
    let st = state(false).await;
    let admin = admin_token(&st).await;
    let app = router(st);
    let join = |code: Option<&str>| serde_json::json!({"username": "wendy", "password": PASSWORD, "invite": code});

    let r = app
        .clone()
        .oneshot(anon("POST", "/v1/register", join(None)))
        .await
        .unwrap();
    assert_eq!(
        (
            r.status(),
            body_json::<api::ErrorBody>(r).await.code.as_str()
        ),
        (StatusCode::BAD_REQUEST, "invalid_invite")
    );

    let make = serde_json::json!({"role": "writer", "hours": 24});
    let invite: api::CreatedInvite = body_json(
        app.clone()
            .oneshot(with_token(
                "POST",
                "/v1/repos/owner/game/invites",
                &admin,
                Some(make),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert!(invite.code.starts_with("pyni_"));
    assert_eq!(
        status(&app, anon("POST", "/v1/register", join(Some(&invite.code)))).await,
        StatusCode::CREATED
    );

    let again =
        serde_json::json!({"username": "xavier", "password": PASSWORD, "invite": invite.code});
    assert_eq!(
        status(&app, anon("POST", "/v1/register", again)).await,
        StatusCode::BAD_REQUEST
    );

    let listed: Vec<api::InviteInfo> = body_json(
        app.clone()
            .oneshot(with_token(
                "GET",
                "/v1/repos/owner/game/invites",
                &admin,
                None,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        (listed.len(), listed[0].used_by.as_deref()),
        (1, Some("wendy"))
    );
    let r = app
        .clone()
        .oneshot(anon(
            "POST",
            "/v1/login",
            serde_json::json!({"username": "wendy", "password": PASSWORD}),
        ))
        .await
        .unwrap();
    let wendy: api::CreatedToken = body_json(r).await;
    assert_eq!(
        status(
            &app,
            with_token(
                "POST",
                "/v1/repos/owner/game/invites",
                &wendy.token,
                Some(serde_json::json!({"role": "reader", "hours": 1}))
            )
        )
        .await,
        StatusCode::FORBIDDEN
    );

    let uri = format!("/v1/repos/owner/game/invites/{}", listed[0].id);
    assert_eq!(
        status(&app, with_token("DELETE", &uri, &admin, None)).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        status(
            &app,
            with_token(
                "DELETE",
                "/v1/repos/owner/game/invites/ffffffffffff",
                &admin,
                None
            )
        )
        .await,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn sign_in_is_throttled_and_never_says_which_part_was_wrong() {
    let st = state(false).await;
    let admin = admin_token(&st).await;
    let app = router(st);
    let add = serde_json::json!({"username": "alice", "password": PASSWORD, "role": "writer"});
    assert_eq!(
        status(
            &app,
            with_token(
                "POST",
                "/v1/repos/owner/game/users",
                &admin,
                Some(add.clone())
            )
        )
        .await,
        StatusCode::CREATED
    );
    assert_eq!(
        status(
            &app,
            with_token("POST", "/v1/repos/owner/game/users", &admin, Some(add))
        )
        .await,
        StatusCode::CONFLICT
    );

    let wrong = |user: &str| {
        anon(
            "POST",
            "/v1/login",
            serde_json::json!({"username": user, "password": "not the password"}),
        )
    };
    let a = app.clone().oneshot(wrong("alice")).await.unwrap();
    let b = app.clone().oneshot(wrong("nobody")).await.unwrap();
    assert_eq!(
        (a.status(), b.status()),
        (StatusCode::UNAUTHORIZED, StatusCode::UNAUTHORIZED)
    );
    assert_eq!(
        body_json::<api::ErrorBody>(a).await.message,
        body_json::<api::ErrorBody>(b).await.message
    );

    for _ in 0..4 {
        app.clone().oneshot(wrong("alice")).await.unwrap();
    }
    let r = app
        .clone()
        .oneshot(anon(
            "POST",
            "/v1/login",
            serde_json::json!({"username": "alice", "password": PASSWORD}),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        body_json::<api::ErrorBody>(r).await.code,
        "too_many_attempts"
    );
}

#[tokio::test]
async fn people_can_change_their_own_password() {
    let st = state_with(false, RegistrationMode::Open).await;
    let app = router(st);
    app.clone()
        .oneshot(anon(
            "POST",
            "/v1/register",
            serde_json::json!({"username": "alice", "password": PASSWORD}),
        ))
        .await
        .unwrap();
    let login = |pw: &str| {
        anon(
            "POST",
            "/v1/login",
            serde_json::json!({"username": "alice", "password": pw}),
        )
    };
    let session: api::CreatedToken =
        body_json(app.clone().oneshot(login(PASSWORD)).await.unwrap()).await;

    let wrong = serde_json::json!({"current": "not the password", "new": "a brand new password"});
    assert_eq!(
        status(
            &app,
            with_token("PUT", "/v1/me/password", &session.token, Some(wrong))
        )
        .await,
        StatusCode::UNAUTHORIZED
    );
    let right = serde_json::json!({"current": PASSWORD, "new": "a brand new password"});
    assert_eq!(
        status(
            &app,
            with_token("PUT", "/v1/me/password", &session.token, Some(right))
        )
        .await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        status(&app, login("a brand new password")).await,
        StatusCode::OK
    );
    assert_eq!(
        status(&app, login(PASSWORD)).await,
        StatusCode::UNAUTHORIZED
    );
}

const ED1: &str = include_str!("../../pyn-core/tests/fixtures/keys/ed1.pub");
const ED2: &str = include_str!("../../pyn-core/tests/fixtures/keys/ed2.pub");
const DSA: &str = include_str!("../../pyn-core/tests/fixtures/keys/dsa.pub");

#[tokio::test]
async fn people_link_ssh_keys_to_their_accounts_like_on_github() {
    let st = state_with(false, RegistrationMode::Open).await;
    let app = router(st);
    for name in ["alice", "bob"] {
        app.clone()
            .oneshot(anon(
                "POST",
                "/v1/register",
                serde_json::json!({"username": name, "password": PASSWORD}),
            ))
            .await
            .unwrap();
    }
    let session = |name: &str| {
        let app = app.clone();
        let name = name.to_string();
        async move {
            let r = app
                .oneshot(anon(
                    "POST",
                    "/v1/login",
                    serde_json::json!({"username": name, "password": PASSWORD}),
                ))
                .await
                .unwrap();
            body_json::<api::CreatedToken>(r).await.token
        }
    };
    let (alice, bob) = (session("alice").await, session("bob").await);

    let r = app
        .clone()
        .oneshot(with_token(
            "POST",
            "/v1/keys",
            &alice,
            Some(serde_json::json!({"key": ED1})),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let key: api::SshKeyInfo = body_json(r).await;
    assert_eq!(
        (
            key.user.as_str(),
            key.title.as_str(),
            key.algorithm.as_str()
        ),
        ("alice", "alice@laptop", "ssh-ed25519")
    );
    assert!(key.fingerprint.starts_with("SHA256:"));

    let r = app
        .clone()
        .oneshot(with_token(
            "POST",
            "/v1/keys",
            &bob,
            Some(serde_json::json!({"key": ED1})),
        ))
        .await
        .unwrap();
    assert_eq!(
        (
            r.status(),
            body_json::<api::ErrorBody>(r).await.code.as_str()
        ),
        (StatusCode::CONFLICT, "key_in_use")
    );
    let r = app
        .clone()
        .oneshot(with_token(
            "POST",
            "/v1/keys",
            &bob,
            Some(serde_json::json!({"key": DSA})),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        status(
            &app,
            with_token(
                "POST",
                "/v1/keys",
                &bob,
                Some(serde_json::json!({"key": ED2, "title": "desk"}))
            )
        )
        .await,
        StatusCode::OK
    );

    let listed: Vec<api::SshKeyInfo> = body_json(
        app.clone()
            .oneshot(with_token("GET", "/v1/keys", &alice, None))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(listed.len(), 1, "only your own keys");
    assert_eq!(
        status(&app, with_token("GET", "/v1/keys?user=bob", &alice, None)).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        status(
            &app,
            with_token(
                "DELETE",
                &format!("/v1/keys/{}?user=alice", key.id),
                &bob,
                None
            )
        )
        .await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        status(
            &app,
            with_token("DELETE", &format!("/v1/keys/{}", key.id), &alice, None)
        )
        .await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        status(
            &app,
            with_token("DELETE", &format!("/v1/keys/{}", key.id), &alice, None)
        )
        .await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        status(&app, Request::get("/v1/keys").body(Body::empty()).unwrap()).await,
        StatusCode::UNAUTHORIZED
    );
}

fn with_cookie(
    method: &str,
    uri: &str,
    cookie: &str,
    csrf: Option<&str>,
    body: Option<serde_json::Value>,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("cookie", format!("{}={cookie}", api::SESSION_COOKIE));
    if let Some(csrf) = csrf {
        builder = builder.header(api::CSRF_HEADER, csrf);
    }
    match body {
        Some(b) => builder
            .header("content-type", "application/json")
            .body(Body::from(b.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    }
}

async fn web_sign_in(
    app: &axum::Router,
    forwarded_proto: Option<&str>,
) -> (String, String, api::SessionInfo) {
    let mut req = anon(
        "POST",
        "/v1/session",
        serde_json::json!({"username": "alice", "password": PASSWORD}),
    );
    if let Some(proto) = forwarded_proto {
        req.headers_mut()
            .insert("x-forwarded-proto", proto.parse().unwrap());
    }
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let set = res.headers()["set-cookie"].to_str().unwrap().to_string();
    let info: api::SessionInfo = body_json(res).await;
    (set, info.csrf_token.clone(), info)
}

#[tokio::test]
async fn a_web_session_is_an_httponly_cookie_that_signs_out_and_follows_the_role() {
    let st = state_with(false, RegistrationMode::Open).await;
    let app = router(st.clone());
    app.clone()
        .oneshot(anon(
            "POST",
            "/v1/register",
            serde_json::json!({"username": "alice", "password": PASSWORD}),
        ))
        .await
        .unwrap();

    let (set, csrf, info) = web_sign_in(&app, None).await;
    assert_eq!(info.user, "alice");
    for part in ["HttpOnly", "SameSite=Lax", "Path=/"] {
        assert!(set.contains(part), "{set}");
    }
    assert!(!set.contains("Secure"), "plain HTTP: {set}");
    let cookie = set
        .split(';')
        .next()
        .unwrap()
        .strip_prefix("pyn_session=")
        .unwrap()
        .to_string();
    assert!(!cookie.is_empty() && !set.contains(&csrf));

    let (set_https, _, _) = web_sign_in(&app, Some("https")).await;
    assert!(set_https.contains("; Secure"), "{set_https}");

    let me = |c: &str| with_cookie("GET", "/v1/repos/owner/game/me", c, None, None);
    let account = |c: &str| with_cookie("GET", "/v1/me", c, None, None);
    assert_eq!(status(&app, account(&cookie)).await, StatusCode::OK);
    assert_eq!(status(&app, me(&cookie)).await, StatusCode::NOT_FOUND);
    assert_eq!(
        status(&app, with_cookie("GET", "/v1/session", &cookie, None, None)).await,
        StatusCode::OK
    );
    assert_eq!(
        status(&app, account("forged")).await,
        StatusCode::UNAUTHORIZED
    );

    let root = admin_token(&st).await;
    let promote = |role: &str| {
        with_token(
            "PUT",
            "/v1/repos/owner/game/members/alice",
            &root,
            Some(serde_json::json!({"role": role})),
        )
    };
    assert_eq!(
        status(&app, promote("writer")).await,
        StatusCode::NO_CONTENT
    );
    let after: api::Me = body_json(app.clone().oneshot(me(&cookie)).await.unwrap()).await;
    assert!(after.permissions.contains(&"checkin".to_string()));

    let out = app
        .clone()
        .oneshot(with_cookie(
            "DELETE",
            "/v1/session",
            &cookie,
            Some(&csrf),
            None,
        ))
        .await
        .unwrap();
    assert_eq!(out.status(), StatusCode::NO_CONTENT);
    assert!(
        out.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .contains("Max-Age=0")
    );
    assert_eq!(status(&app, me(&cookie)).await, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn state_changing_requests_with_a_cookie_need_the_csrf_token() {
    let st = state_with(false, RegistrationMode::Open).await;
    let app = router(st);
    app.clone()
        .oneshot(anon(
            "POST",
            "/v1/register",
            serde_json::json!({"username": "alice", "password": PASSWORD}),
        ))
        .await
        .unwrap();
    let (set, csrf, _) = web_sign_in(&app, None).await;
    let cookie = set
        .split(';')
        .next()
        .unwrap()
        .strip_prefix("pyn_session=")
        .unwrap()
        .to_string();

    let body = || Some(serde_json::json!({"title": "laptop", "key": "ssh-ed25519 bad"}));
    for token in [None, Some("wrong")] {
        let res = app
            .clone()
            .oneshot(with_cookie("POST", "/v1/keys", &cookie, token, body()))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
        let err: api::ErrorBody = body_json(res).await;
        assert_eq!(err.code, "csrf_failed");
    }
    let pw = serde_json::json!({"current": PASSWORD, "new": "a brand new password"});
    assert_eq!(
        status(
            &app,
            with_cookie("PUT", "/v1/me/password", &cookie, None, Some(pw.clone()))
        )
        .await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        status(
            &app,
            with_cookie("PUT", "/v1/me/password", &cookie, Some(&csrf), Some(pw))
        )
        .await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        status(
            &app,
            with_cookie("DELETE", "/v1/session", &cookie, None, None)
        )
        .await,
        StatusCode::FORBIDDEN,
        "signing out needs the token too"
    );
}

#[tokio::test]
async fn bearer_requests_need_no_csrf_token_even_beside_a_session_cookie() {
    let st = state_with(false, RegistrationMode::Open).await;
    let app = router(st.clone());
    app.clone()
        .oneshot(anon(
            "POST",
            "/v1/register",
            serde_json::json!({"username": "alice", "password": PASSWORD}),
        ))
        .await
        .unwrap();
    let (set, _, _) = web_sign_in(&app, None).await;
    let cookie = set.split(';').next().unwrap().to_string();
    let root = admin_token(&st).await;
    let mut req = with_token(
        "POST",
        "/v1/tokens",
        &root,
        Some(serde_json::json!({"name": "ci", "repos": [REPO], "permissions": ["read"]})),
    );
    req.headers_mut().insert("cookie", cookie.parse().unwrap());
    assert_eq!(status(&app, req).await, StatusCode::OK);
}

#[tokio::test]
async fn member_role_and_token_changes_are_audited_without_secrets() {
    let st = state(false).await;
    let admin = admin_token(&st).await;
    let wendy = member_token(&st, "wendy", pyn_core::Role::Writer).await;
    let app = router(st);
    let send = |method, uri: &str, tok: &str, body| {
        app.clone().oneshot(with_token(method, uri, tok, body))
    };

    send(
        "PUT",
        "/v1/repos/owner/game/members/wendy",
        &admin,
        Some(serde_json::json!({"role": "maintainer"})),
    )
    .await
    .unwrap();
    send(
        "PUT",
        "/v1/repos/owner/game/roles/reader",
        &admin,
        Some(serde_json::json!({"permissions": ["read", "view_audit"]})),
    )
    .await
    .unwrap();
    let made: api::CreatedToken = body_json(
        send(
            "POST",
            "/v1/tokens",
            &admin,
            Some(serde_json::json!({"name": "ci", "repos": [REPO], "permissions": ["read"]})),
        )
        .await
        .unwrap(),
    )
    .await;
    send(
        "DELETE",
        &format!("/v1/tokens/{}", made.info.id),
        &admin,
        None,
    )
    .await
    .unwrap();

    assert_eq!(
        status(
            &app,
            with_token("GET", "/v1/repos/owner/game/audit", &wendy, None)
        )
        .await,
        StatusCode::FORBIDDEN,
        "a writer cannot read the audit log"
    );
    let page: api::AuditPage = body_json(
        send("GET", "/v1/repos/owner/game/audit", &admin, None)
            .await
            .unwrap(),
    )
    .await;
    let seen: Vec<_> = page
        .entries
        .iter()
        .map(|e| (e.action.as_str(), e.actor.as_str()))
        .collect();
    assert_eq!(
        seen,
        [
            ("token_revoked", "root"),
            ("token_created", "root"),
            ("role_permissions_changed", "root"),
            ("role_changed", "root"),
            ("token_created", "wendy"),
            ("member_added", "root"),
            ("member_added", "root"),
        ]
    );
    assert!(
        page.entries[3]
            .detail
            .contains("wendy: writer -> maintainer")
    );
    assert!(page.entries[2].detail.contains("added [view_audit]"));
    let everything = serde_json::to_string(&page).unwrap();
    for secret in [&made.token, &admin, &wendy] {
        assert!(!everything.contains(secret.as_str()), "token secret leaked");
    }
}

async fn register_and_sign_in(app: &axum::Router, name: &str) -> String {
    let r = app
        .clone()
        .oneshot(anon(
            "POST",
            "/v1/register",
            serde_json::json!({"username": name, "password": PASSWORD}),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::CREATED);
    let r = app
        .clone()
        .oneshot(anon(
            "POST",
            "/v1/login",
            serde_json::json!({"username": name, "password": PASSWORD}),
        ))
        .await
        .unwrap();
    body_json::<api::CreatedToken>(r).await.token
}

async fn send(
    app: &axum::Router,
    method: &str,
    uri: &str,
    token: &str,
    body: Option<serde_json::Value>,
) -> axum::response::Response {
    app.clone()
        .oneshot(with_token(method, uri, token, body))
        .await
        .unwrap()
}

async fn create_repo(app: &axum::Router, token: &str, name: &str) -> api::RepoInfo {
    let r = send(
        app,
        "POST",
        "/v1/repos",
        token,
        Some(serde_json::json!({"name": name})),
    )
    .await;
    assert_eq!(r.status(), StatusCode::CREATED);
    body_json(r).await
}

#[tokio::test]
async fn anyone_can_create_repositories_in_their_own_namespace() {
    let app = router(state_with(false, RegistrationMode::Open).await);
    let alice = register_and_sign_in(&app, "alice").await;

    let made = create_repo(&app, &alice, "game").await;
    assert_eq!(
        (
            made.owner.as_str(),
            made.name.as_str(),
            made.role.as_deref()
        ),
        ("alice", "game", Some("admin"))
    );
    assert_eq!(made.visibility, api::Visibility::Private);
    assert_eq!(made.lease_hours, 8);

    let r = send(&app, "GET", "/v1/repos/alice/game", &alice, None).await;
    assert_eq!(body_json::<api::RepoInfo>(r).await, made);
    let me: api::Me =
        body_json(send(&app, "GET", "/v1/repos/alice/game/me", &alice, None).await).await;
    assert_eq!(me.permissions.len(), 9);

    let dup = send(
        &app,
        "POST",
        "/v1/repos",
        &alice,
        Some(serde_json::json!({"name": "game"})),
    )
    .await;
    assert_eq!(dup.status(), StatusCode::CONFLICT);
    assert_eq!(body_json::<api::ErrorBody>(dup).await.code, "repo_exists");

    let bad = send(
        &app,
        "POST",
        "/v1/repos",
        &alice,
        Some(serde_json::json!({"name": "Not Ok"})),
    )
    .await;
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        body_json::<api::ErrorBody>(bad).await.code,
        "invalid_repo_name"
    );

    let elsewhere = send(
        &app,
        "POST",
        "/v1/repos",
        &alice,
        Some(serde_json::json!({"owner": "bob", "name": "game"})),
    )
    .await;
    assert_eq!(elsewhere.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        body_json::<api::ErrorBody>(elsewhere).await.code,
        "not_namespace_owner"
    );

    let anon_create = app
        .clone()
        .oneshot(anon("POST", "/v1/repos", serde_json::json!({"name": "x"})))
        .await
        .unwrap();
    assert_eq!(anon_create.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn listing_shows_only_repositories_you_belong_to() {
    let app = router(state_with(false, RegistrationMode::Open).await);
    let alice = register_and_sign_in(&app, "alice").await;
    let bob = register_and_sign_in(&app, "bob").await;
    create_repo(&app, &alice, "one").await;
    create_repo(&app, &alice, "two").await;
    create_repo(&app, &bob, "three").await;

    let list = |token: String, query: &'static str| {
        let app = app.clone();
        async move {
            let r = send(&app, "GET", &format!("/v1/repos{query}"), &token, None).await;
            let repos: Vec<api::RepoInfo> = body_json(r).await;
            repos
                .into_iter()
                .map(|r| format!("{}/{}", r.owner, r.name))
                .collect::<Vec<_>>()
        }
    };
    assert_eq!(list(alice.clone(), "").await, ["alice/one", "alice/two"]);
    assert_eq!(list(bob.clone(), "").await, ["bob/three"]);
    assert!(list(bob, "?owner=alice").await.is_empty());
}

#[tokio::test]
async fn repositories_keep_files_locks_roles_and_audit_apart() {
    let app = router(state_with(false, RegistrationMode::Open).await);
    let alice = register_and_sign_in(&app, "alice").await;
    create_repo(&app, &alice, "one").await;
    create_repo(&app, &alice, "two").await;

    let checkout = serde_json::json!({"path": "Content/a.bin", "base_revision": null});
    assert_eq!(
        send(
            &app,
            "POST",
            "/v1/repos/alice/one/checkout",
            &alice,
            Some(checkout.clone())
        )
        .await
        .status(),
        StatusCode::OK
    );
    let locks = |repo: &'static str| {
        let (app, alice) = (app.clone(), alice.clone());
        async move {
            let r = send(
                &app,
                "GET",
                &format!("/v1/repos/alice/{repo}/locks"),
                &alice,
                None,
            )
            .await;
            body_json::<Vec<api::Lock>>(r).await.len()
        }
    };
    assert_eq!((locks("one").await, locks("two").await), (1, 0));
    assert_eq!(
        send(
            &app,
            "POST",
            "/v1/repos/alice/two/checkout",
            &alice,
            Some(checkout)
        )
        .await
        .status(),
        StatusCode::OK,
        "the same path is free in another repository"
    );

    let audit = |repo: &'static str| {
        let (app, alice) = (app.clone(), alice.clone());
        async move {
            let r = send(
                &app,
                "GET",
                &format!("/v1/repos/alice/{repo}/audit?action=checkout"),
                &alice,
                None,
            )
            .await;
            body_json::<api::AuditPage>(r).await.entries.len()
        }
    };
    assert_eq!((audit("one").await, audit("two").await), (1, 1));

    send(
        &app,
        "PUT",
        "/v1/repos/alice/one/members/carol",
        &alice,
        Some(serde_json::json!({"role": "reader"})),
    )
    .await;
    let members = |repo: &'static str| {
        let (app, alice) = (app.clone(), alice.clone());
        async move {
            let r = send(
                &app,
                "GET",
                &format!("/v1/repos/alice/{repo}/members"),
                &alice,
                None,
            )
            .await;
            body_json::<Vec<api::Member>>(r).await.len()
        }
    };
    assert_eq!((members("one").await, members("two").await), (2, 1));
}

#[tokio::test]
async fn outsiders_cannot_tell_a_private_repository_exists() {
    let app = router(state_with(false, RegistrationMode::Open).await);
    let alice = register_and_sign_in(&app, "alice").await;
    let bob = register_and_sign_in(&app, "bob").await;
    create_repo(&app, &alice, "secret").await;

    for uri in [
        "/v1/repos/alice/secret",
        "/v1/repos/alice/secret/files",
        "/v1/repos/alice/secret/locks",
        "/v1/repos/alice/nothing/files",
    ] {
        let r = send(&app, "GET", uri, &bob, None).await;
        assert_eq!(r.status(), StatusCode::NOT_FOUND, "{uri}");
        let body: api::ErrorBody = body_json(r).await;
        assert_eq!(body.code, "repo_not_found", "{uri}");
        assert!(!body.message.contains("permission"), "{uri}");
    }
    assert_eq!(
        send(&app, "DELETE", "/v1/repos/alice/secret", &bob, None)
            .await
            .status(),
        StatusCode::NOT_FOUND
    );

    send(
        &app,
        "PUT",
        "/v1/repos/alice/secret/members/bob",
        &alice,
        Some(serde_json::json!({"role": "reader"})),
    )
    .await;
    assert_eq!(
        send(&app, "GET", "/v1/repos/alice/secret/files", &bob, None)
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        send(&app, "DELETE", "/v1/repos/alice/secret", &bob, None)
            .await
            .status(),
        StatusCode::FORBIDDEN,
        "a member who is not the owner cannot delete"
    );
}

#[tokio::test]
async fn a_token_works_only_in_the_repositories_it_was_made_for() {
    let app = router(state_with(false, RegistrationMode::Open).await);
    let alice = register_and_sign_in(&app, "alice").await;
    create_repo(&app, &alice, "one").await;
    create_repo(&app, &alice, "two").await;

    let r = send(
        &app,
        "POST",
        "/v1/tokens",
        &alice,
        Some(serde_json::json!({"name": "ci", "repos": ["alice/one"], "permissions": ["read"]})),
    )
    .await;
    assert_eq!(r.status(), StatusCode::OK);
    let made: api::CreatedToken = body_json(r).await;
    assert_eq!(made.info.repos, ["alice/one"]);

    for (repo, expected) in [("one", StatusCode::OK), ("two", StatusCode::NOT_FOUND)] {
        let uri = format!("/v1/repos/alice/{repo}/files");
        assert_eq!(
            status(&app, with_token("GET", &uri, &made.token, None)).await,
            expected
        );
    }
    let none = send(
        &app,
        "POST",
        "/v1/tokens",
        &alice,
        Some(serde_json::json!({"name": "ci", "repos": [], "permissions": ["read"]})),
    )
    .await;
    assert_eq!(none.status(), StatusCode::BAD_REQUEST);
    let other = send(
        &app,
        "POST",
        "/v1/tokens",
        &alice,
        Some(serde_json::json!({"name": "ci", "repos": ["bob/nope"], "permissions": ["read"]})),
    )
    .await;
    assert_eq!(other.status(), StatusCode::NOT_FOUND);

    let denied = send(
        &app,
        "POST",
        "/v1/repos",
        &made.token,
        Some(serde_json::json!({"name": "three"})),
    )
    .await;
    assert_eq!(
        denied.status(),
        StatusCode::FORBIDDEN,
        "a limited token cannot create repositories"
    );

    let signed_in = send(&app, "GET", "/v1/tokens", &alice, None).await;
    let tokens: Vec<api::TokenInfo> = body_json(signed_in).await;
    assert!(
        tokens
            .iter()
            .any(|t| t.name == "sign-in" && t.repos.is_empty())
    );
}

#[tokio::test]
async fn the_owner_renames_reconfigures_and_deletes_a_repository() {
    let app = router(state_with(false, RegistrationMode::Open).await);
    let alice = register_and_sign_in(&app, "alice").await;
    create_repo(&app, &alice, "game").await;
    let put = Request::put("/v1/repos/alice/game/objects")
        .header("authorization", format!("Bearer {alice}"))
        .body(Body::from("data"))
        .unwrap();
    assert_eq!(status(&app, put).await, StatusCode::OK);
    send(
        &app,
        "POST",
        "/v1/repos/alice/game/checkout",
        &alice,
        Some(serde_json::json!({"path": "Content/a.bin", "base_revision": null})),
    )
    .await;

    let r = send(
        &app,
        "PATCH",
        "/v1/repos/alice/game",
        &alice,
        Some(serde_json::json!({"name": "engine", "visibility": "public", "lease_hours": 2})),
    )
    .await;
    assert_eq!(r.status(), StatusCode::OK);
    let info: api::RepoInfo = body_json(r).await;
    assert_eq!(
        (info.name.as_str(), info.visibility, info.lease_hours),
        ("engine", api::Visibility::Public, 2)
    );
    assert_eq!(
        send(&app, "GET", "/v1/repos/alice/game", &alice, None)
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    let locks: Vec<api::Lock> =
        body_json(send(&app, "GET", "/v1/repos/alice/engine/locks", &alice, None).await).await;
    assert_eq!(locks.len(), 1, "a rename keeps the repository's data");

    let weird = send(
        &app,
        "PATCH",
        "/v1/repos/alice/engine",
        &alice,
        Some(serde_json::json!({"lease_hours": 0})),
    )
    .await;
    assert_eq!(weird.status(), StatusCode::BAD_REQUEST);

    assert_eq!(
        send(&app, "DELETE", "/v1/repos/alice/engine", &alice, None)
            .await
            .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        send(&app, "GET", "/v1/repos/alice/engine/files", &alice, None)
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    let again = create_repo(&app, &alice, "engine").await;
    let locks: Vec<api::Lock> =
        body_json(send(&app, "GET", "/v1/repos/alice/engine/locks", &alice, None).await).await;
    assert!(locks.is_empty() && again.role.as_deref() == Some("admin"));
}

#[tokio::test]
async fn the_single_repository_routes_are_gone() {
    let app = app().await;
    for uri in ["/v1/files", "/v1/locks", "/v1/roles", "/v1/members"] {
        let r = app
            .clone()
            .oneshot(
                Request::get(uri)
                    .header(api::DEV_USER_HEADER, "alice")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::NOT_FOUND, "{uri}");
    }
}

#[tokio::test]
async fn an_invitation_admits_a_new_account_to_its_repository_only() {
    let st = state(false).await;
    let admin = admin_token(&st).await;
    let app = router(st);
    let made: api::CreatedInvite = body_json(
        send(
            &app,
            "POST",
            "/v1/repos/owner/game/invites",
            &admin,
            Some(serde_json::json!({"role": "writer", "hours": 24})),
        )
        .await,
    )
    .await;
    let r = app
        .clone()
        .oneshot(anon(
            "POST",
            "/v1/register",
            serde_json::json!({"username": "newbie", "password": PASSWORD, "invite": made.code}),
        ))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::CREATED);
    let r = app
        .clone()
        .oneshot(anon(
            "POST",
            "/v1/login",
            serde_json::json!({"username": "newbie", "password": PASSWORD}),
        ))
        .await
        .unwrap();
    let token = body_json::<api::CreatedToken>(r).await.token;
    let me: api::Me =
        body_json(send(&app, "GET", "/v1/repos/owner/game/me", &token, None).await).await;
    assert!(me.permissions.contains(&"checkin".to_string()));
}
