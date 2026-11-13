//! The HTTP contract of account self-service (#982): the caller's addresses, username change and moved addresses.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore,
};
use pyn_core::{
    AccessConfig, AccessService, AuthProvider, MemoryEmailSender, RegistrationMode, Repositories,
    Rules, SystemClock,
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

fn server() -> Server {
    let clock = Arc::new(SystemClock);
    let objects = Arc::new(MemoryObjectStore::new());
    let audit = Arc::new(MemoryAuditStore::new());
    let mail = Arc::new(MemoryEmailSender::new());
    let meta = Arc::new(MemoryMetadataStore::new());
    let access = Arc::new(
        AccessService::new(
            Arc::new(MemoryAccessStore::new()),
            clock.clone(),
            Arc::new(pyn_core::MemoryLimitEvents::new()),
        )
        .with_config(AccessConfig {
            registration: RegistrationMode::Open,
            public_url: "https://pyn.example".into(),
            ..AccessConfig::default()
        })
        .with_email(mail.clone())
        .with_registry(meta.clone())
        .with_audit(audit.clone()),
    );
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
        auth: Arc::new(BearerAuth { access }),
        dev_auth: Some(Arc::new(DevHeaderAuth) as Arc<dyn AuthProvider>),
        trust_forwarded: false,
    });
    Server { app, mail }
}

struct Reply {
    status: StatusCode,
    moved_to: Option<String>,
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

async fn send(app: &Router, req: Request<Body>) -> Reply {
    let r = app.clone().oneshot(req).await.unwrap();
    Reply {
        status: r.status(),
        moved_to: r
            .headers()
            .get(api::MOVED_TO_HEADER)
            .map(|v| v.to_str().unwrap().to_string()),
        body: r.into_body().collect().await.unwrap().to_bytes().to_vec(),
    }
}

fn call(user: &str, method: &str, uri: &str, body: Option<serde_json::Value>) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header(api::DEV_USER_HEADER, user)
        .body(Body::from(body.map(|b| b.to_string()).unwrap_or_default()))
        .unwrap()
}

fn anon(method: &str, uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn token_in(mail: &MemoryEmailSender) -> String {
    let body = mail.sent().last().expect("a message").body.clone();
    let (_, rest) = body.split_once("verify-email?token=").expect("a link");
    rest.split_whitespace().next().unwrap().to_string()
}

async fn signup(s: &Server, name: &str, email: &str) {
    let r = send(
        &s.app,
        anon(
            "POST",
            "/v1/register",
            serde_json::json!({"username": name, "password": PASSWORD, "email": email}),
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED);
    let r = send(
        &s.app,
        anon(
            "POST",
            "/v1/register/verify",
            serde_json::json!({"token": token_in(&s.mail)}),
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
}

async fn emails(s: &Server, user: &str) -> Vec<api::AccountEmail> {
    let r = send(&s.app, call(user, "GET", "/v1/me/emails", None)).await;
    assert_eq!(r.status, StatusCode::OK);
    r.json()
}

#[tokio::test]
async fn addresses_are_added_verified_made_primary_and_removed() {
    let s = server();
    signup(&s, "alice", "a@example.org").await;
    let held = emails(&s, "alice").await;
    assert_eq!(held.len(), 1);
    assert!(held[0].primary && held[0].verified);

    let r = send(
        &s.app,
        call(
            "alice",
            "POST",
            "/v1/me/emails",
            Some(serde_json::json!({"address": "B@Example.org"})),
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED);
    let added: api::AccountEmail = r.json();
    assert_eq!(
        (added.address.as_str(), added.verified, added.primary),
        ("b@example.org", false, false)
    );
    assert_eq!(s.mail.sent().last().unwrap().to, "b@example.org");

    let r = send(
        &s.app,
        call("alice", "PUT", "/v1/me/emails/b@example.org/primary", None),
    )
    .await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::CONFLICT, "email_refused")
    );

    let r = send(
        &s.app,
        call("alice", "POST", "/v1/me/emails/b@example.org/resend", None),
    )
    .await;
    assert_eq!(r.status, StatusCode::ACCEPTED);
    let r = send(
        &s.app,
        anon(
            "POST",
            "/v1/register/verify",
            serde_json::json!({"token": token_in(&s.mail)}),
        ),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "the registration link route redeems it"
    );

    let r = send(
        &s.app,
        call("alice", "PUT", "/v1/me/emails/b@example.org/primary", None),
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let held = emails(&s, "alice").await;
    assert_eq!(held[0].address, "b@example.org");
    assert!(held[0].primary);

    let r = send(
        &s.app,
        call("alice", "DELETE", "/v1/me/emails/a@example.org", None),
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let r = send(
        &s.app,
        call("alice", "DELETE", "/v1/me/emails/b@example.org", None),
    )
    .await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::CONFLICT, "email_refused")
    );
    let r = send(
        &s.app,
        call("alice", "DELETE", "/v1/me/emails/nope@example.org", None),
    )
    .await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::NOT_FOUND, "email_not_found")
    );
}

#[tokio::test]
async fn another_accounts_address_is_refused_without_saying_whose() {
    let s = server();
    signup(&s, "alice", "a@example.org").await;
    signup(&s, "bob", "b@example.org").await;
    let before = s.mail.sent().len();
    let r = send(
        &s.app,
        call(
            "alice",
            "POST",
            "/v1/me/emails",
            Some(serde_json::json!({"address": "b@example.org"})),
        ),
    )
    .await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::CONFLICT, "email_unavailable")
    );
    assert!(!String::from_utf8_lossy(&r.body).contains("bob"));
    assert_eq!(s.mail.sent().len(), before);

    let r = send(
        &s.app,
        call(
            "alice",
            "POST",
            "/v1/me/emails",
            Some(serde_json::json!({"address": "not an address"})),
        ),
    )
    .await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::BAD_REQUEST, "invalid_request")
    );
}

