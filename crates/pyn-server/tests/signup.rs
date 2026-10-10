//! The HTTP contract of open-registration protection: verification, approval, rate limits, account administration.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore,
};
use pyn_core::{
    AccessConfig, AccessService, AuthProvider, MemoryEmailSender, RateLimits, RegistrationMode,
    Repositories, Rules, SystemClock,
};
use pyn_proto as api;
use pyn_server::auth::{BearerAuth, DevHeaderAuth};
use pyn_server::{AppState, router};
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";

struct Server {
    app: Router,
    mail: Arc<MemoryEmailSender>,
}

fn server_with(trust_forwarded: bool, config: impl FnOnce(&mut AccessConfig)) -> Server {
    let clock = Arc::new(SystemClock);
    let objects = Arc::new(MemoryObjectStore::new());
    let audit = Arc::new(MemoryAuditStore::new());
    let mail = Arc::new(MemoryEmailSender::new());
    let mut cfg = AccessConfig {
        registration: RegistrationMode::Open,
        public_url: "https://pyn.example".into(),
        ..AccessConfig::default()
    };
    config(&mut cfg);
    let access = Arc::new(
        AccessService::new(Arc::new(MemoryAccessStore::new()), clock.clone())
            .with_config(cfg)
            .with_email(mail.clone())
            .with_audit(audit.clone()),
    );
    let repos = Arc::new(Repositories::new(
        Arc::new(MemoryMetadataStore::new()),
        objects.clone(),
        audit,
        access.clone(),
        clock,
        Rules::empty(),
    ));
    let app = router(AppState {
        repos,
        objects,
        access: access.clone(),
        auth: Arc::new(BearerAuth { access }),
        dev_auth: Some(Arc::new(DevHeaderAuth) as Arc<dyn AuthProvider>),
        trust_forwarded,
    });
    Server { app, mail }
}

fn server() -> Server {
    server_with(true, |_| {})
}

struct Reply {
    status: StatusCode,
    retry_after: Option<String>,
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
}

async fn send(app: &Router, req: Request<Body>) -> Reply {
    let r = app.clone().oneshot(req).await.unwrap();
    Reply {
        status: r.status(),
        retry_after: r
            .headers()
            .get("retry-after")
            .map(|v| v.to_str().unwrap().to_string()),
        body: r.into_body().collect().await.unwrap().to_bytes().to_vec(),
    }
}

fn post(uri: &str, body: serde_json::Value, from: Option<&str>) -> Request<Body> {
    let mut req = Request::post(uri).header("content-type", "application/json");
    if let Some(ip) = from {
        req = req.header("x-forwarded-for", ip);
    }
    req.body(Body::from(body.to_string())).unwrap()
}

fn as_admin(method: &str, uri: &str, body: Option<serde_json::Value>) -> Request<Body> {
    as_user("root", method, uri, body)
}

fn as_user(user: &str, method: &str, uri: &str, body: Option<serde_json::Value>) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header(api::DEV_USER_HEADER, user)
        .body(Body::from(body.map(|b| b.to_string()).unwrap_or_default()))
        .unwrap()
}

fn sign_up(name: &str, email: &str) -> serde_json::Value {
    serde_json::json!({"username": name, "password": PASSWORD, "email": email})
}

fn token_in(mail: &MemoryEmailSender) -> String {
    let body = mail.sent().last().expect("a message").body.clone();
    let (_, rest) = body.split_once("verify-email?token=").expect("a link");
    rest.split_whitespace().next().unwrap().to_string()
}

#[tokio::test]
async fn sign_up_then_verify_then_sign_in() {
    let s = server();
    let info = send(
        &s.app,
        Request::get("/v1/registration")
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .json::<api::RegistrationInfo>();
    assert_eq!(
        (
            info.registration.as_str(),
            info.email_verification,
            info.approval
        ),
        ("open", true, false)
    );

    let r = send(
        &s.app,
        post("/v1/register", sign_up("alice", "alice@example.org"), None),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED);
    assert_eq!(
        r.json::<api::Registered>(),
        api::Registered {
            user: "alice".into(),
            status: "pending_verification".into()
        }
    );
    assert_eq!(s.mail.sent()[0].to, "alice@example.org");

    let login = serde_json::json!({"username": "alice", "password": PASSWORD});
    let r = send(&s.app, post("/v1/login", login.clone(), None)).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::FORBIDDEN, "email_not_verified")
    );
    let r = send(&s.app, post("/v1/session", login.clone(), None)).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::FORBIDDEN, "email_not_verified")
    );

    let r = send(
        &s.app,
        post(
            "/v1/register/verify",
            serde_json::json!({"token": token_in(&s.mail)}),
            None,
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json::<api::Registered>().status, "active");
    assert_eq!(
        send(&s.app, post("/v1/login", login, None)).await.status,
        StatusCode::OK
    );

    let again = send(
        &s.app,
        post(
            "/v1/register/verify",
            serde_json::json!({"token": token_in(&s.mail)}),
            None,
        ),
    )
    .await;
    assert_eq!(
        (again.status, again.code().as_str()),
        (StatusCode::BAD_REQUEST, "invalid_verification")
    );
}

