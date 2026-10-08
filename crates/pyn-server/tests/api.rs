use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore,
};
use pyn_core::{
    AccessConfig, AccessService, AuthProvider, RegistrationMode, RepoId, RepoService, Rules,
    ServiceConfig, SystemClock, UserId,
};
use pyn_proto as api;
use pyn_server::auth::{BearerAuth, DevHeaderAuth};
use pyn_server::{AppState, router};
use tower::ServiceExt;

const RULES: &str = "[meta]\ndefault = \"shared\"\n[exclusive]\npaths = [\"Content/\"]\n";

fn state(dev: bool) -> AppState {
    state_with(dev, RegistrationMode::InviteOnly)
}

fn state_with(dev: bool, mode: RegistrationMode) -> AppState {
    let repo = RepoId::new("t");
    let clock = Arc::new(SystemClock);
    let objects = Arc::new(MemoryObjectStore::new());
    let service = Arc::new(RepoService::new(
        repo.clone(),
        Rules::from_toml(RULES).unwrap(),
        Arc::new(MemoryMetadataStore::new()),
        objects.clone(),
        Arc::new(MemoryAuditStore::new()),
        clock.clone(),
        ServiceConfig::default(),
    ));
    let access = Arc::new(
        AccessService::new(Arc::new(MemoryAccessStore::new()), clock).with_config(AccessConfig {
            registration: mode,
            ..AccessConfig::default()
        }),
    );
    AppState {
        service,
        objects,
        access: access.clone(),
        auth: Arc::new(BearerAuth { access, repo }),
        dev_auth: dev.then(|| Arc::new(DevHeaderAuth) as Arc<dyn AuthProvider>),
    }
}

