//! Authorization and validation limits at the HTTP boundary.

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
use serde_json::json;
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";

async fn server(mode: RegistrationMode) -> Router {
    server_with(mode, false).await
}

async fn server_with(mode: RegistrationMode, require_approval: bool) -> Router {
    server_parts(mode, require_approval).await.0
}

async fn server_parts(
    mode: RegistrationMode,
    require_approval: bool,
) -> (Router, Arc<AccessService>) {
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

    fn message(&self) -> String {
        self.json::<api::ErrorBody>().message
    }

    fn is(&self, status: StatusCode, code: &str) -> bool {
        self.status == status && self.json::<api::ErrorBody>().code == code
    }
}

/// `who` is a dev-header user name (all rights), or a bearer token.
async fn send(
    app: &Router,
    who: &str,
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
) -> Reply {
    let builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    let builder = builder.cred(&Cred::who(who));
    let req = builder
        .body(Body::from(body.map(|b| b.to_string()).unwrap_or_default()))
        .unwrap();
    let r = app.clone().oneshot(req).await.unwrap();
    Reply {
        status: r.status(),
        body: r.into_body().collect().await.unwrap().to_bytes().to_vec(),
    }
}

/// Signs `name` up and returns a full-scope bearer token.
async fn token_for(app: &Router, name: &str) -> String {
    let body = json!({"username": name, "password": PASSWORD});
    send(app, "root", "POST", "/v1/register", Some(body.clone())).await;
    let r = send(app, "root", "POST", "/v1/login", Some(body)).await;
    r.json::<api::CreatedToken>().token
}

async fn grant(app: &Router, owner_token: &str, repo: &str, user: &str, role: &str) -> Reply {
    send(
        app,
        owner_token,
        "PUT",
        &format!("/v1/repos/{repo}/members/{user}"),
        Some(json!({"role": role})),
    )
    .await
}

