//! Creating an invitation is server-wide account creation, so it needs a full-scope credential.

#[path = "common/cred.rs"]
mod cred;
use cred::{Cred, WithCred};
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore,
};
use pyn_core::{
    AccessConfig, AccessService, AuthProvider, Permission, RegistrationMode, Repositories, Rules,
    SystemClock, UserId,
};
use pyn_proto as api;
use pyn_server::auth::{BearerAuth, DevHeaderAuth};
use pyn_server::{AppState, router};
use serde_json::{Value, json};
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";
const ALL: [&str; 9] = [
    "read",
    "lock",
    "checkin",
    "restore",
    "force_unlock",
    "edit_policy",
    "manage_users",
    "manage_roles",
    "view_audit",
];
const ORIGIN: &str = "http://localhost:5173";

async fn server(mode: RegistrationMode, require_approval: bool) -> (Router, Arc<AccessService>) {
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
        .with_config(AccessConfig {
            registration: mode,
            require_approval,
            require_email_verification: false,
            allowed_origins: vec![ORIGIN.into()],
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
        auth: Arc::new(BearerAuth {
            access: access.clone(),
        }),
        dev_auth: Some(Arc::new(DevHeaderAuth) as Arc<dyn AuthProvider>),
        trust_forwarded: false,
        updates: None,
        storage: Default::default(),
        ready: Default::default(),
    });
    (app, access)
}

