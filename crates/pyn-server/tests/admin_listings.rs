//! `GET /v1/admin/repositories` and `GET /v1/admin/organizations`: administrators only, paged, filterable.

#[path = "common/cred.rs"]
mod cred;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use cred::{Cred, WithCred};
use http_body_util::BodyExt;
use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore,
};
use pyn_core::{
    AccessConfig, AccessService, AuthProvider, RegistrationMode, Repositories, Rules, SystemClock,
};
use pyn_proto as api;
use pyn_server::auth::{BearerAuth, DevHeaderAuth};
use pyn_server::{AppState, router};
use serde_json::{Value, json};
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";

fn server() -> Router {
    let clock = Arc::new(SystemClock);
    let objects = Arc::new(MemoryObjectStore::new());
    let audit = Arc::new(MemoryAuditStore::new());
    let access = Arc::new(
        AccessService::new(
            Arc::new(MemoryAccessStore::new()),
            clock.clone(),
            Arc::new(pyn_core::MemoryLimitEvents::new()),
        )
        .with_config(AccessConfig {
            registration: RegistrationMode::InviteOnly,
            require_email_verification: false,
            ..AccessConfig::default()
        })
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
    router(AppState {
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

async fn token_for(app: &Router, name: &str) -> String {
    let r = send(
        app,
        "root",
        "POST",
        "/v1/admin/users",
        Some(json!({"username": name, "password": PASSWORD})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED);
    let r = send(
        app,
        "",
        "POST",
        "/v1/login",
        Some(json!({"username": name, "password": PASSWORD})),
    )
    .await;
    r.json::<api::CreatedToken>().token
}

/// Two people, a public and a private repository each, and two organizations (acme with two members).
async fn world() -> (Router, String, String) {
    let app = server();
    let alice = token_for(&app, "alice").await;
    let bob = token_for(&app, "bob").await;
    for (who, name, visibility) in [
        (&alice, "game", "private"),
        (&alice, "site", "public"),
        (&bob, "tools", "private"),
    ] {
        let r = send(
            &app,
            who,
            "POST",
            "/v1/repos",
            Some(json!({"name": name, "visibility": visibility})),
        )
        .await;
        assert_eq!(r.status, StatusCode::CREATED);
    }
    for name in ["acme", "zenith"] {
        let r = send(
            &app,
            &alice,
            "POST",
            "/v1/orgs",
            Some(json!({"name": name})),
        )
        .await;
        assert_eq!(r.status, StatusCode::CREATED);
    }
    let r = send(
        &app,
        &alice,
        "POST",
        "/v1/orgs/acme/members",
        Some(json!({"user": "bob", "role": "member"})),
    )
    .await;
    assert!(
        r.status.is_success(),
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    send(&app, &bob, "POST", "/v1/me/invitations/acme/accept", None).await;
    (app, alice, bob)
}

fn addresses(page: &api::AdminRepoPage) -> Vec<String> {
    page.repos
        .iter()
        .map(|r| format!("{}/{}", r.owner, r.name))
        .collect()
}

#[tokio::test]
async fn an_administrator_lists_every_repository_private_ones_included() {
    let (app, _, _) = world().await;
    let r = send(&app, "root", "GET", "/v1/admin/repositories", None).await;
    assert_eq!(r.status, StatusCode::OK);
    let page: api::AdminRepoPage = r.json();
    assert_eq!(addresses(&page), ["alice/game", "alice/site", "bob/tools"]);
    assert_eq!(page.next_after, None);
    let visibilities: Vec<_> = page.repos.iter().map(|r| r.visibility).collect();
    assert_eq!(
        visibilities,
        [
            api::Visibility::Private,
            api::Visibility::Public,
            api::Visibility::Private
        ]
    );
}

#[tokio::test]
async fn the_repository_listing_pages_by_address_and_filters_by_name() {
    let (app, _, _) = world().await;
    let r = send(&app, "root", "GET", "/v1/admin/repositories?limit=2", None).await;
    let first: api::AdminRepoPage = r.json();
    assert_eq!(addresses(&first), ["alice/game", "alice/site"]);
    assert_eq!(first.next_after.as_deref(), Some("alice/site"));
    let r = send(
        &app,
        "root",
        "GET",
        "/v1/admin/repositories?limit=2&after=alice/site",
        None,
    )
    .await;
    let second: api::AdminRepoPage = r.json();
    assert_eq!(addresses(&second), ["bob/tools"]);
    assert_eq!(second.next_after, None);

    let r = send(&app, "root", "GET", "/v1/admin/repositories?q=TOOL", None).await;
    assert_eq!(addresses(&r.json::<api::AdminRepoPage>()), ["bob/tools"]);
    let r = send(&app, "root", "GET", "/v1/admin/repositories?q=alice/", None).await;
    assert_eq!(
        addresses(&r.json::<api::AdminRepoPage>()),
        ["alice/game", "alice/site"]
    );
    let r = send(&app, "root", "GET", "/v1/admin/repositories?q=nope", None).await;
    assert!(r.json::<api::AdminRepoPage>().repos.is_empty());

    let long = "q".repeat(65);
    let r = send(
        &app,
        "root",
        "GET",
        &format!("/v1/admin/repositories?q={long}"),
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = send(
        &app,
        "root",
        "GET",
        "/v1/admin/repositories?after=nocursor",
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(r.json::<api::ErrorBody>().code, "invalid_request");
}

#[tokio::test]
async fn an_administrator_lists_organizations_with_member_counts_paged_and_filtered() {
    let (app, _, _) = world().await;
    let r = send(&app, "root", "GET", "/v1/admin/organizations", None).await;
    assert_eq!(r.status, StatusCode::OK);
    let page: api::AdminOrgPage = r.json();
    let seen: Vec<_> = page
        .organizations
        .iter()
        .map(|o| (o.name.as_str(), o.member_count))
        .collect();
    assert_eq!(seen, [("acme", 2), ("zenith", 1)]);
    assert!(page.organizations.iter().all(|o| !o.id.is_empty()));
    assert_eq!(page.next_after, None);

    let r = send(&app, "root", "GET", "/v1/admin/organizations?limit=1", None).await;
    let first: api::AdminOrgPage = r.json();
    assert_eq!(first.organizations[0].name, "acme");
    assert_eq!(first.next_after.as_deref(), Some("acme"));
    let r = send(
        &app,
        "root",
        "GET",
        "/v1/admin/organizations?limit=1&after=acme",
        None,
    )
    .await;
    let second: api::AdminOrgPage = r.json();
    assert_eq!(second.organizations[0].name, "zenith");
    assert_eq!(second.next_after, None);

    let r = send(&app, "root", "GET", "/v1/admin/organizations?q=ZEN", None).await;
    let found: api::AdminOrgPage = r.json();
    assert_eq!(found.organizations.len(), 1);
    assert_eq!(found.organizations[0].name, "zenith");
}

#[tokio::test]
async fn only_an_administrator_reaches_the_listings() {
    let (app, alice, _) = world().await;
    for uri in ["/v1/admin/repositories", "/v1/admin/organizations"] {
        let r = send(&app, &alice, "GET", uri, None).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{uri}");
        assert_eq!(r.json::<api::ErrorBody>().code, "server_admin_required");
        let r = send(&app, "", "GET", uri, None).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{uri}");
    }
}