#[tokio::test]
async fn a_delegated_manager_cannot_change_the_repository_owner() {
    let app = server(RegistrationMode::Open).await;
    let (alice, bob) = (token_for(&app, "alice").await, token_for(&app, "bob").await);
    token_for(&app, "carol").await;
    send(
        &app,
        &alice,
        "POST",
        "/v1/repos",
        Some(json!({"name": "game"})),
    )
    .await;
    assert_eq!(
        grant(&app, &alice, "alice/game", "bob", "admin")
            .await
            .status,
        StatusCode::NO_CONTENT
    );

    let r = grant(&app, &bob, "alice/game", "alice", "reader").await;
    assert!(r.is(StatusCode::FORBIDDEN, "forbidden"), "{:?}", r.status);
    let r = send(&app, &bob, "GET", "/v1/repos/alice/game/members", None).await;
    let members = r.json::<Vec<api::Member>>();
    assert!(
        members
            .iter()
            .any(|m| m.user == "alice" && m.role == "admin")
    );

    assert_eq!(
        grant(&app, &bob, "alice/game", "carol", "reader")
            .await
            .status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        grant(&app, &alice, "alice/game", "alice", "admin")
            .await
            .status,
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn adding_an_account_follows_the_registration_policy() {
    for (mode, status) in [
        (RegistrationMode::Closed, StatusCode::FORBIDDEN),
        (RegistrationMode::InviteOnly, StatusCode::FORBIDDEN),
        (RegistrationMode::Open, StatusCode::CREATED),
    ] {
        let app = server(mode).await;
        send(
            &app,
            "root",
            "POST",
            "/v1/repos",
            Some(json!({"name": "seed"})),
        )
        .await;
        let r = send(
            &app,
            "root",
            "POST",
            "/v1/repos/root/seed/users",
            Some(json!({"username": "alice", "password": PASSWORD, "role": "admin"})),
        )
        .await;
        assert_eq!(r.status, StatusCode::CREATED, "{mode:?}: admin may add");
        let login = send(
            &app,
            "root",
            "POST",
            "/v1/login",
            Some(json!({"username": "alice", "password": PASSWORD})),
        )
        .await;
        let alice = login.json::<api::CreatedToken>().token;
        let r = send(
            &app,
            &alice,
            "POST",
            "/v1/repos/root/seed/users",
            Some(json!({"username": "dora", "password": PASSWORD, "role": "reader"})),
        )
        .await;
        assert_eq!(r.status, status, "{mode:?}");
    }
}

async fn login_token(app: &Router, name: &str) -> String {
    let body = json!({"username": name, "password": PASSWORD});
    let r = send(app, "root", "POST", "/v1/login", Some(body)).await;
    r.json::<api::CreatedToken>().token
}

/// A fine-grained token of `user` for every repository of `root`, minted without the creation checks.
async fn scoped_token(access: &AccessService, user: &str, permissions: &[&str]) -> String {
    let permissions: Vec<Permission> = permissions.iter().map(|p| p.parse().unwrap()).collect();
    access
        .token_for_tests(
            &UserId::new(user),
            &UserId::new("root"),
            permissions,
            Vec::new(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn adding_an_account_needs_a_full_scope_credential() {
    for (mode, approval) in [
        (RegistrationMode::Open, false),
        (RegistrationMode::Closed, false),
        (RegistrationMode::Open, true),
    ] {
        let (app, access) = server_parts(mode, approval).await;
        let plain_open = mode == RegistrationMode::Open && !approval;
        let add = |who: String, name: &'static str| {
            let app = app.clone();
            async move {
                let body = json!({"username": name, "password": PASSWORD, "role": "reader"});
                send(&app, &who, "POST", "/v1/repos/root/seed/users", Some(body)).await
            }
        };
        send(
            &app,
            "root",
            "POST",
            "/v1/repos",
            Some(json!({"name": "seed"})),
        )
        .await;
        for (name, role) in [("wendy", "admin"), ("boss", "admin")] {
            let body = json!({"username": name, "password": PASSWORD, "role": role});
            let r = send(
                &app,
                "root",
                "POST",
                "/v1/repos/root/seed/users",
                Some(body),
            )
            .await;
            assert_eq!(r.status, StatusCode::CREATED, "{mode:?}: {name}");
        }
        let r = send(&app, "root", "PUT", "/v1/admin/users/boss/admin", None).await;
        assert!(r.status.is_success(), "{mode:?}");
        let (wendy, boss) = (
            login_token(&app, "wendy").await,
            login_token(&app, "boss").await,
        );
        let manage = ["manage_users"];
        let wendy_scoped = scoped_token(&access, "wendy", &manage).await;
        let boss_scoped = scoped_token(&access, "boss", &manage).await;
        let minted = send(
            &app,
            "root",
            "POST",
            "/v1/admin/service-credentials",
            Some(json!({"name": "svc", "scopes": ["manage_accounts"]})),
        )
        .await;
        let service = minted.json::<api::CreatedServiceCredential>().secret;

        let anon = Request::builder()
            .method("POST")
            .uri("/v1/repos/root/seed/users")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"username": "anon", "password": PASSWORD, "role": "reader"}).to_string(),
            ))
            .unwrap();
        let r = app.clone().oneshot(anon).await.unwrap();
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED, "{mode:?}: signed out");

        let refused = [
            ("scoped token of a normal user", wendy_scoped, "forbidden"),
            ("scoped token of an administrator", boss_scoped, "forbidden"),
            (
                "service credential",
                service,
                "service_credential_not_allowed",
            ),
        ];
        for (n, (what, who, code)) in refused.into_iter().enumerate() {
            let name = ["x0", "x1", "x2"][n];
            let r = add(who, name).await;
            assert!(
                r.is(StatusCode::FORBIDDEN, code),
                "{mode:?} {what}: {}",
                r.message()
            );
        }
        let r = send(&app, "root", "GET", "/v1/repos/root/seed/members", None).await;
        let members = String::from_utf8_lossy(&r.body).into_owned();
        assert!(
            !members.contains("\"x0\"") && !members.contains("\"x1\""),
            "{members}"
        );

        let r = add(wendy, "n1").await;
        if plain_open {
            assert_eq!(r.status, StatusCode::CREATED, "{mode:?}: full-scope member");
        } else {
            assert!(
                r.is(StatusCode::FORBIDDEN, "forbidden"),
                "{mode:?}: full-scope member"
            );
        }
        for (what, who) in [("admin token", boss), ("dev", "root".to_string())] {
            let name = if what == "dev" { "a2" } else { "a1" };
            let r = add(who, name).await;
            assert_eq!(r.status, StatusCode::CREATED, "{mode:?} {what}");
        }
    }
}