/// An app with development auth on, as `make run` uses it.
fn app() -> axum::Router {
    router(state(true))
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
    let app = app();
    let body = serde_json::json!({"path": "Content/a.umap", "base_revision": null});

    let r = app
        .clone()
        .oneshot(json_post("/v1/checkout", "alice", body.clone()))
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);

    let r = app
        .oneshot(json_post("/v1/checkout", "bob", body))
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
        .oneshot(
            Request::post("/v1/checkout")
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
        .oneshot(Request::get("/openapi.json").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let doc: serde_json::Value =
        serde_json::from_slice(&r.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert!(doc["paths"]["/v1/checkout"].is_object());
    assert!(doc["components"]["schemas"]["ErrorBody"].is_object());
}

#[tokio::test]
async fn files_listing_shows_mode_and_lock() {
    let app = app();
    let body = serde_json::json!({"path": "Content/a.umap", "base_revision": null});
    app.clone()
        .oneshot(json_post("/v1/checkout", "alice", body))
        .await
        .unwrap();

    let r = app
        .oneshot(
            Request::get("/v1/files")
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

#[tokio::test]
async fn any_revision_can_be_fetched() {
    let app = app();
    let mut base = serde_json::Value::Null;
    for text in ["v1", "v2"] {
        let put = Request::put("/v1/objects")
            .header(api::DEV_USER_HEADER, "bob")
            .body(Body::from(text))
            .unwrap();
        let r = app.clone().oneshot(put).await.unwrap();
        let obj: api::PutObjectResponse =
            serde_json::from_slice(&r.into_body().collect().await.unwrap().to_bytes()).unwrap();
        let body = serde_json::json!({"path": "Source/a.cpp", "content": obj.content, "base_revision": base, "message": text});
        let r = app
            .clone()
            .oneshot(json_post("/v1/checkin", "bob", body))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        base = serde_json::json!(base.as_u64().unwrap_or(0) + 1);
    }

    let fetch = |query: &'static str| {
        let app = app.clone();
        async move {
            app.oneshot(
                Request::get(format!("/v1/content?{query}"))
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
    state
        .access
        .bootstrap_admin(state.service.repo(), &UserId::new("root"))
        .await
        .unwrap()
}

#[tokio::test]
async fn the_dev_header_is_ignored_unless_dev_auth_is_on() {
    let app = router(state(false));
    let req = Request::get("/v1/files")
        .header(api::DEV_USER_HEADER, "alice")
        .body(Body::empty())
        .unwrap();
    assert_eq!(status(&app, req).await, StatusCode::UNAUTHORIZED);
    let bare = Request::get("/v1/files").body(Body::empty()).unwrap();
    assert_eq!(status(&app, bare).await, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn health_and_openapi_need_no_credentials() {
    let app = router(state(false));
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
    let st = state(false);
    let admin = admin_token(&st).await;
    let app = router(st);

    let made: api::CreatedToken = body_json(
        app.clone()
            .oneshot(with_token(
                "POST",
                "/v1/tokens",
                &admin,
                Some(serde_json::json!({"name": "ci", "permissions": ["read"]})),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert!(made.token.starts_with("pyn_"));
    assert_eq!(made.info.permissions, ["read"]);

    assert_eq!(
        status(&app, with_token("GET", "/v1/files", &made.token, None)).await,
        StatusCode::OK
    );
    let checkout = serde_json::json!({"path": "Content/a.umap", "base_revision": null});
    let r = app
        .clone()
        .oneshot(with_token(
            "POST",
            "/v1/checkout",
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
    let st = state(false);
    let admin = admin_token(&st).await;
    st.access
        .set_user_role(
            &st.access
                .principal(st.service.repo(), &UserId::new("root"))
                .await
                .unwrap(),
            st.service.repo(),
            &UserId::new("wendy"),
            pyn_core::Role::Writer,
        )
        .await
        .unwrap();
    let wendy = st
        .access
        .principal(st.service.repo(), &UserId::new("wendy"))
        .await
        .unwrap();
    let (_, wendy_token) = st
        .access
        .create_token(&wendy, "w", wendy.permissions.clone(), vec![], None)
        .await
        .unwrap();
    let app = router(st);
    let _ = admin;

    let body = serde_json::json!({"name": "sneaky", "permissions": ["read", "restore"]});
    assert_eq!(
        status(
            &app,
            with_token("POST", "/v1/tokens", &wendy_token, Some(body))
        )
        .await,
        StatusCode::FORBIDDEN
    );
    let body = serde_json::json!({"name": "nope", "permissions": ["flying"]});
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
    let st = state(false);
    let admin = admin_token(&st).await;
    let app = router(st);

    let me: api::Me = body_json(
        app.clone()
            .oneshot(with_token("GET", "/v1/me", &admin, None))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(me.user, "root");
    assert_eq!(me.permissions.len(), 9);

    let made: api::CreatedToken = body_json(
        app.clone()
            .oneshot(with_token(
                "POST",
                "/v1/tokens",
                &admin,
                Some(serde_json::json!({"name": "tmp", "permissions": ["read"]})),
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
    let st = state(false);
    let admin = admin_token(&st).await;
    let app = router(st);

    let add = serde_json::json!({"role": "writer"});
    assert_eq!(
        status(
            &app,
            with_token("PUT", "/v1/members/wendy", &admin, Some(add))
        )
        .await,
        StatusCode::NO_CONTENT
    );
    let members: Vec<api::Member> = body_json(
        app.clone()
            .oneshot(with_token("GET", "/v1/members", &admin, None))
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
            .oneshot(with_token("GET", "/v1/roles", &admin, None))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(grants.len(), 4);

    let narrow = serde_json::json!({"permissions": ["read"]});
    assert_eq!(
        status(
            &app,
            with_token("PUT", "/v1/roles/writer", &admin, Some(narrow))
        )
        .await,
        StatusCode::NO_CONTENT
    );
    let lock_out = serde_json::json!({"permissions": ["read"]});
    assert_eq!(
        status(
            &app,
            with_token("PUT", "/v1/roles/admin", &admin, Some(lock_out))
        )
        .await,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn restore_needs_the_permission_the_lock_and_the_confirmation() {
    let st = state(false);
    let admin = admin_token(&st).await;
    let root = st
        .access
        .principal(st.service.repo(), &UserId::new("root"))
        .await
        .unwrap();
    st.access
        .set_user_role(
            &root,
            st.service.repo(),
            &UserId::new("wendy"),
            pyn_core::Role::Writer,
        )
        .await
        .unwrap();
    let wendy = st
        .access
        .principal(st.service.repo(), &UserId::new("wendy"))
        .await
        .unwrap();
    let (_, writer) = st
        .access
        .create_token(&wendy, "w", wendy.permissions.clone(), vec![], None)
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
                "/v1/checkout",
                &admin,
                Some(serde_json::json!({"path": "Content/m.umap", "base_revision": base})),
            ))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        let put = Request::put("/v1/objects")
            .header("authorization", format!("Bearer {admin}"))
            .body(Body::from(text))
            .unwrap();
        let obj: api::PutObjectResponse = body_json(app.clone().oneshot(put).await.unwrap()).await;
        let checkin = serde_json::json!({"path": "Content/m.umap", "content": obj.content, "base_revision": base, "message": text});
        assert_eq!(
            status(
                &app,
                with_token("POST", "/v1/checkin", &admin, Some(checkin))
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
            "/v1/restore",
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
            "/v1/restore",
            &admin,
            Some(restore("Content/m.umap@r2")),
        ))
        .await
        .unwrap();
    let err: api::ErrorBody = body_json(r).await;
    assert_eq!(err.code, "lock_required");

    let lock = serde_json::json!({"path": "Content/m.umap", "base_revision": 2});
    assert_eq!(
        status(&app, with_token("POST", "/v1/checkout", &admin, Some(lock))).await,
        StatusCode::OK
    );
    let r = app
        .clone()
        .oneshot(with_token(
            "POST",
            "/v1/restore",
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
            "/v1/restore",
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
        .principal(st.service.repo(), &UserId::new("root"))
        .await
        .unwrap();
    st.access
        .set_user_role(&root, st.service.repo(), &UserId::new(name), role)
        .await
        .unwrap();
    let who = st
        .access
        .principal(st.service.repo(), &UserId::new(name))
        .await
        .unwrap();
    st.access
        .create_token(&who, name, who.permissions.clone(), vec![], None)
        .await
        .unwrap()
        .1
}

#[tokio::test]
async fn force_unlock_needs_the_permission_and_a_reason_and_is_audited() {
    let st = state(false);
    admin_token(&st).await;
    let wendy = member_token(&st, "wendy", pyn_core::Role::Writer).await;
    let maya = member_token(&st, "maya", pyn_core::Role::Maintainer).await;
    let app = router(st);

    let lock = serde_json::json!({"path": "Content/a.umap", "base_revision": null});
    assert_eq!(
        status(&app, with_token("POST", "/v1/checkout", &wendy, Some(lock))).await,
        StatusCode::OK
    );

    let unlock = |reason: &str| serde_json::json!({"path": "Content/a.umap", "reason": reason});
    let r = app
        .clone()
        .oneshot(with_token(
            "POST",
            "/v1/force-unlock",
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
            with_token("POST", "/v1/force-unlock", &maya, Some(unlock("  ")))
        )
        .await,
        StatusCode::BAD_REQUEST
    );

    let r = app
        .clone()
        .oneshot(with_token(
            "POST",
            "/v1/force-unlock",
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
            "/v1/force-unlock",
            &maya,
            Some(unlock("again")),
        ))
        .await
        .unwrap();
    let err: api::ErrorBody = body_json(r).await;
    assert_eq!(err.code, "not_locked");

    assert_eq!(
        status(&app, with_token("GET", "/v1/audit", &wendy, None)).await,
        StatusCode::FORBIDDEN
    );
    let page: api::AuditPage = body_json(
        app.clone()
            .oneshot(with_token("GET", "/v1/audit", &maya, None))
            .await
            .unwrap(),
    )
    .await;
    let actions: Vec<_> = page
        .entries
        .iter()
        .map(|e| (e.action.as_str(), e.actor.as_str()))
        .collect();
    assert_eq!(actions, [("force_unlock", "maya"), ("checkout", "wendy")]);
    assert!(page.entries[0].detail.contains("wendy is away"));

    let only: api::AuditPage = body_json(
        app.clone()
            .oneshot(with_token(
                "GET",
                "/v1/audit?action=checkout&limit=1",
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
            with_token("GET", "/v1/audit?action=bogus", &maya, None)
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
    let closed = router(state_with(false, RegistrationMode::Closed));
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

    let open = router(state_with(false, RegistrationMode::Open));
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
    let me: api::Me = body_json(
        open.oneshot(with_token("GET", "/v1/me", &session.token, None))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        (me.user.as_str(), me.permissions.as_slice()),
        ("alice", ["read".to_string()].as_slice())
    );
}

#[tokio::test]
async fn an_invite_only_server_admits_people_with_a_one_time_invitation() {
    let st = state(false);
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
            .oneshot(with_token("POST", "/v1/invites", &admin, Some(make)))
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
            .oneshot(with_token("GET", "/v1/invites", &admin, None))
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
                "/v1/invites",
                &wendy.token,
                Some(serde_json::json!({"role": "reader", "hours": 1}))
            )
        )
        .await,
        StatusCode::FORBIDDEN
    );

    let uri = format!("/v1/invites/{}", listed[0].id);
    assert_eq!(
        status(&app, with_token("DELETE", &uri, &admin, None)).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        status(
            &app,
            with_token("DELETE", "/v1/invites/ffffffffffff", &admin, None)
        )
        .await,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn sign_in_is_throttled_and_never_says_which_part_was_wrong() {
    let st = state(false);
    let admin = admin_token(&st).await;
    let app = router(st);
    let add = serde_json::json!({"username": "alice", "password": PASSWORD, "role": "writer"});
    assert_eq!(
        status(
            &app,
            with_token("POST", "/v1/users", &admin, Some(add.clone()))
        )
        .await,
        StatusCode::CREATED
    );
    assert_eq!(
        status(&app, with_token("POST", "/v1/users", &admin, Some(add))).await,
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
    let st = state_with(false, RegistrationMode::Open);
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
    let st = state_with(false, RegistrationMode::Open);
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
