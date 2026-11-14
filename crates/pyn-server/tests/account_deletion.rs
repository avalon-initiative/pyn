//! The HTTP contract of account deletion: self-service with a recovery window, the restricted pending state, and
//! administrator deletion.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore,
};
use pyn_core::{
    AccessConfig, AccessService, AuthProvider, ManualClock, RegistrationMode, Repositories, Rules,
};
use pyn_proto as api;
use pyn_server::auth::{BearerAuth, DevHeaderAuth};
use pyn_server::{AppState, router};
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";

struct Server {
    app: Router,
    access: Arc<AccessService>,
    clock: Arc<ManualClock>,
}

fn server() -> Server {
    let clock = Arc::new(ManualClock::new(chrono::Utc::now()));
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
            registration: RegistrationMode::Open,
            require_email_verification: false,
            public_url: "https://pyn.example".into(),
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
        clock.clone(),
        Rules::empty(),
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
    });
    Server { app, access, clock }
}

struct Reply {
    status: StatusCode,
    body: Vec<u8>,
}

impl Reply {
    fn json<T: serde::de::DeserializeOwned>(&self) -> T {
        serde_json::from_slice(&self.body).unwrap_or_else(|e| {
            panic!("{e}: {}", String::from_utf8_lossy(&self.body));
        })
    }

    fn code(&self) -> String {
        self.json::<api::ErrorBody>().code
    }