#[tokio::test]
async fn invitation_lifetimes_are_bounded() {
    let app = server(RegistrationMode::Open).await;
    let alice = token_for(&app, "alice").await;
    send(
        &app,
        &alice,
        "POST",
        "/v1/repos",
        Some(json!({"name": "game"})),
    )
    .await;
    for hours in [
        json!(0),
        json!(-5),
        json!(8761),
        json!(i64::MAX),
        json!(i64::MIN),
    ] {
        let r = send(
            &app,
            &alice,
            "POST",
            "/v1/repos/alice/game/invites",
            Some(json!({"role": "reader", "hours": hours})),
        )
        .await;
        assert!(r.is(StatusCode::BAD_REQUEST, "invalid_request"), "{hours}");
    }
    for hours in [1, 8760] {
        let r = send(
            &app,
            &alice,
            "POST",
            "/v1/repos/alice/game/invites",
            Some(json!({"role": "reader", "hours": hours})),
        )
        .await;
        assert_eq!(r.status, StatusCode::OK, "{hours}");
    }
}

#[tokio::test]
async fn a_manager_sees_only_tokens_scoped_to_repositories_they_manage() {
    let app = server(RegistrationMode::Open).await;
    let (alice, bob, carol) = (
        token_for(&app, "alice").await,
        token_for(&app, "bob").await,
        token_for(&app, "carol").await,
    );
    let r = send(
        &app,
        &alice,
        "POST",
        "/v1/orgs",
        Some(json!({"name": "acme"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED);
    for (user, token) in [("bob", &bob), ("carol", &carol)] {
        let r = send(
            &app,
            &alice,
            "POST",
            "/v1/orgs/acme/members",
            Some(json!({"user": user})),
        )
        .await;
        assert_eq!(r.status, StatusCode::ACCEPTED, "{user}");
        let r = send(&app, token, "POST", "/v1/me/invitations/acme/accept", None).await;
        assert_eq!(r.status, StatusCode::OK, "{user}");
    }
    for name in ["game", "vault"] {
        let r = send(
            &app,
            &alice,
            "POST",
            "/v1/repos",
            Some(json!({"name": name, "owner": "acme"})),
        )
        .await;
        assert_eq!(r.status, StatusCode::CREATED, "{name}");
        grant(&app, &alice, &format!("acme/{name}"), "carol", "writer").await;
    }
    grant(&app, &alice, "acme/game", "bob", "admin").await;
    let mut ids = Vec::new();
    for (name, repos) in [
        ("g", vec!["acme/game"]),
        ("v", vec!["acme/vault"]),
        ("both", vec!["acme/game", "acme/vault"]),
    ] {
        let r = send(
            &app,
            &carol,
            "POST",
            "/v1/tokens",
            Some(cred::token_body(name, repos, ["read"])),
        )
        .await;
        ids.push(r.json::<api::CreatedToken>().info.id);
    }

    let r = send(&app, &bob, "GET", "/v1/tokens?user=carol", None).await;
    let seen = r.json::<Vec<api::TokenInfo>>();
    assert_eq!(
        seen.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
        ["g"]
    );
    assert!(
        seen.iter()
            .all(|t| t.repository_access.repos == ["acme/game"])
    );
    for id in &ids[1..] {
        let r = send(&app, &bob, "DELETE", &format!("/v1/tokens/{id}"), None).await;
        assert!(r.is(StatusCode::NOT_FOUND, "token_not_found"));
    }
    let r = send(
        &app,
        &bob,
        "DELETE",
        &format!("/v1/tokens/{}", ids[0]),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn new_writes_refuse_reserved_and_oversized_paths_and_text() {
    let app = server(RegistrationMode::Open).await;
    let alice = token_for(&app, "alice").await;
    send(
        &app,
        &alice,
        "POST",
        "/v1/repos",
        Some(json!({"name": "game"})),
    )
    .await;
    let blob = {
        let req = Request::builder()
            .method("PUT")
            .uri("/v1/repos/alice/game/objects")
            .cred(&Cred::bearer(&alice))
            .body(Body::from("hello"))
            .unwrap();
        let r = app.clone().oneshot(req).await.unwrap();
        let body = r.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice::<api::PutObjectResponse>(&body)
            .unwrap()
            .content
    };
    let long = format!("{}/a", "d".repeat(256));
    for path in [
        ".git/config",
        "a/.GIT/x",
        "GIT~1/config",
        "a/git~12",
        ".pyn/local_only/state",
        "a\u{1}b",
        &long,
    ] {
        let r = send(
            &app,
            &alice,
            "POST",
            "/v1/repos/alice/game/checkin",
            Some(json!({"path": path, "content": blob, "message": "m"})),
        )
        .await;
        assert!(r.is(StatusCode::BAD_REQUEST, "invalid_path"), "{path:?}");
    }
    let r = send(
        &app,
        &alice,
        "POST",
        "/v1/repos/alice/game/checkin",
        Some(json!({"path": "docs/GIT~1/x", "content": blob, "message": "m"})),
    )
    .await;
    assert!(r.message().contains("\"GIT~1\""), "{}", r.message());
    for (route, body) in [
        ("checkout", json!({"path": "GIT~1/x"})),
        (
            "changesets",
            json!({"message": "m", "entries": [{"path": "a/git~1", "content": blob}]}),
        ),
    ] {
        let r = send(
            &app,
            &alice,
            "POST",
            &format!("/v1/repos/alice/game/{route}"),
            Some(body),
        )
        .await;
        assert!(r.is(StatusCode::BAD_REQUEST, "invalid_path"), "{route}");
    }
    let r = send(
        &app,
        &alice,
        "POST",
        "/v1/repos/alice/game/checkin",
        Some(json!({"path": "ok.txt", "content": blob, "message": "x".repeat(70_000)})),
    )
    .await;
    assert!(r.is(StatusCode::BAD_REQUEST, "invalid_request"));
    let r = send(
        &app,
        &alice,
        "POST",
        "/v1/repos/alice/game/checkin",
        Some(json!({"path": ".pyn/ignore", "content": blob, "message": "m"})),
    )
    .await;
    assert_ne!(r.status, StatusCode::BAD_REQUEST);

    for name in ["a\nb".to_string(), "n".repeat(300)] {
        let r = send(
            &app,
            &alice,
            "POST",
            "/v1/tokens",
            Some(cred::token_body(&name, ["alice/game"], ["read"])),
        )
        .await;
        assert!(r.is(StatusCode::BAD_REQUEST, "invalid_request"));
    }
}

/// The body with the address (and ids) the caller asked about replaced, so two answers compare byte for byte.
fn masked(r: &Reply, replace: &[(&str, &str)]) -> (StatusCode, String) {
    let mut text = String::from_utf8(r.body.clone()).unwrap();
    for (from, to) in replace {
        text = text.replace(from, to);
    }
    (r.status, text)
}

#[tokio::test]
async fn a_missing_repository_and_one_the_caller_cannot_see_answer_identically() {
    let app = server(RegistrationMode::Open).await;
    let (alice, bob) = (token_for(&app, "alice").await, token_for(&app, "bob").await);
    let reader = token_for(&app, "reader").await;
    send(
        &app,
        &alice,
        "POST",
        "/v1/repos",
        Some(json!({"name": "vault", "visibility": "private"})),
    )
    .await;
    grant(&app, &alice, "alice/vault", "reader", "reader").await;
    send(
        &app,
        &bob,
        "POST",
        "/v1/repos",
        Some(json!({"name": "own"})),
    )
    .await;
    let scoped = send(
        &app,
        &bob,
        "POST",
        "/v1/tokens",
        Some(cred::token_body("t", ["bob/own"], ["read"])),
    )
    .await
    .json::<api::CreatedToken>()
    .token;

    let routes = [
        ("GET", "", None),
        ("GET", "/locks", None),
        ("GET", "/members", None),
        ("GET", "/history", None),
        ("POST", "/checkout", Some(json!({"path": "a.txt"}))),
        ("POST", "/push", Some(json!({"commits": [], "trees": []}))),
    ];
    for who in [&bob, &scoped] {
        for (method, suffix, body) in &routes {
            let hidden = send(
                &app,
                who,
                method,
                &format!("/v1/repos/alice/vault{suffix}"),
                body.clone(),
            )
            .await;
            let absent = send(
                &app,
                who,
                method,
                &format!("/v1/repos/alice/nothing{suffix}"),
                body.clone(),
            )
            .await;
            assert!(
                hidden.is(StatusCode::NOT_FOUND, "repo_not_found"),
                "{method} {suffix} {:?} {}",
                hidden.status,
                String::from_utf8_lossy(&hidden.body)
            );
            assert_eq!(
                masked(&hidden, &[("alice/vault", "X")]),
                masked(&absent, &[("alice/nothing", "X")]),
                "{method} {suffix}"
            );
        }
    }

    let denied = send(
        &app,
        &reader,
        "POST",
        "/v1/repos/alice/vault/checkout",
        Some(json!({"path": "a.txt"})),
    )
    .await;
    assert!(denied.is(StatusCode::FORBIDDEN, "forbidden"));
}

#[tokio::test]
async fn an_unknown_token_and_a_token_the_caller_cannot_manage_answer_identically() {
    let app = server(RegistrationMode::Open).await;
    let (alice, bob) = (token_for(&app, "alice").await, token_for(&app, "bob").await);
    send(
        &app,
        &alice,
        "POST",
        "/v1/repos",
        Some(json!({"name": "game"})),
    )
    .await;
    let id = send(
        &app,
        &alice,
        "POST",
        "/v1/tokens",
        Some(cred::token_body("ci", ["alice/game"], ["read"])),
    )
    .await
    .json::<api::CreatedToken>()
    .info
    .id;
    let missing = "0".repeat(id.len());
    let foreign = send(&app, &bob, "DELETE", &format!("/v1/tokens/{id}"), None).await;
    let absent = send(&app, &bob, "DELETE", &format!("/v1/tokens/{missing}"), None).await;
    assert!(foreign.is(StatusCode::NOT_FOUND, "token_not_found"));
    assert_eq!(
        masked(&foreign, &[(&id, "X")]),
        masked(&absent, &[(&missing, "X")])
    );
    let still = send(&app, &alice, "DELETE", &format!("/v1/tokens/{id}"), None).await;
    assert_eq!(still.status, StatusCode::NO_CONTENT);

    let (known, secret) = alice.rsplit_once('_').unwrap();
    let wrong_secret = format!("{known}_{}", "f".repeat(secret.len()));
    let unknown_id = format!("pyn_{}_{}", "e".repeat(12), "f".repeat(secret.len()));
    let a = send(&app, &wrong_secret, "GET", "/v1/repos", None).await;
    let b = send(&app, &unknown_id, "GET", "/v1/repos", None).await;
    assert_eq!(a.status, StatusCode::UNAUTHORIZED);
    assert_eq!(masked(&a, &[]), masked(&b, &[]));
}
