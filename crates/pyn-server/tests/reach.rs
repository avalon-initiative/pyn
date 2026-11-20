//! Credential reach: every route declares what it accepts in `routes.rs`, the OpenAPI document mirrors it, and each
//! credential kind meets exactly the routes it should.

#[path = "common/cred.rs"]
mod cred;
use cred::{Cred, WithCred, grants_body};
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
    AccessConfig, AccessMode, AccessService, AuthProvider, Credential, GrantLevel, GrantNeed,
    Identity, Permission, RegistrationMode, Repositories, Rules, ServiceScope, SystemClock,
    TokenResource, UserId, Visibility,
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
        Reach::OptionalFullScope if mode == AccessMode::Private => {
            hit(matches!(kind, Kind::Session | Kind::Full | Kind::Admin))
        }
        Reach::OptionalFullScope => hit(matches!(
            kind,
            Kind::SignedOut | Kind::Session | Kind::Full | Kind::Admin
        )),
        Reach::Session => hit(kind == Kind::Session),
        Reach::AnyCredential => hit(user),
        Reach::FullScope
        | Reach::FullScopePending
        | Reach::FullScopeEnrolment
        | Reach::Namespace
        | Reach::OrgScoped(_)
        | Reach::AccountScoped(_) => hit(matches!(kind, Kind::Session | Kind::Full | Kind::Admin)),
        Reach::AdminOnly => hit(kind == Kind::Admin),
        Reach::Service(_) => hit(matches!(kind, Kind::Admin | Kind::ServiceAll)),
        Reach::FullScopeOrService(_) => hit(matches!(
            kind,
            Kind::Session | Kind::Full | Kind::Admin | Kind::ServiceAll
        )),
        Reach::RepoScoped(need) => match kind {
            Kind::ReadOnly => hit(read_only_holds(need)),
            Kind::Session | Kind::Full | Kind::Admin | Kind::ScopedAll => Reached,
            _ => Refused,
        },
        Reach::PublicRepoRead(_) | Reach::PublicLfsRead(_) if mode == AccessMode::Private => {
            hit(user)
        }
        Reach::PublicRepoRead(_) | Reach::PublicLfsRead(_) => hit(!kind.service()),
    }
}

/// The `ReadOnly` token holds metadata, contents and locks at read.
fn read_only_holds(need: GrantNeed) -> bool {
    need.level == GrantLevel::Read
        && matches!(
            need.resource,
            TokenResource::Metadata | TokenResource::Contents | TokenResource::Locks
        )
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
        updates: None,
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

/// The resource and level a repository route declares.
fn repo_need(reach: Reach) -> Option<GrantNeed> {
    match reach {
        Reach::RepoScoped(n) | Reach::PublicRepoRead(n) | Reach::PublicLfsRead(n) => Some(n),
        _ => None,
    }
}

/// A body the handler parses, so the answer comes from the permission check and not from the request's shape.
fn valid_body(path: &str) -> Option<&'static str> {
    Some(match path.rsplit('/').next()? {
        "checkout" | "release" => r#"{"path":"Content/x.bin"}"#,
        "checkin" => r#"{"path":"Content/x.bin","content":"","message":"m"}"#,
        "changesets" => r#"{"entries":[],"deletions":[],"message":"m"}"#,
        "restore" => r#"{"path":"Content/x.bin","revision":1,"base_revision":1,"confirm":"x"}"#,
        "force-unlock" => r#"{"path":"Content/x.bin","reason":"r"}"#,
        "preview" if path.contains("/policy/") => r#"{"content":""}"#,
        "preview" | "merge" => r#"{"source":"topic"}"#,
        "missing" => r#"{"hashes":[]}"#,
        "assemble" => {
            r#"{"chunks":["0000000000000000000000000000000000000000000000000000000000000000"]}"#
        }
        "branches" => r#"{"name":"topic"}"#,
        "push" => r#"{"commits":[],"trees":[]}"#,
        _ if path.contains("/branches/") => r#"{"name":"topic2"}"#,
        _ => return None,
    })
}