    fn message(&self) -> String {
        self.json::<api::ErrorBody>().message
    }
}

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    auth: Option<(&str, &str)>,
    body: Option<serde_json::Value>,
) -> Reply {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    match auth {
        Some(("bearer", token)) => req = req.header("authorization", format!("Bearer {token}")),
        Some((_, user)) => req = req.header(api::DEV_USER_HEADER, user),
        None => {}
    }
    let body = body.map(|b| b.to_string()).unwrap_or_default();
    let r = app
        .clone()
        .oneshot(req.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    Reply {
        status: r.status(),
        body: r.into_body().collect().await.unwrap().to_bytes().to_vec(),
    }
}

async fn sign_up(app: &Router, name: &str) {
    let r = call(
        app,
        "POST",
        "/v1/register",
        None,
        Some(serde_json::json!({"username": name, "password": PASSWORD})),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
}

async fn login(app: &Router, name: &str) -> Reply {
    call(
        app,
        "POST",
        "/v1/login",
        None,
        Some(serde_json::json!({"username": name, "password": PASSWORD})),
    )
    .await
}

async fn token(app: &Router, name: &str) -> String {
    let r = login(app, name).await;
    assert_eq!(r.status, StatusCode::OK);
    r.json::<api::CreatedToken>().token
}

#[tokio::test]
async fn self_service_deletion_disables_the_account_and_restore_brings_it_back() {
    let s = server();
    sign_up(&s.app, "alice").await;
    let alice = token(&s.app, "alice").await;

    let none = call(
        &s.app,
        "GET",
        "/v1/account/deletion",
        Some(("bearer", &alice)),
        None,
    )
    .await;
    assert_eq!(none.status, StatusCode::OK);
    let none: api::AccountDeletionStatus = none.json();
    assert!(!none.pending && none.purge_at.is_none() && none.scheduled_at.is_none());

    let wrong = call(
        &s.app,
        "POST",
        "/v1/account/deletion",
        Some(("bearer", &alice)),
        Some(serde_json::json!({"password": "not it"})),
    )
    .await;
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);

    let done = call(
        &s.app,
        "POST",
        "/v1/account/deletion",
        Some(("bearer", &alice)),
        Some(serde_json::json!({"password": PASSWORD})),
    )
    .await;
    assert_eq!(
        done.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&done.body)
    );
    let status: api::AccountDeletionStatus = done.json();
    assert!(status.pending);
    assert_eq!(
        status.purge_at.unwrap() - status.scheduled_at.unwrap(),
        chrono::Duration::days(30)
    );

    // Pending: status and restore answer; everything else is refused with a clear code.
    let again = call(
        &s.app,
        "GET",
        "/v1/account/deletion",
        Some(("bearer", &alice)),
        None,
    )
    .await;
    assert!(again.json::<api::AccountDeletionStatus>().pending);
    for (method, uri) in [("GET", "/v1/me"), ("GET", "/v1/repos"), ("GET", "/v1/keys")] {
        let r = call(&s.app, method, uri, Some(("bearer", &alice)), None).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{uri}");
        assert_eq!(r.code(), "account_pending_deletion", "{uri}");
        assert!(r.message().contains("restore"), "{}", r.message());
    }

    // Signing in during the window still works, and that sign-in restores.
    let again = token(&s.app, "alice").await;
    let restored = call(
        &s.app,
        "POST",
        "/v1/account/deletion/restore",
        Some(("bearer", &again)),
        None,
    )
    .await;
    assert_eq!(restored.status, StatusCode::OK);
    assert!(!restored.json::<api::AccountDeletionStatus>().pending);
    let me = call(&s.app, "GET", "/v1/me", Some(("bearer", &again)), None).await;
    assert_eq!(me.status, StatusCode::OK);
    let nothing = call(
        &s.app,
        "POST",
        "/v1/account/deletion/restore",
        Some(("bearer", &again)),
        None,
    )
    .await;
    assert_eq!(nothing.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn the_server_task_deletes_the_account_when_the_window_ends() {
    let s = server();
    sign_up(&s.app, "alice").await;
    let alice = token(&s.app, "alice").await;
    call(
        &s.app,
        "POST",
        "/v1/account/deletion",
        Some(("bearer", &alice)),
        Some(serde_json::json!({"password": PASSWORD})),
    )
    .await;
    s.clock.advance(chrono::Duration::days(31));
    pyn_server::account_deletion::run_once(&s.access).await;
    assert_eq!(
        login(&s.app, "alice").await.status,
        StatusCode::UNAUTHORIZED
    );
    let register = call(
        &s.app,
        "POST",
        "/v1/register",
        None,
        Some(serde_json::json!({"username": "alice", "password": PASSWORD})),
    )
    .await;
    assert_eq!(register.status, StatusCode::CONFLICT);
    assert_eq!(register.code(), "name_reserved");
}

#[tokio::test]
async fn an_account_owning_a_repository_names_it_in_the_refusal() {
    let s = server();
    sign_up(&s.app, "alice").await;
    let alice = token(&s.app, "alice").await;
    let repo = call(
        &s.app,
        "POST",
        "/v1/repos",
        Some(("bearer", &alice)),
        Some(serde_json::json!({"name": "game"})),
    )
    .await;
    assert_eq!(
        repo.status,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&repo.body)
    );
    let r = call(
        &s.app,
        "POST",
        "/v1/account/deletion",
        Some(("bearer", &alice)),
        Some(serde_json::json!({"password": PASSWORD})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.code(), "account_owns_resources");
    assert!(r.message().contains("alice/game"), "{}", r.message());
}

#[tokio::test]
async fn an_administrator_deletes_a_disabled_account_with_a_reason() {
    let s = server();
    sign_up(&s.app, "alice").await;
    sign_up(&s.app, "bob").await;
    let bob = token(&s.app, "bob").await;
    let delete = |who: &'static str, body: serde_json::Value| {
        let app = s.app.clone();
        async move {
            call(
                &app,
                "POST",
                "/v1/admin/users/alice/delete",
                Some(("dev", who)),
                Some(body),
            )
            .await
        }
    };

    let live = delete("root", serde_json::json!({"reason": "spam"})).await;
    assert_eq!(live.status, StatusCode::CONFLICT);
    assert_eq!(live.code(), "account_not_disabled");

    let disabled = call(
        &s.app,
        "POST",
        "/v1/admin/users/alice/disable",
        Some(("dev", "root")),
        Some(serde_json::json!({})),
    )
    .await;
    assert_eq!(disabled.status, StatusCode::OK);

    let no_reason = delete("root", serde_json::json!({"reason": " "})).await;
    assert_eq!(no_reason.status, StatusCode::BAD_REQUEST);
    let denied = call(
        &s.app,
        "POST",
        "/v1/admin/users/alice/delete",
        Some(("bearer", &bob)),
        Some(serde_json::json!({"reason": "x"})),
    )
    .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);

    let scheduled = delete("root", serde_json::json!({"reason": "inactive"})).await;
    assert_eq!(scheduled.status, StatusCode::OK);
    let scheduled: api::AdminDeleteAccountResult = scheduled.json();
    assert!(!scheduled.deleted && scheduled.purge_at.is_some());

    let gone = delete(
        "root",
        serde_json::json!({"reason": "inactive", "immediately": true}),
    )
    .await;
    assert_eq!(gone.status, StatusCode::OK);
    let gone: api::AdminDeleteAccountResult = gone.json();
    assert!(gone.deleted && gone.purge_at.is_none());

    let users = call(
        &s.app,
        "GET",
        "/v1/admin/users",
        Some(("dev", "root")),
        None,
    )
    .await;
    let names: Vec<String> = users
        .json::<Vec<api::AccountInfo>>()
        .into_iter()
        .map(|a| a.user)
        .collect();
    assert_eq!(names, ["bob"]);
    let audit = call(
        &s.app,
        "GET",
        "/v1/admin/audit",
        Some(("dev", "root")),
        None,
    )
    .await;
    let actions: Vec<String> = audit
        .json::<api::AuditPage>()
        .entries
        .into_iter()
        .map(|e| e.action)
        .collect();
    assert!(
        actions.contains(&"account_deleted".to_string()),
        "{actions:?}"
    );
    assert!(actions.contains(&"account_deletion_scheduled".to_string()));
}
