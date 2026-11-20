//! Administrator access sessions (decision #1024): start, reason, one hour, early end, what the session reaches,
//! the audit trail and the account's notification.

#[path = "common/cred.rs"]
mod cred;
use cred::{Cred, WithCred};
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{Duration, TimeZone, Utc};
use http_body_util::BodyExt;
use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore,
};
use pyn_core::{
    AccessConfig, AccessService, AuditAction, AuditQuery, AuditScope, AuditStore, AuthProvider,
    ManualClock, RegistrationMode, Repositories, Rules, UserId,
};
use pyn_proto as api;
use pyn_server::auth::{BearerAuth, DevHeaderAuth};
use pyn_server::{AppState, router};
use serde_json::{Value, json};
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";

struct QuickPasswords;

#[async_trait::async_trait]
impl pyn_core::PasswordWorker for QuickPasswords {
    async fn hash(&self, password: &str) -> pyn_core::Result<String> {
        Ok(format!("quick:{password}"))
    }

    async fn verify(&self, password: &str, hash: &str) -> bool {
        hash == format!("quick:{password}")
    }
}

struct World {
    app: Router,
    clock: Arc<ManualClock>,
    access: Arc<AccessService>,
    audit: Arc<MemoryAuditStore>,
}

async fn world() -> World {
    let clock = Arc::new(ManualClock::new(
        Utc.with_ymd_and_hms(2026, 11, 20, 9, 0, 0).unwrap(),
    ));
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
            ..AccessConfig::default()
        })
        .with_passwords(Arc::new(QuickPasswords))
        .with_registry(meta.clone())
        .with_audit(audit.clone()),
    );
    access.admin_for_tests(&UserId::new("root")).await.unwrap();
    let repos = Arc::new(Repositories::new(
        meta,
        objects.clone(),
        audit.clone(),
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
        updates: None,
    });
    World {
        app,
        clock,
        access,
        audit,
    }
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
        serde_json::from_slice::<api::ErrorBody>(&self.body)
            .map(|e| e.code)
            .unwrap_or_default()
    }

    fn is(&self, status: StatusCode, code: &str) -> bool {
        self.status == status && self.code() == code
    }
}

async fn send(app: &Router, who: &str, method: &str, uri: &str, body: Option<Value>) -> Reply {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .cred(&Cred::who(who))
        .body(Body::from(body.map(|b| b.to_string()).unwrap_or_default()))
        .unwrap();
    let r = app.clone().oneshot(req).await.unwrap();
    Reply {
        status: r.status(),
        body: r.into_body().collect().await.unwrap().to_bytes().to_vec(),
    }
}

/// Registers the account and returns its sign-in token.
async fn sign_up(w: &World, name: &str) -> String {
    let body = json!({"username": name, "password": PASSWORD});
    let r = send(&w.app, "", "POST", "/v1/register", Some(body.clone())).await;
    assert_eq!(r.status, StatusCode::CREATED);
    send(&w.app, "", "POST", "/v1/login", Some(body))
        .await
        .json::<api::CreatedToken>()
        .token
}

async fn start(w: &World, who: &str, user: &str, body: Value) -> Reply {
    send(
        &w.app,
        who,
        "POST",
        &format!("/v1/admin/users/{user}/access-sessions"),
        Some(body),
    )
    .await
}

/// Alice with a private repository and an organization repository, and a session of root on her.
struct Scene {
    w: World,
    alice: String,
    session: String,
    id: String,
}

