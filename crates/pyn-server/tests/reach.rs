//! Credential reach: every route declares what it accepts in `routes.rs`, the OpenAPI document mirrors it, and each
//! credential kind meets exactly the routes it should.

#[path = "common/cred.rs"]
mod cred;
use cred::{Cred, WithCred};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore,
};
use pyn_core::{
    AccessConfig, AccessMode, AccessService, AuthProvider, Credential, Identity, Permission,
    RegistrationMode, Repositories, Rules, ServiceScope, SystemClock, UserId, Visibility,
};
use pyn_proto as api;
use pyn_server::auth::{BearerAuth, DevHeaderAuth};
use pyn_server::{AppState, Reach, declarations, openapi, router};
use serde_json::json;
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";

fn is_lfs(path: &str) -> bool {
    path.contains("/info/lfs/")
}

fn spec_operations() -> BTreeMap<(String, String), Option<String>> {
    let spec = serde_json::to_value(openapi()).unwrap();
    let mut ops = BTreeMap::new();
    for (path, item) in spec["paths"].as_object().unwrap() {
        for (method, op) in item.as_object().unwrap() {
            let reach = op["x-pyn-reach"].as_str().map(str::to_string);
            ops.insert((method.to_uppercase(), path.clone()), reach);
        }
    }
    ops
}

#[test]
fn every_route_in_the_document_and_the_table_declares_its_reach() {
    let table: BTreeMap<(String, String), String> = declarations()
        .into_iter()
        .map(|d| {
            let path = pyn_server::routes::openapi_path(d.path);
            ((d.method.to_string(), path), d.reach.label())
        })
        .collect();
    assert_eq!(
        table.len(),
        declarations().len(),
        "a route is declared twice"
    );
    let spec = spec_operations();

    let mut problems = Vec::new();
    for (op, reach) in &spec {
        match (table.get(op), reach) {
            (None, _) => problems.push(format!("{op:?} is documented but not in the table")),
            (Some(_), None) => problems.push(format!("{op:?} has no x-pyn-reach")),
            (Some(declared), Some(published)) if declared != published => problems.push(format!(
                "{op:?} is {declared} in the table but {published} in the document"
            )),
            _ => {}
        }
    }
    for (method, path) in table.keys() {
        let documented = spec.contains_key(&(method.clone(), path.clone()));
        if is_lfs(path) {
            assert!(!documented, "{method} {path} must stay out of the document");
        } else if !documented && path != "/openapi.json" {
            problems.push(format!("{method} {path} is in the table but undocumented"));
        }
    }
    assert!(problems.is_empty(), "{problems:#?}");
}

/// Identity-reading functions live in `caller.rs` alone, so a handler can only learn the caller through an extractor.
#[test]
fn only_the_caller_module_reads_credentials() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let forbidden = [
        "identify(",
        "identify_admin(",
        "identify_optional(",
        "identify_pending_ok(",
        "identify_allowing_pending(",
        "authenticate_session",
        ".authenticate(",
    ];
    let mut found = Vec::new();
    let mut dirs = vec![src.clone()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
                continue;
            }
            let name = path.file_name().unwrap().to_str().unwrap().to_string();
            if !name.ends_with(".rs") || name == "caller.rs" || name == "auth.rs" {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            for (n, line) in text.lines().enumerate() {
                let code = line.split("//").next().unwrap_or("");
                if forbidden.iter().any(|f| code.contains(f)) {
                    found.push(format!("{}:{}: {}", path.display(), n + 1, line.trim()));
                }
            }
        }
    }
    assert!(
        found.is_empty(),
        "credentials read outside caller.rs: {found:#?}"
    );
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    SignedOut,
    Session,
    Full,
    Admin,
    ReadOnly,
    ScopedAll,
    OtherRepo,
    ServiceOther,
    ServiceAll,
}

impl Kind {
    const ALL: [Kind; 9] = [
        Kind::SignedOut,
        Kind::ServiceOther,
        Kind::ServiceAll,
        Kind::OtherRepo,
        Kind::ReadOnly,
        Kind::ScopedAll,
        Kind::Session,
        Kind::Full,
        Kind::Admin,
    ];

