//! The HTTP contract of server invitations, administrator-added accounts, resending a confirmation and the email status.

#[path = "common/cred.rs"]
mod cred;
use cred::{BROWSER_ORIGIN, Cred, WithCred};
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore,
};
use pyn_core::{
    AccessConfig, AccessService, AuthProvider, EmailSender, MemoryEmailSender, NullEmailSender,
    RegistrationMode, Repositories, Rules, SystemClock,
};
use pyn_proto as api;
use pyn_server::auth::{BearerAuth, DevHeaderAuth};
use pyn_server::{AppState, router};
use serde_json::{Value, json};
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";

struct Server {
    app: Router,
    mail: Arc<MemoryEmailSender>,
}

fn server(mode: RegistrationMode, verify: bool, mailer: Option<Arc<dyn EmailSender>>) -> Server {
    let clock = Arc::new(SystemClock);
    let objects = Arc::new(MemoryObjectStore::new());
    let audit = Arc::new(MemoryAuditStore::new());
    let mail = Arc::new(MemoryEmailSender::new());
    let access = Arc::new(
        AccessService::new(
            Arc::new(MemoryAccessStore::new()),
            clock.clone(),
            Arc::new(pyn_core::MemoryLimitEvents::new()),
        )
        .with_config(AccessConfig {
            registration: mode,
            require_email_verification: verify,
            public_url: "https://pyn.example".into(),
            allowed_origins: vec![BROWSER_ORIGIN.into()],
            ..AccessConfig::default()
        })
        .with_email(mailer.unwrap_or_else(|| mail.clone()))
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
        trust_forwarded: false,
        trusted_proxies: 1,
        updates: None,
        storage: Default::default(),
        certificate: Default::default(),
        ready: Default::default(),
    });
    Server { app, mail }
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

    fn is(&self, status: StatusCode, code: &str) -> bool {
        self.status == status && self.json::<api::ErrorBody>().code == code
    }
}

async fn send(app: &Router, who: &str, method: &str, uri: &str, body: Option<Value>) -> Reply {
    let builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    let req = builder
        .cred(&Cred::who(who))
        .body(Body::from(body.map(|b| b.to_string()).unwrap_or_default()))
        .unwrap();
    let r = app.clone().oneshot(req).await.unwrap();
    Reply {
        status: r.status(),
        body: r.into_body().collect().await.unwrap().to_bytes().to_vec(),
    }
}

async fn sign_in(app: &Router, name: &str, password: &str) -> Reply {
    let req = Request::builder()
        .method("POST")
        .uri("/v1/session")
        .header("content-type", "application/json")
        .header("origin", BROWSER_ORIGIN)
        .body(Body::from(
            json!({"username": name, "password": password}).to_string(),
        ))
        .unwrap();
    let r = app.clone().oneshot(req).await.unwrap();
    Reply {
        status: r.status(),
        body: r.into_body().collect().await.unwrap().to_bytes().to_vec(),
    }
}