#[tokio::test]
async fn addresses_are_the_owners_alone_and_need_a_sign_in() {
    let s = server();
    signup(&s, "alice", "a@example.org").await;
    let r = send(
        &s.app,
        Request::get("/v1/me/emails").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let held = emails(&s, "alice").await;
    assert_eq!(held[0].address, "a@example.org");
    let r = send(&s.app, call("alice", "GET", "/v1/me", None)).await;
    assert!(!String::from_utf8_lossy(&r.body).contains("a@example.org"));
}

async fn make_repo(s: &Server, user: &str, name: &str) {
    let r = send(
        &s.app,
        call(
            user,
            "POST",
            "/v1/repos",
            Some(serde_json::json!({"name": name, "visibility": "public"})),
        ),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
}

fn rename(to: &str, password: &str) -> Option<serde_json::Value> {
    Some(serde_json::json!({"username": to, "password": password}))
}

#[tokio::test]
async fn a_rename_needs_the_password_and_keeps_old_addresses_working_with_a_moved_header() {
    let s = server();
    signup(&s, "alice", "a@example.org").await;
    make_repo(&s, "alice", "game").await;
    make_repo(&s, "alice", "tools").await;

    let r = send(
        &s.app,
        call(
            "alice",
            "PUT",
            "/v1/me/username",
            rename("alicia", "wrong password"),
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r = send(
        &s.app,
        call(
            "alice",
            "PUT",
            "/v1/me/username",
            rename("Alicia", PASSWORD),
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);

    let r = send(
        &s.app,
        call(
            "alice",
            "PUT",
            "/v1/me/username",
            rename("alicia", PASSWORD),
        ),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let done: api::Renamed = r.json();
    assert_eq!(
        done,
        api::Renamed {
            old_name: "alice".into(),
            new_name: "alicia".into(),
            repositories: vec!["game".into(), "tools".into()]
        }
    );

    let direct = send(
        &s.app,
        call("alicia", "GET", "/v1/repos/alicia/game/summary", None),
    )
    .await;
    assert_eq!(direct.status, StatusCode::OK);
    assert_eq!(
        direct.moved_to, None,
        "the current address carries no header"
    );
    let old = send(
        &s.app,
        call("alicia", "GET", "/v1/repos/alice/game/summary", None),
    )
    .await;
    assert_eq!(old.status, StatusCode::OK, "the old address resolves");
    assert_eq!(old.moved_to.as_deref(), Some("alicia/game"));
    let missing = send(
        &s.app,
        call("alicia", "GET", "/v1/repos/alice/nothing/summary", None),
    )
    .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    assert_eq!(missing.moved_to, None);

    let r = send(&s.app, call("alicia", "GET", "/v1/me", None)).await;
    assert_eq!(r.status, StatusCode::OK);
}

#[tokio::test]
async fn a_taken_name_and_an_organization_name_refuse_a_rename() {
    let s = server();
    signup(&s, "alice", "a@example.org").await;
    signup(&s, "bob", "b@example.org").await;
    let r = send(
        &s.app,
        call(
            "bob",
            "POST",
            "/v1/orgs",
            Some(serde_json::json!({"name": "acme"})),
        ),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    for taken in ["bob", "acme"] {
        let r = send(
            &s.app,
            call("alice", "PUT", "/v1/me/username", rename(taken, PASSWORD)),
        )
        .await;
        assert_eq!(
            (r.status, r.code().as_str()),
            (StatusCode::CONFLICT, "user_exists"),
            "{taken}"
        );
    }
}

#[tokio::test]
async fn a_live_repository_under_the_old_name_ends_that_redirect_only() {
    let s = server();
    signup(&s, "alice", "a@example.org").await;
    make_repo(&s, "alice", "game").await;
    make_repo(&s, "alice", "tools").await;
    let r = send(
        &s.app,
        call(
            "alice",
            "PUT",
            "/v1/me/username",
            rename("alicia", PASSWORD),
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);

    signup(&s, "alice", "new@example.org").await;
    make_repo(&s, "alice", "game").await;
    let live = send(
        &s.app,
        call("alice", "GET", "/v1/repos/alice/game/summary", None),
    )
    .await;
    assert_eq!(live.status, StatusCode::OK);
    assert_eq!(
        live.moved_to, None,
        "the new holder's repository answers, no redirect"
    );
    let kept = send(
        &s.app,
        call("alicia", "GET", "/v1/repos/alice/tools/summary", None),
    )
    .await;
    assert_eq!(kept.moved_to.as_deref(), Some("alicia/tools"));
}

#[tokio::test]
async fn history_shows_the_current_name_while_the_written_one_stays_for_verification() {
    let s = server();
    signup(&s, "alice", "a@example.org").await;
    make_repo(&s, "alice", "game").await;
    let put = Request::builder()
        .method("PUT")
        .uri("/v1/repos/alice/game/objects")
        .header(api::DEV_USER_HEADER, "alice")
        .body(Body::from("hello"))
        .unwrap();
    let content = send(&s.app, put)
        .await
        .json::<api::PutObjectResponse>()
        .content;
    let checkin = serde_json::json!({"path": "a.txt", "content": content, "base_revision": null, "message": "m"});
    let r = send(
        &s.app,
        call(
            "alice",
            "POST",
            "/v1/repos/alice/game/checkin",
            Some(checkin),
        ),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let fresh: api::Revision = r.json();
    assert_eq!(fresh.author_current, None);

    let r = send(
        &s.app,
        call(
            "alice",
            "PUT",
            "/v1/me/username",
            rename("alicia", PASSWORD),
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);

    let r = send(
        &s.app,
        call("alicia", "GET", "/v1/repos/alicia/game/history", None),
    )
    .await;
    let page: api::HistoryPage = r.json();
    let rev = &page.revisions[0];
    assert_eq!(rev.author, "alice", "the hash covers the name as written");
    assert_eq!(rev.author_current.as_deref(), Some("alicia"));

    let r = send(
        &s.app,
        call("alicia", "GET", "/v1/repos/alicia/game/commits", None),
    )
    .await;
    let commits: api::CommitPage = r.json();
    assert_eq!(commits.commits[0].author.name, "alice");
    assert_eq!(commits.commits[0].author_current.as_deref(), Some("alicia"));
    assert_eq!(
        commits.commits[0].revisions[0].author_current.as_deref(),
        Some("alicia")
    );
}