struct Reply {
    status: StatusCode,
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

async fn send(app: &Router, who: &Cred, method: &str, uri: &str, body: Option<Value>) -> Reply {
    let mut b = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    b = b.cred(who);
    let req = b
        .body(Body::from(body.map(|v| v.to_string()).unwrap_or_default()))
        .unwrap();
    let r = app.clone().oneshot(req).await.unwrap();
    Reply {
        status: r.status(),
        body: r.into_body().collect().await.unwrap().to_bytes().to_vec(),
    }
}

async fn bearer(app: &Router, name: &str) -> Cred {
    let body = json!({"username": name, "password": PASSWORD});
    let r = send(app, &Cred::dev("root"), "POST", "/v1/login", Some(body)).await;
    assert_eq!(r.status, StatusCode::OK);
    Cred::token(&r.json::<api::CreatedToken>().token)
}

async fn session(app: &Router, name: &str) -> Cred {
    let req = Request::builder()
        .method("POST")
        .uri("/v1/session")
        .header("content-type", "application/json")
        .header("origin", ORIGIN)
        .body(Body::from(
            json!({"username": name, "password": PASSWORD}).to_string(),
        ))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let set = res.headers()["set-cookie"].to_str().unwrap().to_string();
    let cookie = set
        .split(';')
        .next()
        .and_then(|kv| kv.split_once('='))
        .map(|(_, v)| v.to_string())
        .unwrap();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let info: api::SessionInfo = serde_json::from_slice(&bytes).unwrap();
    Cred::session(&cookie, &info.csrf_token)
}

/// A fine-grained token of `user` for every repository of `root`, minted without the creation checks.
async fn scoped(access: &AccessService, user: &str, permissions: &[&str]) -> Cred {
    let permissions: Vec<Permission> = permissions.iter().map(|p| p.parse().unwrap()).collect();
    let secret = access
        .token_for_tests(
            &UserId::new(user),
            &UserId::new("root"),
            permissions,
            Vec::new(),
        )
        .await
        .unwrap();
    Cred::token(&secret)
}

async fn who_am_i(app: &Router, who: &Cred) -> String {
    let r = send(app, who, "GET", "/v1/repos/root/seed/me", None).await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    r.json::<api::Me>().user
}

async fn invites(app: &Router) -> usize {
    let r = send(
        app,
        &Cred::dev("root"),
        "GET",
        "/v1/repos/root/seed/invites",
        None,
    )
    .await;
    r.json::<Vec<api::InviteInfo>>().len()
}

async fn invite(app: &Router, who: &Cred) -> Reply {
    let body = json!({"role": "reader", "hours": 24});
    send(app, who, "POST", "/v1/repos/root/seed/invites", Some(body)).await
}

struct Creds {
    session: Cred,
    full_token: Cred,
    wendy_scoped: Cred,
    wendy_narrow: Cred,
    boss_scoped: Cred,
    boss_token: Cred,
    service: Cred,
}

async fn setup(app: &Router, access: &AccessService) -> Creds {
    let root = Cred::dev("root");
    let r = send(
        app,
        &root,
        "POST",
        "/v1/repos",
        Some(json!({"name": "seed"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED);
    for name in ["wendy", "boss"] {
        let body = json!({"username": name, "password": PASSWORD, "role": "admin"});
        let r = send(app, &root, "POST", "/v1/repos/root/seed/users", Some(body)).await;
        assert_eq!(r.status, StatusCode::CREATED, "{name}");
    }
    let r = send(app, &root, "PUT", "/v1/admin/users/boss/admin", None).await;
    assert!(r.status.is_success());
    let (wendy, boss) = (bearer(app, "wendy").await, bearer(app, "boss").await);
    let minted = send(
        app,
        &root,
        "POST",
        "/v1/admin/service-credentials",
        Some(json!({"name": "svc", "scopes": ["manage_accounts"]})),
    )
    .await;
    Creds {
        session: session(app, "wendy").await,
        wendy_scoped: scoped(access, "wendy", &ALL).await,
        wendy_narrow: scoped(access, "wendy", &["read", "manage_users"]).await,
        boss_scoped: scoped(access, "boss", &ALL).await,
        full_token: wendy,
        boss_token: boss,
        service: Cred::service(&minted.json::<api::CreatedServiceCredential>().secret),
    }
}

#[tokio::test]
async fn creating_an_invitation_needs_a_full_scope_credential() {
    for (mode, approval) in [
        (RegistrationMode::Open, false),
        (RegistrationMode::Closed, false),
        (RegistrationMode::Open, true),
    ] {
        let (app, access) = server(mode, approval).await;
        let c = setup(&app, &access).await;
        let label = format!("{mode:?} approval={approval}");

        assert_eq!(who_am_i(&app, &c.session).await, "wendy");
        assert_eq!(who_am_i(&app, &c.full_token).await, "wendy");
        assert_eq!(who_am_i(&app, &c.wendy_scoped).await, "wendy");
        assert_eq!(who_am_i(&app, &c.wendy_narrow).await, "wendy");
        assert_eq!(who_am_i(&app, &c.boss_scoped).await, "boss");
        assert_eq!(who_am_i(&app, &c.boss_token).await, "boss");
        let r = send(&app, &c.service, "GET", "/v1/repos/root/seed/me", None).await;
        assert_eq!(r.code(), "service_credential_not_allowed", "{label}");

        let r = invite(&app, &Cred::SignedOut).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{label}: signed out");
        let refused = [
            (
                "scoped token of a normal user",
                &c.wendy_scoped,
                "forbidden",
            ),
            (
                "scoped token of an administrator",
                &c.boss_scoped,
                "forbidden",
            ),
            (
                "service credential",
                &c.service,
                "service_credential_not_allowed",
            ),
        ];
        for (what, who, code) in refused {
            let r = invite(&app, who).await;
            assert_eq!(r.status, StatusCode::FORBIDDEN, "{label} {what}");
            assert_eq!(r.code(), code, "{label} {what}");
        }
        assert_eq!(invites(&app).await, 0, "{label}: a refused call minted one");

        for (what, who) in [
            ("session with manage_users", &c.session),
            ("full-scope token", &c.full_token),
            ("administrator full-scope token", &c.boss_token),
            ("dev token", &Cred::dev("root")),
        ] {
            let r = invite(&app, who).await;
            assert_eq!(r.status, StatusCode::OK, "{label} {what}");
            assert!(!r.json::<api::CreatedInvite>().code.is_empty());
        }
        assert_eq!(invites(&app).await, 4, "{label}");
    }
}

#[tokio::test]
async fn an_invitation_made_by_a_full_scope_member_still_registers_on_a_closed_server() {
    let (app, access) = server(RegistrationMode::InviteOnly, false).await;
    let c = setup(&app, &access).await;
    let r = invite(&app, &c.session).await;
    let code = r.json::<api::CreatedInvite>().code;
    let body = json!({"username": "newbie", "password": PASSWORD, "invite": code});
    let r = send(&app, &Cred::SignedOut, "POST", "/v1/register", Some(body)).await;
    assert_eq!(
        r.status,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
}