    fn service(self) -> bool {
        matches!(self, Kind::ServiceOther | Kind::ServiceAll)
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Expect {
    Reached,
    Refused,
    Any,
}

/// What a credential kind meets on a route of the given reach. `ServiceOther` holds one scope, never the declared one (the server refuses a credential with none).
fn expected(mode: AccessMode, reach: Reach, kind: Kind) -> Expect {
    use Expect::{Any, Reached, Refused};
    let user = matches!(
        kind,
        Kind::Session
            | Kind::Full
            | Kind::Admin
            | Kind::ReadOnly
            | Kind::ScopedAll
            | Kind::OtherRepo
    );
    let hit = |yes: bool| if yes { Reached } else { Refused };
    match reach {
        Reach::Open => Any,
        Reach::Public if mode == AccessMode::Public => Any,
        Reach::Public => hit(user),
        Reach::OptionalSignIn if mode == AccessMode::Private => hit(user),
        Reach::OptionalSignIn => hit(user || kind == Kind::SignedOut),
        Reach::Session => hit(kind == Kind::Session),
        Reach::AnyCredential => hit(user),
        Reach::FullScope | Reach::FullScopePending | Reach::Namespace => {
            hit(matches!(kind, Kind::Session | Kind::Full | Kind::Admin))
        }
        Reach::AdminOnly => hit(kind == Kind::Admin),
        Reach::Service(_) => hit(matches!(kind, Kind::Admin | Kind::ServiceAll)),
        Reach::FullScopeOrService(_) => hit(matches!(
            kind,
            Kind::Session | Kind::Full | Kind::Admin | Kind::ServiceAll
        )),
        Reach::RepoScoped(permission) => match kind {
            Kind::ReadOnly => hit(permission == Permission::Read),
            Kind::Session | Kind::Full | Kind::Admin | Kind::ScopedAll => Reached,
            _ => Refused,
        },
        Reach::RepoVisible => match kind {
            Kind::ReadOnly => Any,
            _ => hit(matches!(
                kind,
                Kind::Session | Kind::Full | Kind::Admin | Kind::ScopedAll
            )),
        },
        Reach::PublicRepoRead | Reach::PublicRepoVisible if mode == AccessMode::Private => {
            hit(user)
        }
        Reach::PublicRepoRead | Reach::PublicRepoVisible => hit(!kind.service()),
    }
}

struct Reply {
    status: StatusCode,
    body: Vec<u8>,
}

impl Reply {
    fn code(&self) -> Option<String> {
        serde_json::from_slice::<serde_json::Value>(&self.body)
            .ok()
            .and_then(|v| v["code"].as_str().map(str::to_string))
    }

    /// Whether the credential (not the request's content) kept the call from the route.
    fn refused(&self, lfs: bool) -> bool {
        let text = String::from_utf8_lossy(&self.body);
        match self.status {
            StatusCode::UNAUTHORIZED => !text.contains("password"),
            StatusCode::FORBIDDEN => match self.code() {
                Some(code) => matches!(
                    code.as_str(),
                    "forbidden"
                        | "service_credential_not_allowed"
                        | "service_scope_required"
                        | "server_admin_required"
                ),
                None => lfs,
            },
            StatusCode::NOT_FOUND => {
                self.code().as_deref() == Some("repo_not_found") || (lfs && text.contains("repo"))
            }
            _ => false,
        }
    }
}

async fn send(
    app: &Router,
    cred: &Cred,
    method: &Method,
    uri: &str,
    body: Option<String>,
    extra: &[(&str, &str)],
) -> Reply {
    let mut b = Request::builder().method(method.clone()).uri(uri);
    b = b.cred(cred);
    let mut has_type = false;
    for (k, v) in extra {
        has_type |= k.eq_ignore_ascii_case("content-type");
        b = b.header(*k, *v);
    }
    if !has_type {
        b = b.header("content-type", "application/json");
    }
    let r = app
        .clone()
        .oneshot(b.body(Body::from(body.unwrap_or_default())).unwrap())
        .await
        .unwrap();
    let (parts, body) = r.into_parts();
    Reply {
        status: parts.status,
        body: body.collect().await.unwrap().to_bytes().to_vec(),
    }
}

/// Hashes by prefixing, so the matrix does not pay for argon2 on every world.
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

fn state(mode: AccessMode) -> AppState {
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
            access_mode: mode,
            require_email_verification: false,
            ..AccessConfig::default()
        })
        .with_passwords(Arc::new(QuickPasswords))
        .with_registry(meta.clone())
        .with_audit(audit.clone()),
    );
    let repos = Arc::new(Repositories::new(
        meta,
        objects.clone(),
        audit,
        access.clone(),
        clock,
        Rules::empty(),
    ));
    AppState {
        repos,
        objects,
        access: access.clone(),
        auth: Arc::new(BearerAuth { access }),
        dev_auth: Some(Arc::new(DevHeaderAuth) as Arc<dyn AuthProvider>),
        trust_forwarded: false,
    }
}