#[tokio::test]
async fn an_address_is_required_and_a_wrong_password_reveals_nothing() {
    let s = server();
    let r = send(
        &s.app,
        post(
            "/v1/register",
            serde_json::json!({"username": "alice", "password": PASSWORD}),
            None,
        ),
    )
    .await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::BAD_REQUEST, "invalid_request")
    );

    send(
        &s.app,
        post("/v1/register", sign_up("alice", "alice@example.org"), None),
    )
    .await;
    let wrong = send(
        &s.app,
        post(
            "/v1/login",
            serde_json::json!({"username": "alice", "password": "not the password"}),
            None,
        ),
    )
    .await;
    let unknown = send(
        &s.app,
        post(
            "/v1/login",
            serde_json::json!({"username": "nobody", "password": "not the password"}),
            None,
        ),
    )
    .await;
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
    assert_eq!(wrong.status, unknown.status);
    assert_eq!(
        wrong.body, unknown.body,
        "identical bodies for a real and an unknown user"
    );
}

#[tokio::test]
async fn signing_up_with_a_registered_address_gets_the_same_answer() {
    let s = server();
    send(
        &s.app,
        post("/v1/register", sign_up("alice", "alice@example.org"), None),
    )
    .await;
    send(
        &s.app,
        post(
            "/v1/register/verify",
            serde_json::json!({"token": token_in(&s.mail)}),
            None,
        ),
    )
    .await;

    let fresh = send(
        &s.app,
        post("/v1/register", sign_up("bob", "bob@example.org"), None),
    )
    .await;
    let taken = send(
        &s.app,
        post("/v1/register", sign_up("eve", "alice@example.org"), None),
    )
    .await;
    assert_eq!(fresh.status, taken.status);
    assert_eq!(
        fresh.json::<api::Registered>().status,
        taken.json::<api::Registered>().status
    );
    let notice = s.mail.sent().last().unwrap().clone();
    assert_eq!(notice.to, "alice@example.org");
    assert!(!notice.body.contains("verify-email"));
    let r = send(
        &s.app,
        post(
            "/v1/login",
            serde_json::json!({"username": "eve", "password": PASSWORD}),
            None,
        ),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::UNAUTHORIZED,
        "no account exists for eve"
    );
}

#[tokio::test]
async fn resending_always_answers_accepted() {
    let s = server();
    send(
        &s.app,
        post("/v1/register", sign_up("alice", "alice@example.org"), None),
    )
    .await;
    for email in ["alice@example.org", "nobody@example.org", "junk"] {
        let r = send(
            &s.app,
            post(
                "/v1/register/resend",
                serde_json::json!({"email": email}),
                None,
            ),
        )
        .await;
        assert_eq!(r.status, StatusCode::ACCEPTED, "{email}");
        assert!(r.body.is_empty());
    }
    assert_eq!(
        s.mail.sent().len(),
        2,
        "only the real pending address got a second message"
    );
}

#[tokio::test]
async fn sign_ups_are_limited_per_client_address_with_retry_after() {
    let s = server_with(true, |c| {
        c.require_email_verification = false;
        c.limits = RateLimits {
            register_per_client: 2,
            ..RateLimits::default()
        };
    });
    let join = |n: &str| serde_json::json!({"username": n, "password": PASSWORD});
    for n in ["user1", "user2"] {
        let r = send(&s.app, post("/v1/register", join(n), Some("203.0.113.9"))).await;
        assert_eq!(r.status, StatusCode::CREATED);
    }
    let r = send(
        &s.app,
        post("/v1/register", join("user3"), Some("203.0.113.9")),
    )
    .await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(r.code(), "too_many_attempts");
    let wait: i64 = r.retry_after.expect("Retry-After").parse().unwrap();
    assert!((1..=3600).contains(&wait));

    let r = send(
        &s.app,
        post("/v1/register", join("user3"), Some("198.51.100.1")),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::CREATED,
        "another address is unaffected"
    );
    let r = send(
        &s.app,
        post(
            "/v1/register",
            join("user4"),
            Some("203.0.113.9, 198.51.100.7"),
        ),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::CREATED,
        "the last forwarded entry is the client"
    );
}

