//! The role and the locks `GET /v1/repos/{owner}/{name}`, `/v1/explore/repos`, `/v1/users/{user}` and `/v1/me/locks`
//! show a repository-scoped credential: only what its grant covers, never the owner's standing elsewhere.

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
use serde_json::json;
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";
const RULES: &str = "[meta]\ndefault = \"shared\"\n[exclusive]\npaths = [\"Content/\"]\n";

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
        Rules::from_toml(RULES).unwrap(),
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
    set_cookie: Option<String>,
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

async fn call(
    app: &Router,
    cred: &Cred,
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
) -> Reply {
    let mut b = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    b = b.cred(cred);
    let body = body.map(|b| b.to_string()).unwrap_or_default();
    let r = app
        .clone()
        .oneshot(b.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let (parts, body) = r.into_parts();
    Reply {
        status: parts.status,
        set_cookie: parts
            .headers
            .get("set-cookie")
            .map(|v| v.to_str().unwrap().to_string()),
        body: body.collect().await.unwrap().to_bytes().to_vec(),
    }
}

struct World {
    app: Router,
    session: Cred,
    full: Cred,
    scoped: Cred,
    read_only: Cred,
    service: Cred,
}

async fn token(app: &Router, who: &Cred, perms: &[&str]) -> Cred {
    let body = json!({"name": "ci", "permissions": perms, "repos": ["alice/game"]});
    let r = call(app, who, "POST", "/v1/tokens", Some(body)).await;
    assert!(
        r.status.is_success(),
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    Cred::token(&r.json::<api::CreatedToken>().token)
}

/// alice has a hidden bio and a repository; each credential kind of hers (and a service credential) is minted.
async fn world() -> World {
    let app = server().await;
    let body = json!({"username": "alice", "password": PASSWORD});
    let r = call(
        &app,
        &Cred::SignedOut,
        "POST",
        "/v1/register",
        Some(body.clone()),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED);
    let r = call(
        &app,
        &Cred::SignedOut,
        "POST",
        "/v1/login",
        Some(body.clone()),
    )
    .await;
    let full = Cred::token(&r.json::<api::CreatedToken>().token);
    let r = call(&app, &Cred::SignedOut, "POST", "/v1/session", Some(body)).await;
    let cookie = r.set_cookie.clone().unwrap();
    let cookie = cookie.split(';').next().unwrap();
    let session = Cred::session_read(cookie.strip_prefix("pyn_session=").unwrap());

    for name in ["game", "lobby"] {
        let body = json!({"owner": "alice", "name": name, "visibility": "public"});
        let r = call(&app, &full, "POST", "/v1/repos", Some(body)).await;
        assert_eq!(
            r.status,
            StatusCode::CREATED,
            "{}",
            String::from_utf8_lossy(&r.body)
        );
        let uri = format!("/v1/repos/alice/{name}/checkout");
        let r = call(
            &app,
            &full,
            "POST",
            &uri,
            Some(json!({"path": "Content/a.umap"})),
        )
        .await;
        assert_eq!(
            r.status,
            StatusCode::OK,
            "{}",
            String::from_utf8_lossy(&r.body)
        );
    }
    let r = call(
        &app,
        &full,
        "POST",
        "/v1/repos",
        Some(json!({"owner": "alice", "name": "vault", "visibility": "private"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED);

    let scoped = token(&app, &full, &["read", "lock", "checkin"]).await;
    let read_only = token(&app, &full, &["read"]).await;
    let r = call(
        &app,
        &Cred::dev("root"),
        "POST",
        "/v1/admin/service-credentials",
        Some(json!({"name": "billing", "scopes": ["manage_accounts"]})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED);
    let service = Cred::service(&r.json::<api::CreatedServiceCredential>().secret);
    World {
        app,
        session,
        full,
        scoped,
        read_only,
        service,
    }
}

/// The account each credential resolves to, asked of `GET /v1/me`, so a case cannot pass as someone else.
async fn assert_is_alice(w: &World, cred: &Cred, label: &str) {
    let r = call(&w.app, cred, "GET", "/v1/me", None).await;
    assert_eq!(r.status, StatusCode::OK, "{label}");
    assert_eq!(r.json::<api::Account>().user, "alice", "{label}");
}

fn role_of(repos: &[api::RepoInfo], name: &str, label: &str) -> Option<String> {
    repos
        .iter()
        .find(|r| r.name == name)
        .unwrap_or_else(|| panic!("{label}: {name} missing"))
        .role
        .clone()
}

/// The role per repository on each route that reports one: `(get game, get lobby, explore, profile)`.
async fn roles(w: &World, cred: &Cred, label: &str) -> [(Option<String>, Option<String>); 3] {
    let get = |name: &'static str| async move {
        let r = call(
            &w.app,
            cred,
            "GET",
            &format!("/v1/repos/alice/{name}"),
            None,
        )
        .await;
        assert_eq!(r.status, StatusCode::OK, "{label}: {name}");
        r.json::<api::RepoInfo>().role
    };
    let explore = call(&w.app, cred, "GET", "/v1/explore/repos", None).await;
    assert_eq!(explore.status, StatusCode::OK, "{label}");
    let explore = explore.json::<api::RepoPage>().repos;
    let profile = call(&w.app, cred, "GET", "/v1/users/alice", None).await;
    assert_eq!(profile.status, StatusCode::OK, "{label}");
    let profile = profile.json::<api::PublicProfile>().repositories;
    [
        (get("game").await, get("lobby").await),
        (
            role_of(&explore, "game", label),
            role_of(&explore, "lobby", label),
        ),
        (
            role_of(&profile, "game", label),
            role_of(&profile, "lobby", label),
        ),
    ]
}

async fn lock_repos(w: &World, cred: &Cred, label: &str) -> Vec<String> {
    let r = call(&w.app, cred, "GET", "/v1/me/locks", None).await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "{label}: {}",
        String::from_utf8_lossy(&r.body)
    );
    r.json::<Vec<api::MyLock>>()
        .into_iter()
        .map(|l| format!("{}/{}", l.owner, l.name))
        .collect()
}

#[tokio::test]
async fn full_scope_credentials_keep_their_roles_and_locks() {
    let w = world().await;
    let admin = Some("admin".to_string());
    for (label, cred) in [
        ("session", &w.session),
        ("full token", &w.full),
        ("dev", &Cred::dev("alice")),
    ] {
        if label != "dev" {
            assert_is_alice(&w, cred, label).await;
        }
        for (game, lobby) in roles(&w, cred, label).await {
            assert_eq!((game, lobby), (admin.clone(), admin.clone()), "{label}");
        }
        assert_eq!(
            lock_repos(&w, cred, label).await,
            ["alice/game", "alice/lobby"],
            "{label}"
        );
    }
}

#[tokio::test]
async fn a_scoped_token_has_a_role_and_locks_only_inside_its_grant() {
    let w = world().await;
    for (label, cred) in [("scoped", &w.scoped), ("read-only", &w.read_only)] {
        assert_is_alice(&w, cred, label).await;
        for (game, lobby) in roles(&w, cred, label).await {
            assert_eq!(game.as_deref(), Some("admin"), "{label}: in the grant");
            assert_eq!(lobby, None, "{label}: outside the grant");
        }
        assert_eq!(lock_repos(&w, cred, label).await, ["alice/game"], "{label}");
    }
}

#[tokio::test]
async fn a_scoped_token_still_reads_a_public_repository_outside_its_grant() {
    let w = world().await;
    let r = call(&w.app, &w.scoped, "GET", "/v1/repos/alice/lobby", None).await;
    assert_eq!(r.status, StatusCode::OK);
    let info: api::RepoInfo = r.json();
    assert_eq!((info.name.as_str(), info.role), ("lobby", None));
    let r = call(&w.app, &w.scoped, "GET", "/v1/repos/alice/vault", None).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(r.code(), "repo_not_found");
}

#[tokio::test]
async fn signed_out_callers_have_no_role_and_service_credentials_are_refused() {
    let w = world().await;
    for (game, lobby) in roles(&w, &Cred::SignedOut, "signed out").await {
        assert_eq!((game, lobby), (None, None));
    }
    let r = call(&w.app, &Cred::SignedOut, "GET", "/v1/me/locks", None).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    for uri in [
        "/v1/repos/alice/lobby",
        "/v1/explore/repos",
        "/v1/users/alice",
        "/v1/me/locks",
        "/v1/me",
    ] {
        let r = call(&w.app, &w.service, "GET", uri, None).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{uri}");
        assert_eq!(r.code(), "service_credential_not_allowed", "{uri}");
    }
}

async fn token_for(app: &Router, who: &Cred, perms: &[&str], repo: &str) -> Cred {
    let body = json!({"name": "ci", "permissions": perms, "repos": [repo]});
    let r = call(app, who, "POST", "/v1/tokens", Some(body)).await;
    assert!(
        r.status.is_success(),
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    Cred::token(&r.json::<api::CreatedToken>().token)
}

#[tokio::test]
async fn a_repo_scoped_token_renames_and_deletes_only_with_manage_roles() {
    let w = world().await;
    let rename = Some(json!({"name": "engine"}));
    let admin = token_for(&w.app, &w.full, &["read", "manage_roles"], "alice/game").await;

    for (method, body) in [("PATCH", rename.clone()), ("DELETE", None)] {
        let r = call(&w.app, &w.scoped, method, "/v1/repos/alice/game", body).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "no manage_roles: {method}");
    }
    let r = call(&w.app, &w.full, "GET", "/v1/repos/alice/game", None).await;
    assert_eq!(r.status, StatusCode::OK, "still there after the refusals");

    let r = call(&w.app, &admin, "PATCH", "/v1/repos/alice/game", rename).await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let r = call(&w.app, &w.full, "GET", "/v1/repos/alice/engine", None).await;
    assert_eq!(r.status, StatusCode::OK);

    let admin = token_for(&w.app, &w.full, &["read", "manage_roles"], "alice/lobby").await;
    let r = call(&w.app, &admin, "DELETE", "/v1/repos/alice/lobby", None).await;
    assert!(
        r.status.is_success(),
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let r = call(&w.app, &w.full, "GET", "/v1/repos/alice/lobby", None).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_repo_scoped_token_never_creates_a_repository() {
    let w = world().await;
    let admin = token_for(&w.app, &w.full, &["read", "manage_roles"], "alice/game").await;
    for (label, cred) in [("manage_roles", &admin), ("plain", &w.scoped)] {
        let body = json!({"owner": "alice", "name": "fresh", "visibility": "private"});
        let r = call(&w.app, cred, "POST", "/v1/repos", Some(body)).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{label}");
    }
}