#[tokio::test]
async fn an_administrator_invites_and_the_person_registers_with_the_code() {
    let s = server(RegistrationMode::InviteOnly, false, None);
    let r = send(
        &s.app,
        "root",
        "POST",
        "/v1/admin/invitations",
        Some(json!({"email": "dana@example.org", "expires_in_days": 3})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED);
    let made: api::CreatedServerInvitation = r.json();
    assert!(made.emailed);
    assert!(s.mail.sent()[0].body.contains(&made.code));
    assert_eq!(made.invitation.status, api::ServerInvitationStatus::Pending);

    let r = send(&s.app, "root", "GET", "/v1/admin/invitations", None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(!String::from_utf8_lossy(&r.body).contains(&made.code));
    let listed: Vec<api::ServerInvitationInfo> = r.json();
    assert_eq!(listed.len(), 1);

    let register =
        |name: &str| json!({"username": name, "password": PASSWORD, "invite": made.code});
    let r = send(&s.app, "", "POST", "/v1/register", Some(register("dana"))).await;
    assert_eq!(r.status, StatusCode::CREATED);
    assert_eq!(r.json::<api::Registered>().status, "active");
    let r = send(&s.app, "", "POST", "/v1/register", Some(register("eve"))).await;
    assert!(r.is(StatusCode::BAD_REQUEST, "invalid_invite"));

    let listed: Vec<api::ServerInvitationInfo> =
        send(&s.app, "root", "GET", "/v1/admin/invitations", None)
            .await
            .json();
    assert_eq!(listed[0].status, api::ServerInvitationStatus::Used);
    assert_eq!(listed[0].used_by.as_deref(), Some("dana"));
}

#[tokio::test]
async fn a_revoked_invitation_is_refused_and_bad_requests_are_named() {
    let s = server(RegistrationMode::InviteOnly, false, None);
    let made: api::CreatedServerInvitation = send(
        &s.app,
        "root",
        "POST",
        "/v1/admin/invitations",
        Some(json!({})),
    )
    .await
    .json();
    let uri = format!("/v1/admin/invitations/{}", made.invitation.id);
    assert_eq!(
        send(&s.app, "root", "DELETE", &uri, None).await.status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        send(&s.app, "root", "DELETE", &uri, None).await.status,
        StatusCode::NO_CONTENT
    );
    let r = send(
        &s.app,
        "",
        "POST",
        "/v1/register",
        Some(json!({"username": "dana", "password": PASSWORD, "invite": made.code})),
    )
    .await;
    assert!(r.is(StatusCode::BAD_REQUEST, "invalid_invite"));
    let r = send(
        &s.app,
        "root",
        "DELETE",
        "/v1/admin/invitations/ffffffffffff",
        None,
    )
    .await;
    assert!(r.is(StatusCode::NOT_FOUND, "server_invitation_not_found"));

    for body in [
        json!({"expires_in_days": 0}),
        json!({"expires_in_days": 91}),
        json!({"email": "nope"}),
        json!({"grant": {"role": "reader"}}),
        json!({"grant": {"repository": "a/b", "organization": "c", "role": "reader"}}),
        json!({"grant": {"organization": "ghost", "role": "king"}}),
    ] {
        let r = send(
            &s.app,
            "root",
            "POST",
            "/v1/admin/invitations",
            Some(body.clone()),
        )
        .await;
        assert!(
            r.is(StatusCode::BAD_REQUEST, "invalid_request"),
            "{body}: {} {}",
            r.status,
            String::from_utf8_lossy(&r.body)
        );
    }
    let r = send(
        &s.app,
        "root",
        "POST",
        "/v1/admin/invitations",
        Some(json!({"grant": {"organization": "ghost", "role": "member"}})),
    )
    .await;
    assert!(r.is(StatusCode::NOT_FOUND, "org_not_found"));
}

#[tokio::test]
async fn only_an_administrator_reaches_the_invitation_routes() {
    let s = server(RegistrationMode::InviteOnly, false, None);
    let alice = {
        let body = json!({"username": "alice", "password": PASSWORD});
        send(
            &s.app,
            "root",
            "POST",
            "/v1/admin/users",
            Some(json!({"username": "alice", "password": PASSWORD})),
        )
        .await;
        let r = send(&s.app, "", "POST", "/v1/login", Some(body)).await;
        r.json::<api::CreatedToken>().token
    };
    for (method, uri, body) in [
        ("POST", "/v1/admin/invitations", Some(json!({}))),
        ("GET", "/v1/admin/invitations", None),
        ("DELETE", "/v1/admin/invitations/aaaaaaaaaaaa", None),
        (
            "POST",
            "/v1/admin/users",
            Some(json!({"username": "x1", "password": "one-time password"})),
        ),
        ("POST", "/v1/admin/users/alice/resend-confirmation", None),
    ] {
        let r = send(&s.app, &alice, method, uri, body).await;
        assert!(
            r.is(StatusCode::FORBIDDEN, "server_admin_required"),
            "{method} {uri}: {} {}",
            r.status,
            String::from_utf8_lossy(&r.body)
        );
    }
}

#[tokio::test]
async fn an_added_account_signs_in_with_a_change_required_mark_until_it_changes_the_password() {
    let s = server(RegistrationMode::InviteOnly, false, None);
    let r = send(
        &s.app,
        "root",
        "POST",
        "/v1/admin/users",
        Some(json!({"username": "erin", "password": "one-time password"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED);
    let info: api::AccountInfo = r.json();
    assert_eq!(
        (info.user.as_str(), info.status.as_str()),
        ("erin", "active")
    );

    let r = send(
        &s.app,
        "root",
        "POST",
        "/v1/admin/users",
        Some(json!({"username": "erin", "password": "another password"})),
    )
    .await;
    assert!(r.is(StatusCode::CONFLICT, "user_exists"));
    let r = send(
        &s.app,
        "root",
        "POST",
        "/v1/admin/users",
        Some(json!({"username": "frank", "password": "short"})),
    )
    .await;
    assert!(r.is(StatusCode::BAD_REQUEST, "invalid_request"));

    let r = sign_in(&s.app, "erin", "one-time password").await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.json::<api::SessionInfo>().password_change_required);

    let r = send(
        &s.app,
        "erin",
        "PUT",
        "/v1/me/password",
        Some(json!({"current": "one-time password", "new": "a better password"})),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::NO_CONTENT,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let r = sign_in(&s.app, "erin", "a better password").await;
    assert!(!r.json::<api::SessionInfo>().password_change_required);
}

#[tokio::test]
async fn an_administrator_resends_a_confirmation_and_the_conflicts_are_named() {
    let s = server(RegistrationMode::Open, true, None);
    let r = send(
        &s.app,
        "",
        "POST",
        "/v1/register",
        Some(json!({"username": "alice", "password": PASSWORD, "email": "alice@example.org"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED);
    let uri = "/v1/admin/users/alice/resend-confirmation";
    let r = send(&s.app, "root", "POST", uri, None).await;
    assert_eq!(r.status, StatusCode::ACCEPTED);
    assert_eq!(s.mail.sent().len(), 2);
    let r = send(
        &s.app,
        "root",
        "POST",
        "/v1/admin/users/ghost/resend-confirmation",
        None,
    )
    .await;
    assert!(r.is(StatusCode::NOT_FOUND, "user_not_found"));

    let token = s.mail.sent()[1]
        .body
        .split_once("verify-email?token=")
        .unwrap()
        .1
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();
    let r = send(
        &s.app,
        "",
        "POST",
        "/v1/register/verify",
        Some(json!({"token": token})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(s.mail.sent()[2].subject.contains("Welcome"));
    let r = send(&s.app, "root", "POST", uri, None).await;
    assert!(r.is(StatusCode::CONFLICT, "email_already_confirmed"));

    let off = server(RegistrationMode::Open, false, None);
    send(
        &off.app,
        "",
        "POST",
        "/v1/register",
        Some(json!({"username": "bob", "password": PASSWORD})),
    )
    .await;
    let r = send(
        &off.app,
        "root",
        "POST",
        "/v1/admin/users/bob/resend-confirmation",
        None,
    )
    .await;
    assert!(r.is(StatusCode::CONFLICT, "email_verification_off"));
}

#[tokio::test]
async fn admin_settings_say_whether_email_is_sent_without_naming_a_host() {
    let log = server(RegistrationMode::Open, true, None);
    let r = send(&log.app, "root", "GET", "/v1/admin/settings", None).await;
    let settings: api::AdminSettings = r.json();
    assert_eq!(
        settings.email,
        api::EmailSettings {
            mailer: api::MailerKind::Log,
            verification: true,
            last_failure: None,
            last_sent_at: None,
            last_check: None,
        }
    );
    let none = server(
        RegistrationMode::Open,
        false,
        Some(Arc::new(NullEmailSender)),
    );
    let r = send(&none.app, "root", "GET", "/v1/admin/settings", None).await;
    assert_eq!(
        r.json::<api::AdminSettings>().email,
        api::EmailSettings {
            mailer: api::MailerKind::None,
            verification: false,
            last_failure: None,
            last_sent_at: None,
            last_check: None,
        }
    );
    let raw: Value = serde_json::from_slice(&r.body).unwrap();
    assert_eq!(
        raw["email"],
        json!({"mailer": "none", "verification": false, "last_failure": null, "last_sent_at": null, "last_check": null})
    );
}

struct Broken;

#[async_trait::async_trait]
impl EmailSender for Broken {
    async fn send(&self, _message: pyn_core::EmailMessage) -> pyn_core::Result<()> {
        Err(pyn_core::PynError::EmailDeliveryFailed(
            "authentication failed".into(),
        ))
    }
}

#[tokio::test]
async fn admin_settings_report_a_failed_send_and_a_working_one() {
    let broken = server(RegistrationMode::Open, false, Some(Arc::new(Broken)));
    let body = json!({"username": "maya", "password": PASSWORD, "email": "maya@example.org"});
    let r = send(&broken.app, "", "POST", "/v1/register", Some(body.clone())).await;
    assert_eq!(r.status, StatusCode::CREATED);
    let r = send(&broken.app, "root", "GET", "/v1/admin/settings", None).await;
    let email = r.json::<api::AdminSettings>().email;
    let failure = email.last_failure.expect("the failure is reported");
    assert!(failure.error.contains("authentication failed"));
    assert!(email.last_sent_at.is_none());

    let ok = server(RegistrationMode::Open, false, None);
    send(&ok.app, "", "POST", "/v1/register", Some(body)).await;
    let r = send(&ok.app, "root", "GET", "/v1/admin/settings", None).await;
    let email = r.json::<api::AdminSettings>().email;
    assert!(email.last_failure.is_none() && email.last_sent_at.is_some());
}

#[tokio::test]
async fn a_test_email_needs_a_mailer_that_sends_and_an_address_to_send_to() {
    let log = server(RegistrationMode::Open, true, None);
    let r = send(&log.app, "root", "POST", "/v1/admin/email/test", None).await;
    assert!(
        r.is(StatusCode::BAD_REQUEST, "invalid_request"),
        "the account has no address: {}",
        String::from_utf8_lossy(&r.body)
    );
    let none = server(RegistrationMode::Open, false, Some(Arc::new(NullEmailSender)));
    let r = send(&none.app, "root", "POST", "/v1/admin/email/test", None).await;
    assert!(r.is(StatusCode::BAD_REQUEST, "invalid_request"));
    let r = send(&log.app, "nobody", "POST", "/v1/admin/email/test", None).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert!(log.mail.sent().is_empty());
}