/// A server with alice (repositories game, open and other; the organization acme), the administrator root, and the
/// credential of `kind`.
async fn world(kind: Kind, reach: Reach) -> (Router, Cred) {
    world_in(AccessMode::Private, kind, reach).await
}

async fn world_in(mode: AccessMode, kind: Kind, reach: Reach) -> (Router, Cred) {
    let st = state(mode);
    let access = st.access.clone();
    let root = UserId::new("root");
    access.admin_for_tests(&root).await.unwrap();
    let alice = UserId::new("alice");
    let as_alice = Identity {
        user: alice.clone(),
        credential: Credential::Session,
    };
    let app = router(st.clone());
    let password_body = json!({"username": "alice", "password": PASSWORD}).to_string();
    let signs_in = matches!(kind, Kind::Session | Kind::Full);
    if signs_in {
        let r = send(
            &app,
            &Cred::SignedOut,
            &Method::POST,
            "/v1/register",
            Some(password_body.clone()),
            &[],
        )
        .await;
        assert_eq!(r.status, StatusCode::CREATED);
    } else {
        access.account_for_tests(&alice).await.unwrap();
    }
    let mut ids = BTreeMap::new();
    for (name, visibility) in [
        ("game", Visibility::Private),
        ("open", Visibility::Public),
        ("other", Visibility::Private),
    ] {
        let record = st
            .repos
            .create(&as_alice, &alice, name, Some(visibility), None)
            .await
            .unwrap();
        ids.insert(name, record.id);
    }
    access.create_org(&as_alice, "acme").await.unwrap();
    let cred = match kind {
        Kind::SignedOut => Cred::SignedOut,
        Kind::Admin => Cred::dev("root"),
        Kind::Full => {
            let r = send(
                &app,
                &Cred::SignedOut,
                &Method::POST,
                "/v1/login",
                Some(password_body),
                &[],
            )
            .await;
            Cred::token(
                &serde_json::from_slice::<api::CreatedToken>(&r.body)
                    .unwrap()
                    .token,
            )
        }
        Kind::Session => raw_session(&app, password_body).await,
        Kind::ReadOnly | Kind::ScopedAll | Kind::OtherRepo => {
            let (permissions, repo): (std::collections::BTreeSet<Permission>, &str) = match kind {
                Kind::ReadOnly => ([Permission::Read].into(), "game"),
                Kind::ScopedAll => (Permission::ALL.into(), "game"),
                _ => (Permission::ALL.into(), "other"),
            };
            let token = access
                .token_for_tests(
                    &UserId::new("alice"),
                    &UserId::new("alice"),
                    permissions,
                    vec![ids[repo].clone()],
                )
                .await
                .unwrap();
            Cred::token(&token)
        }
        Kind::ServiceOther | Kind::ServiceAll => {
            let declared = match reach {
                Reach::Service(s) | Reach::FullScopeOrService(s) => Some(s),
                _ => None,
            };
            let scopes: Vec<&str> = match kind {
                Kind::ServiceAll => ServiceScope::ALL.iter().map(|s| s.as_str()).collect(),
                _ => ServiceScope::ALL
                    .iter()
                    .filter(|s| Some(**s) != declared)
                    .map(|s| s.as_str())
                    .take(1)
                    .collect(),
            };
            let r = send(
                &app,
                &Cred::dev("root"),
                &Method::POST,
                "/v1/admin/service-credentials",
                Some(json!({"name": "svc", "scopes": scopes}).to_string()),
                &[],
            )
            .await;
            assert_eq!(
                r.status,
                StatusCode::CREATED,
                "{:?}",
                String::from_utf8_lossy(&r.body)
            );
            Cred::service(
                &serde_json::from_slice::<api::CreatedServiceCredential>(&r.body)
                    .unwrap()
                    .secret,
            )
        }
    };
    (app, cred)
}