#[tokio::test]
async fn the_forwarded_header_is_ignored_unless_trusted() {
    let s = server_with(false, |c| {
        c.require_email_verification = false;
        c.limits = RateLimits {
            register_per_client: 1,
            ..RateLimits::default()
        };
    });
    for n in ["user1", "user2", "user3"] {
        let r = send(
            &s.app,
            post(
                "/v1/register",
                serde_json::json!({"username": n, "password": PASSWORD}),
                Some("203.0.113.9"),
            ),
        )
        .await;
        assert_eq!(
            r.status,
            StatusCode::CREATED,
            "a spoofed header buys nothing and costs nothing"
        );
    }
}

#[tokio::test]
async fn failed_sign_ins_lock_out_a_client_address() {
    let s = server_with(true, |c| {
        c.require_email_verification = false;
        c.limits = RateLimits {
            sign_in_per_client: 2,
            ..RateLimits::default()
        };
    });
    send(
        &s.app,
        post(
            "/v1/register",
            serde_json::json!({"username": "alice", "password": PASSWORD}),
            None,
        ),
    )
    .await;
    let bad = |n: &str| serde_json::json!({"username": n, "password": "not the password"});
    for n in ["a", "b"] {
        let r = send(&s.app, post("/v1/session", bad(n), Some("203.0.113.9"))).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    }
    let good = serde_json::json!({"username": "alice", "password": PASSWORD});
    let r = send(
        &s.app,
        post("/v1/session", good.clone(), Some("203.0.113.9")),
    )
    .await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(r.retry_after.is_some());
    let r = send(&s.app, post("/v1/session", good, Some("198.51.100.1"))).await;
    assert_eq!(r.status, StatusCode::OK);
}

#[tokio::test]
async fn approval_is_an_administrators_decision_and_is_logged() {
    let s = server_with(true, |c| c.require_approval = true);
    let info = send(
        &s.app,
        Request::get("/v1/registration")
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .json::<api::RegistrationInfo>();
    assert!(info.approval);

    send(
        &s.app,
        post("/v1/register", sign_up("alice", "alice@example.org"), None),
    )
    .await;
    let r = send(
        &s.app,
        post(
            "/v1/register/verify",
            serde_json::json!({"token": token_in(&s.mail)}),
            None,
        ),
    )
    .await;
    assert_eq!(r.json::<api::Registered>().status, "pending_approval");
    let login = serde_json::json!({"username": "alice", "password": PASSWORD});
    let r = send(&s.app, post("/v1/login", login.clone(), None)).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::FORBIDDEN, "approval_pending")
    );

    let r = send(
        &s.app,
        as_admin("GET", "/v1/admin/users?status=pending_approval", None),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    let waiting = r.json::<Vec<api::AccountInfo>>();
    assert_eq!(waiting.len(), 1);
    assert_eq!(
        (
            waiting[0].user.as_str(),
            waiting[0].email.as_deref(),
            waiting[0].email_verified,
            waiting[0].status.as_str()
        ),
        ("alice", Some("alice@example.org"), true, "pending_approval")
    );

    let r = send(
        &s.app,
        as_admin("POST", "/v1/admin/users/alice/approve", None),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json::<api::AccountInfo>().status, "active");
    let r = send(&s.app, post("/v1/login", login, None)).await;
    assert_eq!(r.status, StatusCode::OK);

    let r = send(
        &s.app,
        as_admin("POST", "/v1/admin/users/alice/approve", None),
    )
    .await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::BAD_REQUEST, "invalid_request")
    );
    let page = send(&s.app, as_admin("GET", "/v1/admin/audit", None))
        .await
        .json::<api::AuditPage>();
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.entries[0].action, "account_approved");
}

#[tokio::test]
async fn disabling_an_account_ends_its_access_and_enabling_restores_it() {
    let s = server_with(true, |c| c.require_email_verification = false);
    let join = serde_json::json!({"username": "alice", "password": PASSWORD});
    send(&s.app, post("/v1/register", join.clone(), None)).await;

    let r = send(&s.app, post("/v1/login", join.clone(), None)).await;
    let token = r.json::<api::CreatedToken>().token;
    let bearer = |t: &str| {
        Request::get("/v1/me")
            .header("authorization", format!("Bearer {t}"))
            .body(Body::empty())
            .unwrap()
    };
    let me = send(&s.app, bearer(&token)).await;
    assert_eq!(
        me.json::<api::Account>(),
        api::Account {
            user: "alice".into(),
            admin: false
        }
    );

    let r = send(
        &s.app,
        as_admin(
            "POST",
            "/v1/admin/users/alice/disable",
            Some(serde_json::json!({"reason": "spam"})),
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    let disabled = r.json::<api::AccountInfo>();
    assert_eq!(
        (
            disabled.status.as_str(),
            disabled.disabled_reason.as_deref()
        ),
        ("disabled", Some("spam"))
    );
    assert!(disabled.disabled_at.is_some());

    let r = send(&s.app, bearer(&token)).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::FORBIDDEN, "account_disabled")
    );
    let r = send(&s.app, post("/v1/session", join.clone(), None)).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::FORBIDDEN, "account_disabled")
    );

    let r = send(
        &s.app,
        as_admin("POST", "/v1/admin/users/alice/enable", None),
    )
    .await;
    assert_eq!(r.json::<api::AccountInfo>().status, "active");
    assert_eq!(send(&s.app, bearer(&token)).await.status, StatusCode::OK);

    let r = send(
        &s.app,
        as_admin(
            "POST",
            "/v1/admin/users/ghost/disable",
            Some(serde_json::json!({})),
        ),
    )
    .await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::NOT_FOUND, "user_not_found")
    );

    let r = send(&s.app, as_admin("GET", "/v1/admin/audit", None)).await;
    let page = r.json::<api::AuditPage>();
    let actions: Vec<_> = page.entries.iter().map(|e| e.action.as_str()).collect();
    assert_eq!(actions, ["account_enabled", "account_disabled"]);
    assert_eq!(page.entries[1].actor, "root");
    assert!(page.entries[1].detail.contains("spam"));
}

