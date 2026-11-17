//! Fine-grained tokens over HTTP: owner, expiry, repository access, grants and what a token may reach.

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
    AccessConfig, AccessService, AuthProvider, RegistrationMode, Repositories, Rules, SystemClock,
    UserId,
};
use pyn_proto as api;
use pyn_server::auth::{BearerAuth, DevHeaderAuth};
use pyn_server::{AppState, router};
use serde_json::{Value, json};
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";

async fn server() -> Router {
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
            registration: RegistrationMode::Open,
            require_email_verification: false,
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

/// Signs `name` up and returns a sign-in token.
async fn account(app: &Router, name: &str) -> String {
    let body = json!({"username": name, "password": PASSWORD});
    send(app, "root", "POST", "/v1/register", Some(body.clone())).await;
    let r = send(app, "root", "POST", "/v1/login", Some(body)).await;
    r.json::<api::CreatedToken>().token
}

async fn repo(app: &Router, who: &str, owner: &str, name: &str, visibility: &str) {
    let body = json!({"owner": owner, "name": name, "visibility": visibility});
    let r = send(app, who, "POST", "/v1/repos", Some(body)).await;
    assert_eq!(r.status, StatusCode::CREATED, "{owner}/{name}");
}

fn body(owner: &str, mode: &str, repos: &[&str], permissions: &[(&str, &str)]) -> Value {
    json!({
        "name": "ci",
        "owner": owner,
        "repository_access": {"mode": mode, "repos": repos},
        "permissions": permissions
            .iter()
            .map(|(resource, level)| json!({"resource": resource, "level": level}))
            .collect::<Vec<_>>(),
    })
}

async fn create(app: &Router, who: &str, req: Value) -> Reply {
    send(app, who, "POST", "/v1/tokens", Some(req)).await
}

async fn status(app: &Router, token: &str, method: &str, uri: &str) -> StatusCode {
    send(app, token, method, uri, None).await.status
}

struct World {
    app: Router,
    alice: String,
    bob: String,
}

/// alice owns `alice/game` and `alice/lobby` (private) and the organization `acme` with `acme/tools`; bob is a
/// member of `acme` and owns the public `bob/open`.
async fn world() -> World {
    let app = server().await;
    let (alice, bob) = (account(&app, "alice").await, account(&app, "bob").await);
    repo(&app, &alice, "alice", "game", "private").await;
    repo(&app, &alice, "alice", "lobby", "private").await;
    repo(&app, &bob, "bob", "open", "public").await;
    let r = send(
        &app,
        &alice,
        "POST",
        "/v1/orgs",
        Some(json!({"name": "acme"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED);
    let r = send(
        &app,
        &alice,
        "POST",
        "/v1/orgs/acme/members",
        Some(json!({"user": "bob"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::ACCEPTED);
    let r = send(&app, &bob, "POST", "/v1/me/invitations/acme/accept", None).await;
    assert_eq!(r.status, StatusCode::OK);
    repo(&app, &alice, "acme", "tools", "private").await;
    World { app, alice, bob }
}

const WRITE: &[(&str, &str)] = &[("contents", "write"), ("locks", "write")];

#[tokio::test]
async fn a_token_carries_its_owner_access_grants_and_a_thirty_day_default_expiry() {
    let w = world().await;
    let r = create(
        &w.app,
        &w.alice,
        body("alice", "selected", &["alice/game"], WRITE),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    let made = r.json::<api::CreatedToken>();
    let info = made.info;
    assert_eq!(
        (info.owner.as_str(), info.user.as_str()),
        ("alice", "alice")
    );
    assert_eq!(info.repository_access.mode, "selected");
    assert_eq!(info.repository_access.repos, ["alice/game"]);
    let grants: Vec<_> = info
        .permissions
        .iter()
        .map(|p| (p.resource.as_str(), p.level.as_str()))
        .collect();
    assert_eq!(
        grants,
        [
            ("metadata", "read"),
            ("contents", "write"),
            ("locks", "write")
        ],
        "metadata read is implied"
    );
    assert!(!info.sign_in);
    let days = (info.expires_at.unwrap() - info.created_at).num_days();
    assert!((29..=30).contains(&days), "{days}");

    let mut never = body("alice", "all", &[], WRITE);
    never["never"] = json!(true);
    let r = create(&w.app, &w.alice, never).await;
    let capped = r.json::<api::CreatedToken>().info.expires_at;
    let r = send(&w.app, &w.alice, "GET", "/v1/tokens", None).await;
    let sign_in = r
        .json::<Vec<api::TokenInfo>>()
        .into_iter()
        .find(|t| t.sign_in);
    assert_eq!(
        capped,
        sign_in.unwrap().expires_at,
        "a token minted by a sign-in token never outlives it"
    );

    let at = chrono::Utc::now() + chrono::Duration::days(7);
    let mut dated = body("alice", "all", &[], WRITE);
    dated["expires_at"] = json!(at);
    let r = create(&w.app, &w.alice, dated).await;
    assert_eq!(r.json::<api::CreatedToken>().info.expires_at, Some(at));

    let mut both = body("alice", "all", &[], WRITE);
    both["never"] = json!(true);
    both["expires_at"] = json!(at);
    let r = create(&w.app, &w.alice, both).await;
    assert!(r.is(StatusCode::BAD_REQUEST, "invalid_request"));

    let r = send(&w.app, &w.alice, "GET", "/v1/tokens", None).await;
    let listed = r.json::<Vec<api::TokenInfo>>();
    assert!(
        listed
            .iter()
            .any(|t| t.sign_in && t.repository_access.mode == "all")
    );
    assert_eq!(listed.iter().filter(|t| !t.sign_in).count(), 3);
}

#[tokio::test]
async fn invalid_requests_are_refused_with_existing_codes() {
    let w = world().await;
    let bad = [
        body("alice", "everything", &[], WRITE),
        body("alice", "all", &[], &[("billing", "read")]),
        body("alice", "all", &[], &[("contents", "owner")]),
        body("alice", "all", &[], &[("contents", "admin")]),
        body("alice", "all", &[], &[("audit", "write")]),
        body(
            "alice",
            "all",
            &[],
            &[("contents", "read"), ("contents", "write")],
        ),
        body("alice", "all", &[], &[]),
        body("alice", "public", &[], WRITE),
        body("alice", "all", &["alice/game"], WRITE),
        body("alice", "selected", &[], WRITE),
        body("alice", "selected", &["acme/tools"], WRITE),
    ];
    for (n, req) in bad.into_iter().enumerate() {
        let r = create(&w.app, &w.alice, req).await;
        assert!(
            r.is(StatusCode::BAD_REQUEST, "invalid_request"),
            "{n}: {:?}",
            r.status
        );
    }
    let r = create(&w.app, &w.alice, body("bob", "all", &[], WRITE)).await;
    assert!(r.is(StatusCode::FORBIDDEN, "not_org_member"));
    let r = create(&w.app, &w.bob, body("acme", "all", &[], WRITE)).await;
    assert!(
        r.is(StatusCode::FORBIDDEN, "forbidden"),
        "a member with no role in any of the organization's repositories gets a useless token refused"
    );
    let r = send(
        &w.app,
        &w.alice,
        "PUT",
        "/v1/repos/acme/tools/members/bob",
        Some(json!({"role": "writer"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let r = create(&w.app, &w.bob, body("acme", "all", &[], WRITE)).await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "a member may create tokens for the organization"
    );
    let r = create(
        &w.app,
        &w.alice,
        body("alice", "selected", &["alice/nope"], WRITE),
    )
    .await;
    assert!(r.is(StatusCode::NOT_FOUND, "repo_not_found"));
}

#[tokio::test]
async fn a_token_reaches_only_repositories_its_owner_owns_and_its_access_covers() {
    let w = world().await;
    let mint = |owner: &'static str, mode: &'static str, repos: &'static [&'static str]| {
        let (app, alice) = (w.app.clone(), w.alice.clone());
        async move {
            let r = create(&app, &alice, body(owner, mode, repos, WRITE)).await;
            r.json::<api::CreatedToken>().token
        }
    };
    let reads = |token: String| {
        let app = w.app.clone();
        async move {
            let mut out = Vec::new();
            for uri in [
                "/v1/repos/alice/game/files",
                "/v1/repos/alice/lobby/files",
                "/v1/repos/acme/tools/files",
            ] {
                out.push(status(&app, &token, "GET", uri).await == StatusCode::OK);
            }
            out
        }
    };
    assert_eq!(
        reads(mint("alice", "selected", &["alice/game"]).await).await,
        [true, false, false]
    );
    assert_eq!(
        reads(mint("alice", "all", &[]).await).await,
        [true, true, false]
    );
    assert_eq!(
        reads(mint("acme", "all", &[]).await).await,
        [false, false, true]
    );
    let public = create(&w.app, &w.alice, body("alice", "public", &[], &[])).await;
    assert_eq!(
        reads(public.json::<api::CreatedToken>().token).await,
        [false, false, false]
    );
}

#[tokio::test]
async fn a_token_reads_public_repositories_but_never_changes_them() {
    let w = world().await;
    let r = create(&w.app, &w.alice, body("alice", "all", &[], WRITE)).await;
    let token = r.json::<api::CreatedToken>().token;
    assert_eq!(
        status(&w.app, &token, "GET", "/v1/repos/bob/open/files").await,
        StatusCode::OK,
        "public repositories stay readable"
    );
    let upload = |repo: &'static str| {
        let (app, token) = (w.app.clone(), token.clone());
        async move {
            let req = Request::builder()
                .method("PUT")
                .uri(format!("/v1/repos/{repo}/objects"))
                .cred(&Cred::bearer(&token))
                .body(Body::from("hello"))
                .unwrap();
            app.oneshot(req).await.unwrap().status()
        }
    };
    assert_eq!(
        upload("alice/game").await,
        StatusCode::OK,
        "its own repositories are writable"
    );
    let refused = upload("bob/open").await;
    assert!(
        refused.is_client_error(),
        "no writes to a public repository the token does not cover: {refused}"
    );
}

#[tokio::test]
async fn a_token_cannot_hold_more_than_its_creator_and_demotion_narrows_it() {
    let w = world().await;
    let r = send(
        &w.app,
        &w.alice,
        "PUT",
        "/v1/repos/acme/tools/members/bob",
        Some(json!({"role": "reader"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let r = create(
        &w.app,
        &w.bob,
        body("acme", "selected", &["acme/tools"], WRITE),
    )
    .await;
    assert!(
        r.is(StatusCode::FORBIDDEN, "forbidden"),
        "a reader cannot mint write grants"
    );
    let r = create(
        &w.app,
        &w.bob,
        body("acme", "selected", &["acme/tools"], &[("contents", "read")]),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    let token = r.json::<api::CreatedToken>().token;
    assert_eq!(
        status(&w.app, &token, "GET", "/v1/repos/acme/tools/files").await,
        StatusCode::OK
    );
    let r = send(
        &w.app,
        &w.alice,
        "DELETE",
        "/v1/orgs/acme/members/bob",
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert_eq!(
        status(&w.app, &token, "GET", "/v1/repos/acme/tools/files").await,
        StatusCode::NOT_FOUND,
        "a creator who lost the role leaves the token with nothing"
    );
}

#[tokio::test]
async fn only_a_sign_in_token_is_full_scope_even_beside_a_token_with_every_grant() {
    let w = world().await;
    let every = [
        ("metadata", "read"),
        ("contents", "write"),
        ("locks", "admin"),
        ("policy", "write"),
        ("members", "write"),
        ("administration", "write"),
        ("audit", "read"),
    ];
    let r = create(&w.app, &w.alice, body("alice", "all", &[], &every)).await;
    let fine = r.json::<api::CreatedToken>().token;
    for (method, uri, req) in [
        ("POST", "/v1/tokens", Some(body("alice", "all", &[], WRITE))),
        ("GET", "/v1/tokens", None),
        ("GET", "/v1/keys", None),
    ] {
        let r = send(&w.app, &fine, method, uri, req).await;
        assert!(r.is(StatusCode::FORBIDDEN, "forbidden"), "{method} {uri}");
    }
    let me = send(&w.app, &fine, "GET", "/v1/me", None).await;
    assert_eq!(me.json::<api::Account>().has_password, None);
    let me = send(&w.app, &w.alice, "GET", "/v1/me", None).await;
    assert_eq!(me.json::<api::Account>().has_password, Some(true));
    assert_eq!(
        status(&w.app, &w.alice, "GET", "/v1/tokens").await,
        StatusCode::OK
    );
    let r = send(&w.app, &w.alice, "GET", "/v1/tokens", None).await;
    let signed_in = r.json::<Vec<api::TokenInfo>>();
    let sign_in = signed_in.iter().find(|t| t.sign_in).unwrap();
    assert_eq!(sign_in.owner, "alice");
    assert_eq!(sign_in.repository_access.mode, "all");
    assert_eq!(sign_in.permissions.len(), 7);
}

async fn member_of_one_repository() -> World {
    let w = world().await;
    repo(&w.app, &w.alice, "acme", "design", "private").await;
    let r = send(
        &w.app,
        &w.alice,
        "PUT",
        "/v1/repos/acme/tools/members/bob",
        Some(json!({"role": "writer"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    w
}

#[tokio::test]
async fn an_organization_member_cannot_reach_a_repository_they_lack_through_a_token() {
    let w = member_of_one_repository().await;
    for repos in [&["acme/design"][..], &["acme/tools", "acme/design"][..]] {
        let r = create(&w.app, &w.bob, body("acme", "selected", repos, WRITE)).await;
        assert!(
            r.is(StatusCode::NOT_FOUND, "repo_not_found"),
            "a repository the creator cannot see does not exist for them: {repos:?}"
        );
    }
    let r = create(
        &w.app,
        &w.bob,
        body("acme", "selected", &["acme/tools"], WRITE),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);

    let r = create(&w.app, &w.bob, body("acme", "all", &[], WRITE)).await;
    assert_eq!(r.status, StatusCode::OK);
    let token = r.json::<api::CreatedToken>().token;
    assert_eq!(
        status(&w.app, &token, "GET", "/v1/repos/acme/tools/files").await,
        StatusCode::OK
    );
    let restore = json!({"path": "x", "revision": 1, "base_revision": 2, "confirm": "x@r2"});
    let by_token = send(
        &w.app,
        &token,
        "POST",
        "/v1/repos/acme/tools/restore",
        Some(restore.clone()),
    )
    .await;
    assert!(
        by_token.is(StatusCode::FORBIDDEN, "forbidden"),
        "restore is beyond the writer role: {:?}",
        by_token.status
    );
    let in_y = send(&w.app, &token, "GET", "/v1/repos/acme/design/files", None).await;
    let missing = send(&w.app, &token, "GET", "/v1/repos/acme/nope/files", None).await;
    assert!(missing.is(StatusCode::NOT_FOUND, "repo_not_found"));
    assert_eq!(
        (in_y.status, in_y.json::<api::ErrorBody>().code),
        (missing.status, missing.json::<api::ErrorBody>().code)
    );
    let r = send(
        &w.app,
        &token,
        "POST",
        "/v1/repos/acme/design/restore",
        Some(restore),
    )
    .await;
    assert!(r.is(StatusCode::NOT_FOUND, "repo_not_found"));
}

#[tokio::test]
async fn demoting_a_member_narrows_their_organization_token() {
    let w = member_of_one_repository().await;
    let r = create(&w.app, &w.bob, body("acme", "all", &[], WRITE)).await;
    let token = r.json::<api::CreatedToken>().token;
    let upload = || {
        let (app, token) = (w.app.clone(), token.clone());
        async move {
            let req = Request::builder()
                .method("PUT")
                .uri("/v1/repos/acme/tools/objects")
                .cred(&Cred::bearer(&token))
                .body(Body::from("hello"))
                .unwrap();
            app.oneshot(req).await.unwrap().status()
        }
    };
    assert_eq!(upload().await, StatusCode::OK);
    let r = send(
        &w.app,
        &w.alice,
        "PUT",
        "/v1/repos/acme/tools/members/bob",
        Some(json!({"role": "reader"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert_eq!(upload().await, StatusCode::FORBIDDEN);
    assert_eq!(
        status(&w.app, &token, "GET", "/v1/repos/acme/tools/files").await,
        StatusCode::OK,
        "reading stays"
    );
}

#[tokio::test]
async fn a_grant_the_creator_does_not_hold_is_refused_and_a_collaborators_token_stays_at_home() {
    let w = member_of_one_repository().await;
    let r = send(
        &w.app,
        &w.alice,
        "PUT",
        "/v1/repos/acme/tools/members/bob",
        Some(json!({"role": "reader"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    for grants in [
        &[("contents", "read"), ("locks", "write")][..],
        &[("contents", "read"), ("policy", "write")][..],
    ] {
        let r = create(
            &w.app,
            &w.bob,
            body("acme", "selected", &["acme/tools"], grants),
        )
        .await;
        assert!(r.is(StatusCode::FORBIDDEN, "forbidden"), "{grants:?}");
    }

    let r = send(
        &w.app,
        &w.alice,
        "PUT",
        "/v1/repos/alice/game/members/bob",
        Some(json!({"role": "writer"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let files = "/v1/repos/alice/game/files";
    assert_eq!(status(&w.app, &w.bob, "GET", files).await, StatusCode::OK);
    let r = create(
        &w.app,
        &w.bob,
        body("bob", "selected", &["alice/game"], WRITE),
    )
    .await;
    assert!(r.is(StatusCode::BAD_REQUEST, "invalid_request"));
    let r = create(
        &w.app,
        &w.bob,
        body("bob", "selected", &["bob/open"], WRITE),
    )
    .await;
    let token = r.json::<api::CreatedToken>().token;
    assert_eq!(
        status(&w.app, &token, "GET", files).await,
        StatusCode::NOT_FOUND
    );
}