async fn raw_session(app: &Router, body: String) -> Cred {
    let req = Request::builder()
        .method("POST")
        .uri("/v1/session")
        .header("content-type", "application/json")
        .header("origin", "http://localhost:5173")
        .body(Body::from(body))
        .unwrap();
    let r = app.clone().oneshot(req).await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let cookie = r.headers()["set-cookie"].to_str().unwrap().to_string();
    let cookie = cookie.split(';').next().unwrap();
    let cookie = cookie
        .strip_prefix(&format!("{}=", api::SESSION_COOKIE))
        .unwrap()
        .to_string();
    let bytes = r.into_body().collect().await.unwrap().to_bytes();
    let info: api::SessionInfo = serde_json::from_slice(&bytes).unwrap();
    Cred::session(&cookie, &info.csrf_token)
}

fn fill(path: &str, repo: &str) -> String {
    let mut out = String::new();
    let mut rest = path;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let end = rest[start..].find('}').unwrap() + start;
        let name = rest[start + 1..end].trim_start_matches('*');
        out.push_str(match name {
            "owner" => "alice",
            "name" => repo,
            "org" => "acme",
            "oid" => "0000000000000000000000000000000000000000000000000000000000000000",
            "commit" => "deadbeef",
            "branch" => "topic",
            "role" => "writer",
            "address" => "ghost%40example.com",
            "effect" => "allow",
            "kind" => "user",
            _ => "ghost",
        });
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out
}

fn body_for(method: &Method, path: &str) -> Option<String> {
    if matches!(
        *method,
        Method::GET | Method::HEAD | Method::OPTIONS | Method::DELETE
    ) {
        return None;
    }
    Some(
        if path.ends_with("/info/lfs/locks") {
            r#"{"path":"Content/x.bin"}"#
        } else if path.ends_with("/info/lfs/objects/batch") {
            r#"{"operation":"download","objects":[]}"#
        } else if path.ends_with("/info/lfs/objects/verify") {
            r#"{"oid":"0000000000000000000000000000000000000000000000000000000000000000","size":1}"#
        } else if (is_lfs(path) && !path.ends_with("/verify") && !path.ends_with("/unlock"))
            || path.ends_with("/objects")
            || path.ends_with("/objects/chunk")
            || path.contains("avatar")
        {
            "x"
        } else {
            "{}"
        }
        .to_string(),
    )
}