fn body_for(method: &Method, path: &str) -> Option<String> {
    if matches!(
        *method,
        Method::GET | Method::HEAD | Method::OPTIONS | Method::DELETE
    ) {
        return None;
    }
    Some(
        if let Some(valid) = valid_body(path) {
            valid
        } else if path.ends_with("/users") {
            r#"{"username":"bob","password":"correct horse battery","role":"reader"}"#
        } else if path.ends_with("/invites") {
            r#"{"role":"reader","hours":1}"#
        } else if path.contains("/roles/") {
            r#"{"permissions":["read"]}"#
        } else if path.contains("/members/") || path.contains("/teams/") {
            r#"{"role":"reader"}"#
        } else if path.ends_with("/info/lfs/locks") {
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
        let public_repo = matches!(d.reach, Reach::PublicRepoRead(_) | Reach::PublicLfsRead(_));
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
            let repo_route = repo_need(d.reach).is_some();
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
            let mut expect = expected(mode, d.reach, kind);
            if matches!(kind, Kind::ReadOnly | Kind::ScopedAll)
                && full_scope_in_core(&d.method, d.path)
            {
                expect = Expect::Refused;
            }
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
                        | Reach::OptionalFullScope
                        | Reach::PublicRepoRead(_)
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
        "optional_full_scope",
        "session",
        "any_credential",
        "full_scope",
        "full_scope_pending",
        "full_scope_enrolment",
        "namespace",
        "org_scoped",
        "account_scoped",
        "admin_only",
        "service",
        "full_scope_or_service",
        "repo_scoped",
        "public_repo_read",
        "public_lfs_read",
    ] {
        assert!(used.contains(kind), "no route declares {kind}");
    }
}

/// A token made for another repository gets the answer a missing repository gets, and a public one only read.
#[tokio::test]
async fn a_token_for_another_repository_is_told_it_does_not_exist() {
    let (app, cred) = world(Kind::OtherRepo, Reach::Open).await;
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

/// A server with alice (repositories game, open and other) and a fine-grained token of hers for `repo` holding exactly
/// `grants` (metadata read is added by the server), created through the token API with her sign-in token.
async fn fine_world(grants: &[(TokenResource, GrantLevel)], repo: &str) -> (Router, Cred) {
    let st = state(AccessMode::Private);
    let alice = UserId::new("alice");
    let as_alice = Identity {
        user: alice.clone(),
        credential: Credential::Session,
    };
    let app = router(st.clone());
    let password_body = json!({"username": "alice", "password": PASSWORD}).to_string();
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
    for (name, visibility) in [
        ("game", Visibility::Private),
        ("open", Visibility::Public),
        ("other", Visibility::Private),
    ] {
        st.repos
            .create(&as_alice, &alice, name, Some(visibility), None)
            .await
            .unwrap();
    }
    let r = send(
        &app,
        &Cred::SignedOut,
        &Method::POST,
        "/v1/login",
        Some(password_body),
        &[],
    )
    .await;
    let full = Cred::token(
        &serde_json::from_slice::<api::CreatedToken>(&r.body)
            .unwrap()
            .token,
    );
    let levels: Vec<_> = grants
        .iter()
        .map(|(r, l)| (r.as_str(), l.as_str()))
        .collect();
    let r = send(
        &app,
        &full,
        &Method::POST,
        "/v1/tokens",
        Some(grants_body("fine", "alice", &[repo], &levels).to_string()),
        &[],
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let token = serde_json::from_slice::<api::CreatedToken>(&r.body)
        .unwrap()
        .token;
    (app, Cred::token(&token))
}

/// The grants of a token holding exactly the declared need. Metadata is always held, so a metadata need adds audit
/// read, the smallest other grant, to make a token the server accepts.
fn exact_grants(need: GrantNeed) -> Vec<(TokenResource, GrantLevel)> {
    match need.resource {
        TokenResource::Metadata => vec![(TokenResource::Audit, GrantLevel::Read)],
        resource => vec![(resource, need.level)],
    }
}

/// Every other resource at its top level, and the declared one one level short (absent for a read).
fn lacking_grants(need: GrantNeed) -> Vec<(TokenResource, GrantLevel)> {
    let mut grants: Vec<_> = TokenResource::ALL
        .into_iter()
        .filter(|r| *r != need.resource && *r != TokenResource::Metadata)
        .map(|r| (r, r.max_level()))
        .collect();
    match need.level {
        GrantLevel::Read => {}
        GrantLevel::Write if need.resource == TokenResource::Policy => {}
        GrantLevel::Write => grants.push((need.resource, GrantLevel::Read)),
        GrantLevel::Admin => grants.push((need.resource, GrantLevel::Write)),
    }
    grants
}

/// Routes whose core also demands a full-scope credential, which no fine-grained token is.
fn full_scope_in_core(method: &Method, path: &str) -> bool {
    *method == Method::POST && (path.ends_with("/users") || path.ends_with("/invites"))
}

/// For every repository route a token holding exactly the declared grant reaches it, one holding every other grant but
/// not that one is refused, and one made for another repository gets the answer a missing repository gets.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fine_grained_token_reaches_exactly_what_its_grants_declare() {
    let mut failures = Vec::new();
    let mut calls = 0;
    for d in declarations() {
        let Some(need) = repo_need(d.reach) else {
            continue;
        };
        let mut uri = fill(d.path, "game");
        if d.path.ends_with("/verify") && d.method == Method::GET {
            uri.push_str("?path=a");
        }
        let lfs = is_lfs(d.path);
        let body = body_for(&d.method, d.path);
        let headers = headers_for(d.path);
        let label = |who: &str| format!("{} {} as {who} ({})", d.method, d.path, d.reach.label());
        let describe = |r: &Reply| format!("{} {}", r.status, String::from_utf8_lossy(&r.body));

        let (app, cred) = fine_world(&exact_grants(need), "game").await;
        let r = send(&app, &cred, &d.method, &uri, body.clone(), &headers).await;
        calls += 1;
        let should_reach = !full_scope_in_core(&d.method, d.path);
        if r.status.is_server_error() || r.refused(lfs) == should_reach {
            failures.push(format!(
                "{}: expected {}, got {}",
                label("the exact grant"),
                if should_reach {
                    "to reach it"
                } else {
                    "a refusal"
                },
                describe(&r)
            ));
        }

        if need.resource != TokenResource::Metadata {
            let (app, cred) = fine_world(&lacking_grants(need), "game").await;
            let r = send(&app, &cred, &d.method, &uri, body.clone(), &headers).await;
            calls += 1;
            if r.status != StatusCode::FORBIDDEN || !r.refused(lfs) {
                failures.push(format!(
                    "{}: expected a 403, got {}",
                    label("every other grant"),
                    describe(&r)
                ));
            }
        }

        let (app, cred) = fine_world(&exact_grants(need), "other").await;
        let r = send(&app, &cred, &d.method, &uri, body, &headers).await;
        calls += 1;
        if d.method != Method::HEAD && (r.status != StatusCode::NOT_FOUND || !r.refused(lfs)) {
            failures.push(format!(
                "{}: expected the missing-repository answer, got {}",
                label("a token for another repository"),
                describe(&r)
            ));
        }
    }
    assert!(calls >= 150, "the matrix ran {calls} calls");
    assert!(
        failures.is_empty(),
        "{} mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// The catalog (decision #1099) applied to every repository route, written out apart from the table so a changed
/// declaration is a visible change here. Where core demands more than the catalog's read level, the route is declared
/// at the level core needs (listing members, invitations and teams, and the usage breakdown).
const REPOSITORY_ROUTES: &[(&str, &str, &str)] = &[
    ("GET", "/usage", "public_repo_read:contents:read"),
    (
        "GET",
        "/usage/breakdown",
        "repo_scoped:administration:write",
    ),
    ("GET", "", "public_repo_read:metadata:read"),
    ("PATCH", "", "repo_scoped:administration:write"),
    ("DELETE", "", "repo_scoped:administration:write"),
    ("GET", "/me", "repo_scoped:metadata:read"),
    ("POST", "/users", "repo_scoped:members:write"),
    ("GET", "/invites", "repo_scoped:members:write"),
    ("POST", "/invites", "repo_scoped:members:write"),
    ("DELETE", "/invites/{id}", "repo_scoped:members:write"),
    ("GET", "/roles", "repo_scoped:members:read"),
    ("PUT", "/roles/{role}", "repo_scoped:administration:write"),
    ("GET", "/members", "repo_scoped:members:write"),
    ("PUT", "/members/{user}", "repo_scoped:members:write"),
    ("GET", "/teams", "repo_scoped:members:write"),
    ("PUT", "/teams/{team}", "repo_scoped:members:write"),
    ("DELETE", "/teams/{team}", "repo_scoped:members:write"),
    ("GET", "/locks", "public_repo_read:locks:read"),
    ("GET", "/locks/events", "public_repo_read:locks:read"),
    ("GET", "/info/lfs/locks", "repo_scoped:locks:read"),
    ("POST", "/info/lfs/locks", "repo_scoped:locks:write"),
    ("POST", "/info/lfs/locks/verify", "repo_scoped:locks:read"),
    (
        "POST",
        "/info/lfs/locks/{id}/unlock",
        "repo_scoped:locks:write",
    ),
    (
        "POST",
        "/info/lfs/objects/batch",
        "public_lfs_read:contents:read",
    ),
    (
        "POST",
        "/info/lfs/objects/verify",
        "repo_scoped:contents:write",
    ),
    (
        "GET",
        "/info/lfs/objects/{oid}",
        "public_lfs_read:contents:read",
    ),
    (
        "PUT",
        "/info/lfs/objects/{oid}",
        "repo_scoped:contents:write",
    ),
    (
        "HEAD",
        "/info/lfs/objects/{oid}/tus",
        "repo_scoped:contents:write",
    ),
    (
        "POST",
        "/info/lfs/objects/{oid}/tus",
        "repo_scoped:contents:write",
    ),
    (
        "PATCH",
        "/info/lfs/objects/{oid}/tus",
        "repo_scoped:contents:write",
    ),
    (
        "DELETE",
        "/info/lfs/objects/{oid}/tus",
        "repo_scoped:contents:write",
    ),
    ("GET", "/files", "public_repo_read:contents:read"),
    ("GET", "/tree", "public_repo_read:contents:read"),
    ("GET", "/summary", "public_repo_read:contents:read"),
    ("GET", "/content", "public_repo_read:contents:read"),
    ("POST", "/checkout", "repo_scoped:locks:write"),
    ("POST", "/release", "repo_scoped:locks:write"),
    ("POST", "/checkin", "repo_scoped:contents:write"),
    ("POST", "/changesets", "repo_scoped:contents:write"),
    ("POST", "/restore", "repo_scoped:contents:write"),
    ("POST", "/force-unlock", "repo_scoped:locks:admin"),
    ("POST", "/policy/preview", "repo_scoped:policy:write"),
    ("GET", "/audit", "repo_scoped:audit:read"),
    ("PUT", "/objects", "repo_scoped:contents:write"),
    ("PUT", "/objects/chunk", "repo_scoped:contents:write"),
    ("POST", "/objects/missing", "repo_scoped:contents:write"),
    ("POST", "/objects/assemble", "repo_scoped:contents:write"),
    ("GET", "/history", "public_repo_read:contents:read"),
    ("GET", "/verify", "public_repo_read:contents:read"),
    ("GET", "/commits", "public_repo_read:contents:read"),
    ("GET", "/commits/{commit}", "public_repo_read:contents:read"),
    ("GET", "/snapshot", "public_repo_read:contents:read"),
    ("GET", "/branches", "public_repo_read:contents:read"),
    ("POST", "/branches", "repo_scoped:contents:write"),
    ("POST", "/push", "repo_scoped:contents:write"),
    ("POST", "/fetch", "public_repo_read:contents:read"),
    ("GET", "/objects/{id}", "public_repo_read:contents:read"),
    ("POST", "/merge", "repo_scoped:contents:write"),
    ("POST", "/merge/preview", "repo_scoped:contents:read"),
    (
        "GET",
        "/branches/{*branch}",
        "public_repo_read:contents:read",
    ),
    (
        "DELETE",
        "/branches/{*branch}",
        "repo_scoped:contents:write",
    ),
    ("PATCH", "/branches/{*branch}", "repo_scoped:contents:write"),
];

#[test]
fn every_repository_route_declares_the_resource_and_level_of_the_catalog() {
    const BASE: &str = "/v1/repos/{owner}/{name}";
    let declared: BTreeMap<(String, String), String> = declarations()
        .into_iter()
        .filter(|d| repo_need(d.reach).is_some())
        .map(|d| {
            let suffix = d.path.strip_prefix(BASE).unwrap().to_string();
            ((d.method.to_string(), suffix), d.reach.label())
        })
        .collect();
    let pinned: BTreeMap<(String, String), String> = REPOSITORY_ROUTES
        .iter()
        .map(|(m, p, l)| ((m.to_string(), p.to_string()), l.to_string()))
        .collect();
    assert_eq!(
        pinned.len(),
        REPOSITORY_ROUTES.len(),
        "a route is pinned twice"
    );
    assert_eq!(declared, pinned);
}

/// The organization and account routes with the resource and level each declares, written out apart from the table.
/// Reads of an organization's or account's public view stay open to any credential and are not listed.
const ORGANIZATION_AND_ACCOUNT_ROUTES: &[(&str, &str, &str)] = &[
    ("GET", "/v1/keys", "account_scoped:account_ssh_keys:read"),
    ("POST", "/v1/keys", "account_scoped:account_ssh_keys:write"),
    (
        "DELETE",
        "/v1/keys/{id}",
        "account_scoped:account_ssh_keys:write",
    ),
    (
        "PUT",
        "/v1/me/profile",
        "account_scoped:account_profile:write",
    ),
    (
        "PUT",
        "/v1/me/avatar",
        "account_scoped:account_profile:write",
    ),
    (
        "DELETE",
        "/v1/me/avatar",
        "account_scoped:account_profile:write",
    ),
    (
        "PUT",
        "/v1/orgs/{org}/profile",
        "org_scoped:org_administration:write",
    ),
    (
        "PUT",
        "/v1/orgs/{org}/avatar",
        "org_scoped:org_administration:write",
    ),
    (
        "DELETE",
        "/v1/orgs/{org}/avatar",
        "org_scoped:org_administration:write",
    ),
    (
        "DELETE",
        "/v1/orgs/{org}",
        "org_scoped:org_administration:write",
    ),
    ("GET", "/v1/orgs/{org}/audit", "org_scoped:org_audit:read"),
    (
        "GET",
        "/v1/orgs/{org}/members",
        "org_scoped:org_members:read",
    ),
    (
        "POST",
        "/v1/orgs/{org}/members",
        "org_scoped:org_members:write",
    ),
    (
        "GET",
        "/v1/orgs/{org}/invitations",
        "org_scoped:org_members:write",
    ),
    (
        "DELETE",
        "/v1/orgs/{org}/invitations/{user}",
        "org_scoped:org_members:write",
    ),
    (
        "PATCH",
        "/v1/orgs/{org}/members/{user}",
        "org_scoped:org_members:write",
    ),
    (
        "DELETE",
        "/v1/orgs/{org}/members/{user}",
        "org_scoped:org_members:write",
    ),
    (
        "GET",
        "/v1/orgs/{org}/settings",
        "org_scoped:org_administration:read",
    ),
    (
        "PUT",
        "/v1/orgs/{org}/settings",
        "org_scoped:org_administration:write",
    ),
    (
        "GET",
        "/v1/orgs/{org}/repo-policy",
        "org_scoped:org_administration:read",
    ),
    (
        "PUT",
        "/v1/orgs/{org}/repo-policy",
        "org_scoped:org_administration:write",
    ),
    (
        "PUT",
        "/v1/orgs/{org}/repo-policy/rules/{effect}/{kind}/{subject}",
        "org_scoped:org_administration:write",
    ),
    (
        "DELETE",
        "/v1/orgs/{org}/repo-policy/rules/{effect}/{kind}/{subject}",
        "org_scoped:org_administration:write",
    ),
    (
        "GET",
        "/v1/orgs/{org}/two-factor",
        "org_scoped:org_administration:read",
    ),
    (
        "PUT",
        "/v1/orgs/{org}/two-factor",
        "org_scoped:org_administration:write",
    ),
    ("GET", "/v1/orgs/{org}/teams", "org_scoped:org_teams:read"),
    ("POST", "/v1/orgs/{org}/teams", "org_scoped:org_teams:write"),
    (
        "GET",
        "/v1/orgs/{org}/teams/{team}",
        "org_scoped:org_teams:read",
    ),
    (
        "PATCH",
        "/v1/orgs/{org}/teams/{team}",
        "org_scoped:org_teams:write",
    ),
    (
        "DELETE",
        "/v1/orgs/{org}/teams/{team}",
        "org_scoped:org_teams:write",
    ),
    (
        "PUT",
        "/v1/orgs/{org}/teams/{team}/members/{user}",
        "org_scoped:org_teams:write",
    ),
    (
        "DELETE",
        "/v1/orgs/{org}/teams/{team}/members/{user}",
        "org_scoped:org_teams:write",
    ),
];

fn owner_need(reach: Reach) -> Option<GrantNeed> {
    match reach {
        Reach::OrgScoped(n) | Reach::AccountScoped(n) => Some(n),
        _ => None,
    }
}

#[test]
fn every_organization_and_account_route_declares_the_resource_and_level_of_the_catalog() {
    let declared: BTreeMap<(String, String), String> = declarations()
        .into_iter()
        .filter(|d| owner_need(d.reach).is_some())
        .map(|d| ((d.method.to_string(), d.path.to_string()), d.reach.label()))
        .collect();
    let pinned: BTreeMap<(String, String), String> = ORGANIZATION_AND_ACCOUNT_ROUTES
        .iter()
        .map(|(m, p, l)| ((m.to_string(), p.to_string()), l.to_string()))
        .collect();
    assert_eq!(pinned.len(), ORGANIZATION_AND_ACCOUNT_ROUTES.len());
    assert_eq!(declared, pinned);
    for (_, need) in declared
        .keys()
        .zip(declarations().iter().filter_map(|d| owner_need(d.reach)))
    {
        assert_ne!(need.resource.scope(), pyn_core::GrantScope::Repository);
    }
}

/// A server with alice (repositories game, open and other; the organizations acme and beta, both hers) and a fine-grained
/// token of hers for `owner` with repository access `public` (`all` when a grant is a repository one) holding exactly `grants`, made through the token API.
async fn owner_world(owner: &str, grants: &[(TokenResource, GrantLevel)]) -> (Router, Cred) {
    let st = state(AccessMode::Private);
    let alice = UserId::new("alice");
    let as_alice = Identity {
        user: alice.clone(),
        credential: Credential::Session,
    };
    let app = router(st.clone());
    let password_body = json!({"username": "alice", "password": PASSWORD}).to_string();
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
    for name in ["game", "open", "other"] {
        st.repos
            .create(&as_alice, &alice, name, Some(Visibility::Private), None)
            .await
            .unwrap();
    }
    for org in ["acme", "beta"] {
        st.access.create_org(&as_alice, org).await.unwrap();
    }
    let r = send(
        &app,
        &Cred::SignedOut,
        &Method::POST,
        "/v1/login",
        Some(password_body),
        &[],
    )
    .await;
    let full = Cred::token(
        &serde_json::from_slice::<api::CreatedToken>(&r.body)
            .unwrap()
            .token,
    );
    let permissions: Vec<_> = grants
        .iter()
        .map(|(r, l)| json!({"resource": r.as_str(), "level": l.as_str()}))
        .collect();
    let repository = grants
        .iter()
        .any(|(r, _)| r.scope() == pyn_core::GrantScope::Repository);
    let mode = if repository { "all" } else { "public" };
    let body = json!({
        "name": "owned",
        "owner": owner,
        "repository_access": {"mode": mode, "repos": []},
        "permissions": permissions,
    });
    let r = send(
        &app,
        &full,
        &Method::POST,
        "/v1/tokens",
        Some(body.to_string()),
        &[],
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "{owner} {grants:?}: {}",
        String::from_utf8_lossy(&r.body)
    );
    let token = serde_json::from_slice::<api::CreatedToken>(&r.body)
        .unwrap()
        .token;
    (app, Cred::token(&token))
}

/// Every other resource of the category at its top level, and the declared one one level short (absent for a read).
fn lacking_owner_grants(need: GrantNeed) -> Vec<(TokenResource, GrantLevel)> {
    let category = match need.resource.scope() {
        pyn_core::GrantScope::Organization => TokenResource::ORGANIZATION.to_vec(),
        _ => TokenResource::ACCOUNT.to_vec(),
    };
    let mut grants: Vec<_> = category
        .into_iter()
        .filter(|r| *r != need.resource)
        .map(|r| (r, r.max_level()))
        .collect();
    if need.level == GrantLevel::Write && need.resource.min_level() == GrantLevel::Read {
        grants.push((need.resource, GrantLevel::Read));
    }
    grants
}

/// For every organization and account route a token holding exactly the declared grant reaches it, one holding every
/// other grant of its category does not, nor one owned by another organization or account, nor a repository token.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_organization_or_account_token_reaches_exactly_what_its_grants_declare() {
    let mut failures = Vec::new();
    let mut calls = 0;
    for d in declarations() {
        let Some(need) = owner_need(d.reach) else {
            continue;
        };
        let org_route = need.resource.scope() == pyn_core::GrantScope::Organization;
        let owner = if org_route { "acme" } else { "alice" };
        let uri = fill(d.path, "game");
        let body = body_for(&d.method, d.path);
        let headers = headers_for(d.path);
        let label = |who: &str| format!("{} {} as {who} ({})", d.method, d.path, d.reach.label());
        let describe = |r: &Reply| format!("{} {}", r.status, String::from_utf8_lossy(&r.body));

        let (app, cred) = owner_world(owner, &[(need.resource, need.level)]).await;
        let r = send(&app, &cred, &d.method, &uri, body.clone(), &headers).await;
        calls += 1;
        if r.status.is_server_error() || r.refused(false) {
            failures.push(format!(
                "{}: expected to reach it, got {}",
                label("the exact grant"),
                describe(&r)
            ));
        }

        let (app, cred) = owner_world(owner, &lacking_owner_grants(need)).await;
        let r = send(&app, &cred, &d.method, &uri, body.clone(), &headers).await;
        calls += 1;
        if r.status != StatusCode::FORBIDDEN || r.code().as_deref() != Some("forbidden") {
            failures.push(format!(
                "{}: expected a 403 forbidden, got {}",
                label("every other grant of its category"),
                describe(&r)
            ));
        }

        let (other_owner, other_grants) = if org_route {
            ("beta", vec![(need.resource, need.level)])
        } else {
            (
                "acme",
                TokenResource::ORGANIZATION
                    .map(|r| (r, r.max_level()))
                    .to_vec(),
            )
        };
        let (app, cred) = owner_world(other_owner, &other_grants).await;
        let r = send(&app, &cred, &d.method, &uri, body.clone(), &headers).await;
        calls += 1;
        if r.status != StatusCode::FORBIDDEN || r.code().as_deref() != Some("forbidden") {
            failures.push(format!(
                "{}: expected a 403 forbidden, got {}",
                label("a token owned by another organization or account"),
                describe(&r)
            ));
        }

        if org_route {
            let personal = [
                (TokenResource::Contents, GrantLevel::Read),
                (TokenResource::Administration, GrantLevel::Write),
            ];
            let (app, cred) = owner_world("alice", &personal).await;
            let r = send(&app, &cred, &d.method, &uri, body.clone(), &headers).await;
            calls += 1;
            if r.status != StatusCode::FORBIDDEN || r.code().as_deref() != Some("forbidden") {
                failures.push(format!(
                    "{}: expected a 403 forbidden, got {}",
                    label("a personal token with administration write"),
                    describe(&r)
                ));
            }
        }

        let (app, cred) = fine_world(
            &TokenResource::ALL
                .into_iter()
                .filter(|r| *r != TokenResource::Metadata)
                .map(|r| (r, r.max_level()))
                .collect::<Vec<_>>(),
            "game",
        )
        .await;
        let r = send(&app, &cred, &d.method, &uri, body, &headers).await;
        calls += 1;
        if r.status != StatusCode::FORBIDDEN || r.code().as_deref() != Some("forbidden") {
            failures.push(format!(
                "{}: expected a 403 forbidden, got {}",
                label("a repository-only token"),
                describe(&r)
            ));
        }
    }
    assert!(calls >= 100, "the matrix ran {calls} calls");
    assert!(
        failures.is_empty(),
        "{} mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

async fn json_of(app: &Router, cred: &Cred, uri: &str) -> (StatusCode, serde_json::Value) {
    let r = send(app, cred, &Method::GET, uri, None, &[]).await;
    let v = serde_json::from_slice(&r.body).unwrap_or(json!(null));
    (r.status, v)
}

fn names(v: &serde_json::Value, key: &str) -> BTreeSet<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|e| e[key].as_str().unwrap().to_string())
        .collect()
}

/// A repository-scoped token's answers on the routes any credential reaches stay inside its repositories.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_scoped_token_sees_only_its_repositories_on_the_shared_routes() {
    for kind in [Kind::ScopedAll, Kind::ReadOnly] {
        let (app, cred) = world(kind, Reach::AnyCredential).await;

        let (status, repos) = json_of(&app, &cred, "/v1/repos").await;
        assert_eq!(status, StatusCode::OK, "{kind:?}");
        let listed = names(&repos, "name");
        assert!(listed.contains("game"), "{kind:?}: {listed:?}");
        assert!(!listed.contains("other"), "{kind:?}: {listed:?}");

        let (status, profile) = json_of(&app, &cred, "/v1/users/alice").await;
        assert_eq!(status, StatusCode::OK, "{kind:?}");
        let listed = names(&profile["repositories"], "name");
        assert!(!listed.contains("other"), "{kind:?}: {listed:?}");
        assert!(
            profile["organizations"].as_array().unwrap().is_empty(),
            "{kind:?}: {profile}"
        );

        let (status, explore) = json_of(&app, &cred, "/v1/explore/repos").await;
        assert_eq!(status, StatusCode::OK, "{kind:?}");
        let listed = names(&explore["repos"], "name");
        assert!(
            !listed.contains("other") && !listed.contains("game"),
            "{kind:?}: {listed:?}"
        );

        let (status, mine) = json_of(&app, &cred, "/v1/me").await;
        assert_eq!(status, StatusCode::OK, "{kind:?}");
        assert!(mine.get("has_password").is_none(), "{kind:?}: {mine}");

        let (status, org) = json_of(&app, &cred, "/v1/orgs/acme").await;
        assert_eq!(status, StatusCode::OK, "{kind:?}");
        assert!(
            org.get("role").is_none_or(|r| r.is_null()),
            "{kind:?}: {org}"
        );

        let (_, locks) = json_of(&app, &cred, "/v1/me/locks").await;
        assert!(
            locks
                .as_array()
                .unwrap()
                .iter()
                .all(|l| l["name"] == "game"),
            "{kind:?}"
        );
    }
}

/// A scoped token of the account is not the account: what the owner hid stays hidden from it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_scoped_token_reads_its_owners_profile_as_a_stranger_does() {
    for kind in [Kind::ScopedAll, Kind::ReadOnly] {
        let (app, cred) = world(kind, Reach::AnyCredential).await;
        let body = json!({"bio": "private bio", "visibility": {"bio": false}}).to_string();
        let r = send(
            &app,
            &Cred::dev("alice"),
            &Method::PUT,
            "/v1/me/profile",
            Some(body),
            &[],
        )
        .await;
        assert!(r.status.is_success(), "{kind:?}: {}", r.status);
        let (status, profile) = json_of(&app, &cred, "/v1/users/alice").await;
        assert_eq!(status, StatusCode::OK, "{kind:?}");
        assert!(profile["profile"]["bio"].is_null(), "{kind:?}: {profile}");
        assert!(profile["profile"].get("visibility").is_none(), "{kind:?}");
        let (_, mine) = json_of(&app, &cred, "/v1/me").await;
        assert!(mine["profile"]["bio"].is_null(), "{kind:?}: {mine}");
    }
}

/// The credential-kind table of the access doc names every reach in use.
#[test]
fn the_access_doc_table_names_every_reach() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/concepts/access.md");
    let doc = std::fs::read_to_string(path).unwrap();
    let start = doc.find("### What each credential kind reaches").unwrap();
    let end = doc[start..].find("### Repository routes").unwrap() + start;
    let section = &doc[start..end];
    let missing: BTreeSet<String> = declarations()
        .iter()
        .map(|d| d.reach.label().split(':').next().unwrap().to_string())
        .filter(|label| !section.contains(&format!("`{label}")))
        .collect();
    assert!(
        missing.is_empty(),
        "not in the access doc table: {missing:?}"
    );
}

/// The people directory is for full-scope credentials and signed-out callers on a public server only.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_people_directory_refuses_scoped_tokens_and_service_credentials() {
    for kind in [Kind::ReadOnly, Kind::ScopedAll, Kind::OtherRepo] {
        let (app, cred) = world_in(AccessMode::Public, kind, Reach::OptionalFullScope).await;
        let r = send(&app, &cred, &Method::GET, "/v1/users", None, &[]).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{kind:?}");
        assert_eq!(r.code().as_deref(), Some("forbidden"), "{kind:?}");
    }
    let (app, cred) = world_in(
        AccessMode::Public,
        Kind::ServiceAll,
        Reach::OptionalFullScope,
    )
    .await;
    let r = send(&app, &cred, &Method::GET, "/v1/users", None, &[]).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(r.code().as_deref(), Some("service_credential_not_allowed"));
    for kind in [Kind::SignedOut, Kind::Session, Kind::Full, Kind::Admin] {
        let (app, cred) = world_in(AccessMode::Public, kind, Reach::OptionalFullScope).await;
        let r = send(&app, &cred, &Method::GET, "/v1/users", None, &[]).await;
        assert_eq!(r.status, StatusCode::OK, "{kind:?}");
    }
}