#[tokio::test]
async fn an_ordinary_account_cannot_reach_the_admin_routes() {
    let s = server_with(true, |c| c.require_email_verification = false);
    send(
        &s.app,
        post(
            "/v1/register",
            serde_json::json!({"username": "alice", "password": PASSWORD}),
            None,
        ),
    )
    .await;
    let r = send(
        &s.app,
        post(
            "/v1/login",
            serde_json::json!({"username": "alice", "password": PASSWORD}),
            None,
        ),
    )
    .await;
    let token = r.json::<api::CreatedToken>().token;
    for (method, uri) in [
        ("GET", "/v1/admin/users"),
        ("GET", "/v1/admin/audit"),
        ("POST", "/v1/admin/users/alice/enable"),
    ] {
        let r = send(
            &s.app,
            Request::builder()
                .method(method)
                .uri(uri)
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(
            (r.status, r.code().as_str()),
            (StatusCode::FORBIDDEN, "server_admin_required"),
            "{method} {uri}"
        );
    }
    let r = send(
        &s.app,
        Request::get("/v1/admin/users").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
}

async fn sign_in_token(s: &Server, name: &str) -> String {
    let body = serde_json::json!({"username": name, "password": PASSWORD});
    send(&s.app, post("/v1/register", body.clone(), None)).await;
    let r = send(&s.app, post("/v1/login", body, None)).await;
    r.json::<api::CreatedToken>().token
}

fn with_bearer(method: &str, uri: &str, token: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
async fn administrators_are_granted_and_revoked_over_the_admin_route() {
    let s = server_with(true, |c| c.require_email_verification = false);
    let alice = sign_in_token(&s, "alice").await;
    let bob = sign_in_token(&s, "bob").await;
    let uri = "/v1/admin/users/alice/admin";

    let r = send(
        &s.app,
        with_bearer("PUT", "/v1/admin/users/bob/admin", &alice),
    )
    .await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::FORBIDDEN, "server_admin_required")
    );
    let r = send(&s.app, as_admin("PUT", "/v1/admin/users/ghost/admin", None)).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::NOT_FOUND, "user_not_found")
    );

    let r = send(&s.app, as_admin("PUT", uri, None)).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.json::<api::AccountInfo>().admin);
    let r = send(&s.app, as_admin("PUT", uri, None)).await;
    assert!(r.json::<api::AccountInfo>().admin);

    let r = send(
        &s.app,
        with_bearer("PUT", "/v1/admin/users/bob/admin", &alice),
    )
    .await;
    assert!(r.json::<api::AccountInfo>().admin);
    let r = send(&s.app, with_bearer("DELETE", uri, &bob)).await;
    assert!(!r.json::<api::AccountInfo>().admin);
    let r = send(&s.app, with_bearer("DELETE", uri, &alice)).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);

    let r = send(
        &s.app,
        with_bearer("DELETE", "/v1/admin/users/bob/admin", &bob),
    )
    .await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::CONFLICT, "last_server_admin")
    );

    let page = send(&s.app, as_admin("GET", "/v1/admin/audit", None))
        .await
        .json::<api::AuditPage>();
    let seen: Vec<_> = page
        .entries
        .iter()
        .rev()
        .map(|e| (e.actor.as_str(), e.action.as_str(), e.detail.as_str()))
        .collect();
    assert_eq!(
        seen,
        [
            ("root", "admin_granted", "alice made an administrator"),
            ("alice", "admin_granted", "bob made an administrator"),
            ("bob", "admin_revoked", "alice no longer an administrator"),
        ]
    );
}

#[tokio::test]
async fn the_development_identity_is_reported_as_an_administrator() {
    let s = server_with(true, |c| c.require_email_verification = false);
    let r = send(&s.app, as_user("root", "GET", "/v1/me", None)).await;
    assert!(r.json::<api::Account>().admin, "the development identity");
}