fn headers_for(path: &str) -> Vec<(&'static str, &'static str)> {
    if path.ends_with("/tus") {
        vec![
            ("tus-resumable", "1.0.0"),
            ("upload-length", "1"),
            ("upload-offset", "0"),
            ("content-type", "application/offset+octet-stream"),
        ]
    } else if is_lfs(path) {
        vec![("content-type", "application/vnd.git-lfs+json")]
    } else if path.contains("avatar")
        || path.ends_with("/objects")
        || path.ends_with("/objects/chunk")
    {
        vec![("content-type", "application/octet-stream")]
    } else {
        vec![]
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn each_credential_kind_meets_the_routes_it_should_on_a_private_server() {
    matrix(AccessMode::Private).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn each_credential_kind_meets_the_routes_it_should_on_a_public_server() {
    matrix(AccessMode::Public).await;
}

async fn matrix(mode: AccessMode) {
    let mut failures = Vec::new();
    let mut calls = 0;
    for d in declarations() {
        let public_repo = matches!(d.reach, Reach::PublicRepoRead | Reach::PublicRepoVisible);
        let mut uri = fill(d.path, if public_repo { "open" } else { "game" });
        if d.path.ends_with("/verify") && d.method == Method::GET {
            uri.push_str("?path=a");
        }
        for kind in Kind::ALL {
            let (app, cred) = world_in(mode, kind, d.reach).await;
            let reply = send(
                &app,
                &cred,
                &d.method,
                &uri,
                body_for(&d.method, d.path),
                &headers_for(d.path),
            )
            .await;
            calls += 1;
            let label = format!("{} {} as {kind:?} ({})", d.method, d.path, d.reach.label());
            if reply.status.is_server_error() {
                failures.push(format!(
                    "{label}: {} {}",
                    reply.status,
                    String::from_utf8_lossy(&reply.body)
                ));
                continue;
            }
            if d.method == Method::HEAD && reply.status == StatusCode::NOT_FOUND {
                continue;
            }
            let refused = reply.refused(is_lfs(d.path));
            let repo_route = matches!(
                d.reach,
                Reach::RepoScoped(_)
                    | Reach::RepoVisible
                    | Reach::PublicRepoRead
                    | Reach::PublicRepoVisible
            );
            if kind == Kind::OtherRepo
                && repo_route
                && !public_repo
                && d.method != Method::HEAD
                && reply.status != StatusCode::NOT_FOUND
            {
                failures.push(format!(
                    "{label}: a token for another repository must get 404, got {} {}",
                    reply.status,
                    String::from_utf8_lossy(&reply.body)
                ));
            }
            let expect = expected(mode, d.reach, kind);
            if mode == AccessMode::Private && kind == Kind::SignedOut && expect == Expect::Refused {
                let answer = if public_repo {
                    ("repo_not_found", StatusCode::NOT_FOUND)
                } else {
                    ("unauthenticated", StatusCode::UNAUTHORIZED)
                };
                let closed = matches!(
                    d.reach,
                    Reach::Public
                        | Reach::OptionalSignIn
                        | Reach::PublicRepoRead
                        | Reach::PublicRepoVisible
                );
                if closed
                    && d.method != Method::HEAD
                    && (reply.code().as_deref(), reply.status) != (Some(answer.0), answer.1)
                {
                    failures.push(format!(
                        "{label}: a signed-out caller must get {} {}, got {} {}",
                        answer.1,
                        answer.0,
                        reply.status,
                        String::from_utf8_lossy(&reply.body)
                    ));
                }
            }
            match expect {
                Expect::Reached if refused => failures.push(format!(
                    "{label}: expected to reach the route, got {} {}",
                    reply.status,
                    String::from_utf8_lossy(&reply.body)
                )),
                Expect::Refused if !refused => failures.push(format!(
                    "{label}: expected a refusal, got {} {}",
                    reply.status,
                    String::from_utf8_lossy(&reply.body)
                )),
                _ => {}
            }
        }
    }
    assert!(calls >= 1000, "the matrix ran {calls} calls");
    assert!(
        failures.is_empty(),
        "{} mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// Every reach kind appears in the table, so the matrix exercises each rule.
#[test]
fn the_table_uses_every_reach_kind() {
    let used: BTreeSet<String> = declarations()
        .iter()
        .map(|d| d.reach.label().split(':').next().unwrap().to_string())
        .collect();
    for kind in [
        "open",
        "public",
        "optional_sign_in",
        "session",
        "any_credential",
        "full_scope",
        "full_scope_pending",
        "namespace",
        "admin_only",
        "service",
        "full_scope_or_service",
        "repo_scoped",
        "repo_visible",
        "public_repo_read",
        "public_repo_visible",
    ] {
        assert!(used.contains(kind), "no route declares {kind}");
    }
}

/// A token made for another repository gets the answer a missing repository gets, and a public one only read.
#[tokio::test]
async fn a_token_for_another_repository_is_told_it_does_not_exist() {
    let (app, cred) = world(Kind::OtherRepo, Reach::RepoVisible).await;
    for method in [Method::GET, Method::PATCH, Method::DELETE] {
        let body = (method == Method::PATCH).then(|| "{}".to_string());
        let uncovered = send(
            &app,
            &cred,
            &method,
            "/v1/repos/alice/game",
            body.clone(),
            &[],
        )
        .await;
        let missing = send(&app, &cred, &method, "/v1/repos/alice/ghost", body, &[]).await;
        assert_eq!(uncovered.status, StatusCode::NOT_FOUND, "{method}");
        assert_eq!(uncovered.status, missing.status, "{method}");
        assert_eq!(
            String::from_utf8_lossy(&uncovered.body).replace("game", "ghost"),
            String::from_utf8_lossy(&missing.body),
            "{method}"
        );
        assert_eq!(uncovered.code().as_deref(), Some("repo_not_found"));
    }
    for method in [Method::PATCH, Method::DELETE] {
        let r = send(
            &app,
            &cred,
            &method,
            "/v1/repos/alice/open",
            Some("{}".into()),
            &[],
        )
        .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{method}");
    }
    let r = send(&app, &cred, &Method::GET, "/v1/repos/alice/open", None, &[]).await;
    assert_eq!(r.status, StatusCode::OK);
}