async fn scene() -> Scene {
    let w = world().await;
    let alice = sign_up(&w, "alice").await;
    for (owner, name) in [("alice", "game")] {
        let r = send(
            &w.app,
            &alice,
            "POST",
            "/v1/repos",
            Some(json!({"owner": owner, "name": name})),
        )
        .await;
        assert_eq!(r.status, StatusCode::CREATED);
    }
    let r = send(
        &w.app,
        &alice,
        "POST",
        "/v1/orgs",
        Some(json!({"name": "acme"})),
    )
    .await;
    assert!(r.status.is_success(), "{}", r.code());
    let r = send(
        &w.app,
        &alice,
        "POST",
        "/v1/repos",
        Some(json!({"owner": "acme", "name": "tools"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED);
    let r = start(&w, "root", "alice", json!({"reason": "abuse_report"})).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.code());
    let started = r.json::<api::StartedAccessSession>();
    Scene {
        w,
        alice,
        session: started.secret,
        id: started.info.id,
    }
}

fn account_log(w: &World, id: &str) -> AuditScope {
    let _ = w;
    AuditScope::Account(pyn_core::AccountId::new(id))
}

async fn account_id(w: &World, user: &str) -> String {
    let who = pyn_core::Identity {
        user: UserId::new("root"),
        credential: pyn_core::Credential::Unrestricted,
    };
    w.access
        .list_accounts(&who, None, 100)
        .await
        .unwrap()
        .into_iter()
        .find(|a| a.user.as_str() == user)
        .unwrap()
        .id
        .to_string()
}

async fn log(w: &World, scope: &AuditScope) -> Vec<pyn_core::AuditEvent> {
    let mut events = w
        .audit
        .list(
            scope,
            &AuditQuery {
                limit: 500,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    events.reverse();
    pyn_core::integrity::check_events(scope, events).unwrap()
}

#[tokio::test]
async fn only_a_server_administrator_can_start_a_session() {
    let w = world().await;
    let alice = sign_up(&w, "alice").await;
    let bob = sign_up(&w, "bob").await;
    let body = json!({"reason": "abuse_report"});
    let r = start(&w, &bob, "alice", body.clone()).await;
    assert!(r.is(StatusCode::FORBIDDEN, "server_admin_required"));
    let r = start(&w, "", "alice", body.clone()).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r = start(&w, "root", "alice", body.clone()).await;
    assert_eq!(r.status, StatusCode::CREATED);
    let info = r.json::<api::StartedAccessSession>().info;
    assert_eq!((info.admin.as_str(), info.user.as_str()), ("root", "alice"));
    assert_eq!(info.expires_at - info.started_at, Duration::hours(1));
    let r = start(&w, "root", "nobody", body.clone()).await;
    assert!(r.is(StatusCode::NOT_FOUND, "user_not_found"));
    let r = start(&w, "root", "root", body).await;
    assert!(
        r.is(StatusCode::BAD_REQUEST, "invalid_request"),
        "own account"
    );
    let _ = alice;
}

#[tokio::test]
async fn a_session_needs_a_reason_from_the_list_and_notes_for_other() {
    let w = world().await;
    sign_up(&w, "alice").await;
    for bad in [
        json!({"reason": "curiosity"}),
        json!({"reason": "other"}),
        json!({"reason": "other", "notes": "   "}),
        json!({"reason": "abuse_report", "notes": "x".repeat(501)}),
    ] {
        let r = start(&w, "root", "alice", bad.clone()).await;
        assert!(r.is(StatusCode::BAD_REQUEST, "invalid_request"), "{bad}");
    }
    for ok in [
        json!({"reason": "abuse_report"}),
        json!({"reason": "security_incident", "notes": "optional"}),
        json!({"reason": "support_request"}),
        json!({"reason": "legal_request"}),
        json!({"reason": "other", "notes": "a ticket number"}),
    ] {
        let r = start(&w, "root", "alice", ok.clone()).await;
        assert_eq!(r.status, StatusCode::CREATED, "{ok}");
    }
}

#[tokio::test]
async fn a_session_lasts_an_hour_and_never_extends_itself() {
    let s = scene().await;
    async fn me(s: &Scene) -> Reply {
        send(&s.w.app, &s.session, "GET", "/v1/me", None).await
    }
    let r = me(&s).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json::<api::Account>().user, "alice");
    s.w.clock.advance(Duration::minutes(59));
    assert_eq!(me(&s).await.status, StatusCode::OK);
    let listed = send(&s.w.app, "root", "GET", "/v1/admin/access-sessions", None)
        .await
        .json::<Vec<api::AccessSessionInfo>>();
    assert_eq!(
        listed[0].expires_at - listed[0].started_at,
        Duration::hours(1),
        "use did not extend it"
    );
    s.w.clock.advance(Duration::minutes(1));
    let r = me(&s).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    s.w.clock.advance(Duration::hours(5));
    assert_eq!(me(&s).await.status, StatusCode::UNAUTHORIZED);
    let listed = send(&s.w.app, "root", "GET", "/v1/admin/access-sessions", None)
        .await
        .json::<Vec<api::AccessSessionInfo>>();
    assert_eq!(listed[0].state, "expired");
    assert_eq!(listed[0].ended_at, Some(listed[0].expires_at));
}

#[tokio::test]
async fn an_administrator_can_end_a_session_early() {
    let s = scene().await;
    let ok = send(&s.w.app, &s.session, "GET", "/v1/repos/alice/game", None).await;
    assert_eq!(ok.status, StatusCode::OK);
    async fn end(s: &Scene, who: &str) -> Reply {
        send(
            &s.w.app,
            who,
            "POST",
            &format!("/v1/admin/access-sessions/{}/end", s.id),
            None,
        )
        .await
    }
    let r = send(
        &s.w.app,
        &s.alice,
        "POST",
        &format!("/v1/admin/access-sessions/{}/end", s.id),
        None,
    )
    .await;
    assert!(r.is(StatusCode::FORBIDDEN, "server_admin_required"));
    let r = end(&s, "root").await;
    assert_eq!(r.status, StatusCode::OK);
    let info = r.json::<api::AccessSessionInfo>();
    assert_eq!(info.state, "ended");
    assert!(info.ended_at.is_some());
    let r = send(&s.w.app, &s.session, "GET", "/v1/repos/alice/game", None).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(end(&s, "root").await.status, StatusCode::OK, "ending twice");
    let ended = log(&s.w, &AuditScope::Server)
        .await
        .into_iter()
        .filter(|e| e.action == AuditAction::AccessSessionEnded)
        .count();
    assert_eq!(ended, 1, "recorded once");
    let r = send(
        &s.w.app,
        "root",
        "POST",
        "/v1/admin/access-sessions/ffffffffffff/end",
        None,
    )
    .await;
    assert!(r.is(StatusCode::NOT_FOUND, "access_session_not_found"));
}

#[tokio::test]
async fn an_administrator_may_act_as_another_administrator() {
    let w = world().await;
    let body = json!({"reason": "security_incident"});
    w.access
        .admin_for_tests(&UserId::new("second"))
        .await
        .unwrap();
    let r = start(&w, "root", "second", body.clone()).await;
    assert_eq!(r.status, StatusCode::CREATED);
    let secret = r.json::<api::StartedAccessSession>().secret;
    let r = send(&w.app, &secret, "GET", "/v1/admin/audit", None).await;
    assert!(
        r.is(StatusCode::FORBIDDEN, "access_session_not_allowed"),
        "a session on an administrator does not administer the server"
    );
}

#[tokio::test]
async fn a_session_reaches_what_the_account_reaches() {
    let s = scene().await;
    let get = |uri: &'static str| send(&s.w.app, &s.session, "GET", uri, None);
    assert_eq!(get("/v1/repos/alice/game").await.status, StatusCode::OK);
    assert_eq!(
        get("/v1/repos/alice/game/files").await.status,
        StatusCode::OK
    );
    assert_eq!(get("/v1/repos/acme/tools").await.status, StatusCode::OK);
    let repos = get("/v1/repos").await.json::<Vec<api::RepoInfo>>();
    let mut names: Vec<_> = repos
        .iter()
        .map(|r| format!("{}/{}", r.owner, r.name))
        .collect();
    names.sort();
    assert_eq!(names, ["acme/tools", "alice/game"]);
    let r = send(
        &s.w.app,
        &s.session,
        "POST",
        "/v1/repos/alice/game/changesets",
        Some(json!({"entries": [], "deletions": [], "message": "m"})),
    )
    .await;
    assert!(
        !r.is(StatusCode::FORBIDDEN, "access_session_not_allowed"),
        "repository work is the account's: {} {}",
        r.status,
        r.code()
    );
    let other = send(&s.w.app, &s.session, "GET", "/v1/repos/root/none", None).await;
    assert_eq!(
        other.status,
        StatusCode::NOT_FOUND,
        "no more than the account"
    );
}

#[tokio::test]
async fn a_session_is_refused_where_it_could_outlast_itself_or_widen() {
    let s = scene().await;
    let forbidden: [(&str, &str, Option<Value>); 16] = [
        ("POST", "/v1/tokens", Some(json!({"name": "t"}))),
        ("GET", "/v1/tokens", None),
        (
            "PUT",
            "/v1/me/password",
            Some(json!({"current_password": "x", "new_password": "y"})),
        ),
        ("POST", "/v1/me/two-factor/totp", Some(json!({}))),
        ("GET", "/v1/me/passkeys", None),
        (
            "POST",
            "/v1/keys",
            Some(json!({"title": "k", "key": "ssh-ed25519 AAAA"})),
        ),
        (
            "POST",
            "/v1/me/emails",
            Some(json!({"address": "a@example.com"})),
        ),
        (
            "PUT",
            "/v1/me/username",
            Some(json!({"username": "z", "password": "x"})),
        ),
        (
            "POST",
            "/v1/account/deletion",
            Some(json!({"password": "x"})),
        ),
        ("GET", "/v1/me/access-sessions", None),
        (
            "POST",
            "/v1/admin/users/alice/access-sessions",
            Some(json!({"reason": "abuse_report"})),
        ),
        ("GET", "/v1/admin/audit", None),
        ("PUT", "/v1/me/profile", Some(json!({}))),
        (
            "POST",
            "/v1/orgs/acme/members",
            Some(json!({"user": "root", "role": "owner"})),
        ),
        (
            "PUT",
            "/v1/repos/alice/game/members/root",
            Some(json!({"role": "admin"})),
        ),
        ("DELETE", "/v1/repos/alice/game", None),
    ];
    for (method, uri, body) in forbidden {
        let r = send(&s.w.app, &s.session, method, uri, body).await;
        assert!(
            r.is(StatusCode::FORBIDDEN, "access_session_not_allowed"),
            "{method} {uri}: {} {}",
            r.status,
            r.code()
        );
    }
    let still = send(&s.w.app, &s.alice, "GET", "/v1/repos/alice/game", None).await;
    assert_eq!(
        still.status,
        StatusCode::OK,
        "nothing changed for the account"
    );
}

#[tokio::test]
async fn the_account_sees_its_sessions_and_cannot_turn_them_off() {
    let s = scene().await;
    let r = send(&s.w.app, &s.session, "GET", "/v1/repos/alice/game", None).await;
    assert_eq!(r.status, StatusCode::OK);
    let mine = send(&s.w.app, &s.alice, "GET", "/v1/me/access-sessions", None)
        .await
        .json::<Vec<api::AccessSessionInfo>>();
    assert_eq!(mine.len(), 1);
    let info = &mine[0];
    assert_eq!(info.id, s.id);
    assert_eq!(
        (info.admin.as_str(), info.reason.as_str()),
        ("root", "abuse_report")
    );
    assert_eq!(info.state, "active");
    assert!(
        info.reached.iter().any(|c| c.method == "GET"
            && c.route == "/v1/repos/{owner}/{name}"
            && c.target == "alice/game"),
        "{:?}",
        info.reached
    );
    for (method, uri) in [
        ("DELETE", format!("/v1/me/access-sessions/{}", s.id)),
        ("PUT", "/v1/me/access-sessions".to_string()),
        ("PATCH", "/v1/me/access-sessions".to_string()),
        ("DELETE", format!("/v1/me/notices/{}", s.id)),
    ] {
        let r = send(&s.w.app, &s.alice, method, &uri, Some(json!({}))).await;
        assert!(
            matches!(
                r.status,
                StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
            ),
            "{method} {uri}: {}",
            r.status
        );
    }
    send(
        &s.w.app,
        "root",
        "POST",
        &format!("/v1/admin/access-sessions/{}/end", s.id),
        None,
    )
    .await;
    let mine = send(&s.w.app, &s.alice, "GET", "/v1/me/access-sessions", None)
        .await
        .json::<Vec<api::AccessSessionInfo>>();
    assert_eq!(mine.len(), 1, "still listed once over");
    assert_eq!(mine[0].state, "ended");
    let bob = sign_up(&s.w, "bob").await;
    let theirs = send(&s.w.app, &bob, "GET", "/v1/me/access-sessions", None)
        .await
        .json::<Vec<api::AccessSessionInfo>>();
    assert!(theirs.is_empty(), "an account sees only its own");
}

#[tokio::test]
async fn start_end_and_reach_are_in_both_logs_in_the_hash_chain() {
    let s = scene().await;
    for _ in 0..5 {
        send(&s.w.app, &s.session, "GET", "/v1/repos/alice/game", None).await;
    }
    send(
        &s.w.app,
        &s.session,
        "GET",
        "/v1/repos/alice/game/files",
        None,
    )
    .await;
    send(
        &s.w.app,
        &s.session,
        "POST",
        "/v1/tokens",
        Some(json!({"name": "t"})),
    )
    .await;
    send(
        &s.w.app,
        "root",
        "POST",
        &format!("/v1/admin/access-sessions/{}/end", s.id),
        None,
    )
    .await;
    let account = account_id(&s.w, "alice").await;
    for scope in [AuditScope::Server, account_log(&s.w, &account)] {
        let events = log(&s.w, &scope).await;
        let actions: Vec<_> = events
            .iter()
            .filter(|e| {
                matches!(
                    e.action,
                    AuditAction::AccessSessionStarted
                        | AuditAction::AccessSessionReached
                        | AuditAction::AccessSessionEnded
                )
            })
            .collect();
        assert_eq!(
            actions.first().unwrap().action,
            AuditAction::AccessSessionStarted
        );
        assert_eq!(
            actions.last().unwrap().action,
            AuditAction::AccessSessionEnded
        );
        let reached: Vec<_> = actions
            .iter()
            .filter(|e| e.action == AuditAction::AccessSessionReached)
            .map(|e| e.detail.as_str())
            .collect();
        assert_eq!(
            reached.len(),
            3,
            "one entry per route, not per request: {reached:?}"
        );
        assert!(
            reached
                .iter()
                .any(|d| d.contains("reached: GET /v1/repos/{owner}/{name} on alice/game"))
        );
        assert!(
            reached
                .iter()
                .any(|d| d.contains("refused: POST /v1/tokens"))
        );
        assert!(
            actions
                .last()
                .unwrap()
                .detail
                .contains("7 requests over 3 routes")
        );
        assert!(actions.iter().all(|e| e.actor.as_str() == "root"));
        for pair in events.windows(2) {
            assert_eq!(
                pair[1].previous.as_deref(),
                Some(pair[0].hash.as_str()),
                "chained"
            );
        }
    }
    assert_eq!(
        log(&s.w, &account_log(&s.w, &account_id(&s.w, "root").await))
            .await
            .len(),
        0,
        "the administrator's own security log is untouched"
    );
    let page = send(&s.w.app, "root", "GET", "/v1/admin/audit", None)
        .await
        .json::<api::AuditPage>();
    assert!(
        page.entries
            .iter()
            .any(|e| e.action == "access_session_started")
    );
}

#[tokio::test]
async fn an_expired_session_is_recorded_as_ended_at_its_expiry() {
    let s = scene().await;
    s.w.clock.advance(Duration::minutes(90));
    assert_eq!(
        s.w.access.finish_expired_access_sessions().await.unwrap(),
        1
    );
    assert_eq!(
        s.w.access.finish_expired_access_sessions().await.unwrap(),
        0
    );
    let events = log(&s.w, &AuditScope::Server).await;
    let ended: Vec<_> = events
        .iter()
        .filter(|e| e.action == AuditAction::AccessSessionEnded)
        .collect();
    assert_eq!(ended.len(), 1);
    let started = events
        .iter()
        .find(|e| e.action == AuditAction::AccessSessionStarted)
        .unwrap();
    assert_eq!(ended[0].at - started.at, Duration::hours(1));
    assert!(ended[0].detail.contains("expired"));
}

#[tokio::test]
async fn a_frozen_owner_can_be_read_and_not_written_through_a_session() {
    let s = scene().await;
    async fn write(s: &Scene) -> Reply {
        send(
            &s.w.app,
            &s.session,
            "POST",
            "/v1/repos/alice/game/branches",
            Some(json!({"name": "feat"})),
        )
        .await
    }
    let r = send(
        &s.w.app,
        "root",
        "PUT",
        "/v1/admin/owners/alice/freeze",
        Some(json!({"reason": "abuse report"})),
    )
    .await;
    assert!(r.status.is_success(), "{}", r.code());
    let r = send(&s.w.app, &s.session, "GET", "/v1/repos/alice/game", None).await;
    assert_eq!(r.status, StatusCode::OK, "reads work");
    let r = write(&s).await;
    assert!(
        r.is(StatusCode::FORBIDDEN, "owner_frozen"),
        "{} {}",
        r.status,
        r.code()
    );
    let r = start(&s.w, "root", "alice", json!({"reason": "legal_request"})).await;
    assert_eq!(
        r.status,
        StatusCode::CREATED,
        "a frozen account can still be looked at"
    );
    let r = send(
        &s.w.app,
        &s.session,
        "PUT",
        "/v1/admin/owners/alice/freeze",
        Some(json!({"reason": "x"})),
    )
    .await;
    assert!(
        r.is(StatusCode::FORBIDDEN, "access_session_not_allowed"),
        "cannot unfreeze"
    );
}

#[tokio::test]
async fn a_session_stops_when_its_administrator_or_account_stops_being_active() {
    let s = scene().await;
    let ok = || send(&s.w.app, &s.session, "GET", "/v1/repos/alice/game", None);
    assert_eq!(ok().await.status, StatusCode::OK);
    s.w.access
        .admin_for_tests(&UserId::new("backup"))
        .await
        .unwrap();
    let r = send(
        &s.w.app,
        "backup",
        "DELETE",
        "/v1/admin/users/root/admin",
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        ok().await.status,
        StatusCode::UNAUTHORIZED,
        "the administrator lost the role"
    );
    let r = send(
        &s.w.app,
        "backup",
        "PUT",
        "/v1/admin/users/root/admin",
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(ok().await.status, StatusCode::OK);
    let r = send(
        &s.w.app,
        "backup",
        "POST",
        "/v1/admin/users/alice/disable",
        Some(json!({})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        ok().await.status,
        StatusCode::FORBIDDEN,
        "the account is disabled"
    );
}

#[tokio::test]
async fn a_scoped_token_of_the_account_is_untouched_by_a_session() {
    let s = scene().await;
    let r = send(
        &s.w.app,
        &s.alice,
        "POST",
        "/v1/tokens",
        Some(cred::grants_body(
            "ro",
            "alice",
            &["game"],
            &[("contents", "read")],
        )),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.code());
    let scoped = r.json::<api::CreatedToken>().token;
    let before = send(&s.w.app, &scoped, "GET", "/v1/repos/acme/tools", None).await;
    assert_eq!(
        before.status,
        StatusCode::NOT_FOUND,
        "a scoped token covers its repository only"
    );
    let r = send(&s.w.app, &s.session, "GET", "/v1/repos/acme/tools", None).await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "the session has the account's reach, not the token's"
    );
    let tokens = send(&s.w.app, &s.alice, "GET", "/v1/tokens", None).await;
    assert_eq!(tokens.status, StatusCode::OK);
    send(
        &s.w.app,
        "root",
        "POST",
        &format!("/v1/admin/access-sessions/{}/end", s.id),
        None,
    )
    .await;
    let after = send(&s.w.app, &scoped, "GET", "/v1/repos/alice/game", None).await;
    assert_eq!(
        after.status,
        StatusCode::OK,
        "the token still works as before"
    );
}
