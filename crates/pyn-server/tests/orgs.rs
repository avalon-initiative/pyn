use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore,
};
use pyn_core::{
    AccessConfig, AccessService, AccessStore, AuthProvider, OrgCreation, OrgDeleteMark,
    RegistrationMode, Repositories, Rules, SystemClock, UserId,
};
use pyn_proto as api;
use pyn_server::auth::{BearerAuth, DevHeaderAuth};
use pyn_server::{AppState, router};
use serde_json::json;
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";

/// The app and a token for the bootstrap administrator `root`.
async fn server_with_root(org_creation: OrgCreation) -> (Router, String) {
    let (app, root, _) = server_with_store(org_creation).await;
    (app, root)
}

async fn server_with_store(org_creation: OrgCreation) -> (Router, String, Arc<MemoryAccessStore>) {
    let clock = Arc::new(SystemClock);
    let objects = Arc::new(MemoryObjectStore::new());
    let audit = Arc::new(MemoryAuditStore::new());
    let meta = Arc::new(MemoryMetadataStore::new());
    let store = Arc::new(MemoryAccessStore::new());
    let access = Arc::new(
        AccessService::new(store.clone(), clock.clone())
            .with_config(AccessConfig {
                registration: RegistrationMode::Open,
                require_email_verification: false,
                org_creation,
                ..AccessConfig::default()
            })
            .with_registry(meta.clone())
            .with_audit(audit.clone()),
    );
    let root = access.bootstrap_admin(&UserId::new("root")).await.unwrap();
    let repos = Arc::new(Repositories::new(
        meta,
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
    });
    (app, root, store)
}

async fn server(org_creation: OrgCreation) -> Router {
    server_with_root(org_creation).await.0
}

/// Signs `name` up and returns a bearer token; unlike the development header it carries no admin rights.
async fn token_for(app: &Router, name: &str) -> String {
    let body = json!({"username": name, "password": PASSWORD});
    let r = call(app, "root", "POST", "/v1/register", Some(body.clone())).await;
    assert_eq!(r.status, StatusCode::CREATED);
    let r = call(app, "root", "POST", "/v1/login", Some(body)).await;
    r.json::<api::CreatedToken>().token
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
        self.json::<api::ErrorBody>().code
    }
}

async fn call(
    app: &Router,
    user: &str,
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
) -> Reply {
    let builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    let builder = match user.strip_prefix("pyn_") {
        Some(_) => builder.header("authorization", format!("Bearer {user}")),
        None => builder.header(api::DEV_USER_HEADER, user),
    };
    let req = builder
        .body(Body::from(body.map(|b| b.to_string()).unwrap_or_default()))
        .unwrap();
    let r = app.clone().oneshot(req).await.unwrap();
    Reply {
        status: r.status(),
        body: r.into_body().collect().await.unwrap().to_bytes().to_vec(),
    }
}

async fn create_org(app: &Router, user: &str, name: &str) -> Reply {
    call(app, user, "POST", "/v1/orgs", Some(json!({ "name": name }))).await
}

#[tokio::test]
async fn creating_an_organization_makes_the_caller_its_owner() {
    let app = server(OrgCreation::Anyone).await;
    let r = create_org(&app, "alice", "acme").await;
    assert_eq!(r.status, StatusCode::CREATED);
    let org = r.json::<api::OrgInfo>();
    assert_eq!(
        (org.name.as_str(), org.role.as_deref()),
        ("acme", Some("owner"))
    );

    let r = call(&app, "alice", "GET", "/v1/orgs/acme", None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json::<api::OrgInfo>().role.as_deref(), Some("owner"));
    let r = call(&app, "bob", "GET", "/v1/orgs/acme", None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        r.json::<api::OrgInfo>().role,
        None,
        "a stranger sees no role"
    );

    create_org(&app, "alice", "beta").await;
    let r = call(&app, "alice", "GET", "/v1/orgs", None).await;
    let names: Vec<_> = r
        .json::<Vec<api::OrgInfo>>()
        .into_iter()
        .map(|o| o.name)
        .collect();
    assert_eq!(names, ["acme", "beta"]);
    let r = call(&app, "bob", "GET", "/v1/orgs", None).await;
    assert!(r.json::<Vec<api::OrgInfo>>().is_empty());

    let r = call(&app, "alice", "GET", "/v1/orgs/nobody", None).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::NOT_FOUND, "org_not_found")
    );
    let r = call(&app, "bob", "GET", "/v1/orgs/alice", None).await;
    assert_eq!(
        r.code(),
        "org_not_found",
        "a user name is not an organization"
    );
    let r = call(&app, "alice", "GET", "/v1/me", None).await;
    assert_eq!(r.status, StatusCode::OK);
}

#[tokio::test]
async fn names_are_validated_unique_and_not_reserved() {
    let app = server(OrgCreation::Anyone).await;
    create_org(&app, "alice", "acme").await;

    let r = create_org(&app, "bob", "acme").await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::CONFLICT, "user_exists")
    );
    let r = create_org(&app, "bob", "alice").await;
    assert_eq!(r.status, StatusCode::CONFLICT, "a user's name is taken too");
    for name in ["_", "orgs", "settings", "admin", "-"] {
        let r = create_org(&app, "bob", name).await;
        assert_eq!(
            (r.status, r.code().as_str()),
            (StatusCode::BAD_REQUEST, "reserved_name"),
            "{name}"
        );
    }
    let r = create_org(&app, "bob", "Bad Name").await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::BAD_REQUEST, "invalid_request")
    );

    let r = call(
        &app,
        "root",
        "POST",
        "/v1/register",
        Some(json!({"username": "acme", "password": PASSWORD})),
    )
    .await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::CONFLICT, "user_exists")
    );
    let r = call(
        &app,
        "root",
        "POST",
        "/v1/register",
        Some(json!({"username": "settings", "password": PASSWORD})),
    )
    .await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::BAD_REQUEST, "reserved_name")
    );
}

#[tokio::test]
async fn an_organization_cannot_sign_in_or_be_acted_as() {
    let app = server(OrgCreation::Anyone).await;
    create_org(&app, "alice", "acme").await;

    let r = call(
        &app,
        "root",
        "POST",
        "/v1/login",
        Some(json!({"username": "acme", "password": PASSWORD})),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r = call(&app, "acme", "GET", "/v1/me", None).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r = call(&app, "root", "GET", "/v1/admin/users", None).await;
    let users = r.json::<Vec<api::AccountInfo>>();
    assert!(
        users.iter().all(|u| u.user != "acme"),
        "admin lists hold users only"
    );
    let r = call(
        &app,
        "root",
        "POST",
        "/v1/admin/users/acme/disable",
        Some(json!({})),
    )
    .await;
    assert_eq!(r.code(), "user_not_found");
}

#[tokio::test]
async fn the_server_can_limit_creation_to_administrators() {
    let (app, root) = server_with_root(OrgCreation::AdminsOnly).await;
    let alice = token_for(&app, "alice").await;
    let r = create_org(&app, &alice, "acme").await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::FORBIDDEN, "server_admin_required")
    );
    assert_eq!(
        create_org(&app, &root, "acme").await.status,
        StatusCode::CREATED
    );
}

#[tokio::test]
async fn repositories_in_an_organization_are_created_and_run_by_its_owners() {
    let app = server(OrgCreation::Anyone).await;
    let (alice, bob) = (token_for(&app, "alice").await, token_for(&app, "bob").await);
    create_org(&app, &alice, "acme").await;

    let r = call(
        &app,
        &bob,
        "POST",
        "/v1/repos",
        Some(json!({"owner": "acme", "name": "game"})),
    )
    .await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::FORBIDDEN, "not_org_owner")
    );

    let r = call(
        &app,
        &alice,
        "POST",
        "/v1/repos",
        Some(json!({"owner": "acme", "name": "game"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED);
    let repo = r.json::<api::RepoInfo>();
    assert_eq!(
        (repo.owner.as_str(), repo.role.as_deref()),
        ("acme", Some("admin"))
    );

    let r = call(&app, &alice, "GET", "/v1/repos", None).await;
    let repos = r.json::<Vec<api::RepoInfo>>();
    assert_eq!(repos.len(), 1);
    assert_eq!(repos[0].role.as_deref(), Some("admin"));
    let r = call(&app, &alice, "GET", "/v1/repos/acme/game/me", None).await;
    assert_eq!(
        r.json::<api::Me>().permissions.len(),
        pyn_core::Permission::ALL.len()
    );
    let r = call(&app, &bob, "GET", "/v1/repos/acme/game", None).await;
    assert_eq!(
        r.status,
        StatusCode::NOT_FOUND,
        "private to everyone but its admins"
    );

    let r = call(
        &app,
        &alice,
        "PATCH",
        "/v1/repos/acme/game",
        Some(json!({"visibility": "public"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);

    let r = call(&app, &alice, "DELETE", "/v1/orgs/acme", None).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::CONFLICT, "org_not_empty")
    );
    let r = call(&app, &bob, "DELETE", "/v1/orgs/acme", None).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::FORBIDDEN, "not_org_owner")
    );

    let r = call(&app, &alice, "DELETE", "/v1/repos/acme/game", None).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let r = call(&app, &alice, "DELETE", "/v1/orgs/acme", None).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let r = call(&app, &alice, "GET", "/v1/orgs/acme", None).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = call(&app, &alice, "DELETE", "/v1/orgs/acme", None).await;
    assert_eq!(r.code(), "org_not_found");
}

#[tokio::test]
async fn the_organization_log_lists_creation_for_owners_only() {
    let app = server(OrgCreation::Anyone).await;
    create_org(&app, "alice", "acme").await;

    let r = call(&app, "alice", "GET", "/v1/orgs/acme/audit", None).await;
    assert_eq!(r.status, StatusCode::OK);
    let page = r.json::<api::AuditPage>();
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.entries[0].action, "org_created");
    assert_eq!(page.entries[0].actor, "alice");

    let r = call(&app, "bob", "GET", "/v1/orgs/acme/audit", None).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::FORBIDDEN, "not_org_owner")
    );
    let r = call(&app, "root", "GET", "/v1/admin/audit", None).await;
    assert!(
        r.json::<api::AuditPage>().entries.is_empty(),
        "not in the server log"
    );
}

#[tokio::test]
async fn openapi_documents_the_organization_routes() {
    let app = server(OrgCreation::Anyone).await;
    let r = app
        .oneshot(Request::get("/openapi.json").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let doc: serde_json::Value =
        serde_json::from_slice(&r.into_body().collect().await.unwrap().to_bytes()).unwrap();
    for path in [
        "/v1/orgs",
        "/v1/orgs/{org}",
        "/v1/orgs/{org}/audit",
        "/v1/orgs/{org}/members",
        "/v1/orgs/{org}/members/{user}",
        "/v1/me/orgs",
    ] {
        assert!(doc["paths"].get(path).is_some(), "{path}");
    }
}

fn code(r: &Reply) -> (u16, String) {
    (r.status.as_u16(), r.code())
}

async fn members(app: &Router, who: &str, org: &str) -> Vec<(String, String)> {
    let r = call(app, who, "GET", &format!("/v1/orgs/{org}/members"), None).await;
    assert_eq!(r.status, StatusCode::OK);
    r.json::<Vec<api::OrgMember>>()
        .into_iter()
        .map(|m| (m.user, m.role))
        .collect()
}

#[tokio::test]
async fn owners_manage_members_and_any_member_lists_them() {
    let app = server(OrgCreation::Anyone).await;
    let (alice, bob, carol) = (
        token_for(&app, "alice").await,
        token_for(&app, "bob").await,
        token_for(&app, "carol").await,
    );
    create_org(&app, &alice, "acme").await;
    let url = "/v1/orgs/acme/members";

    let r = call(&app, &bob, "GET", url, None).await;
    assert_eq!(code(&r), (403, "not_org_member".into()));
    let r = call(&app, &alice, "POST", url, Some(json!({"user": "bob"}))).await;
    assert_eq!(r.status, StatusCode::CREATED);
    let added = r.json::<api::OrgMember>();
    assert_eq!(
        (added.user.as_str(), added.role.as_str()),
        ("bob", "member"),
        "member is the default"
    );
    let r = call(&app, &alice, "POST", url, Some(json!({"user": "bob"}))).await;
    assert_eq!(code(&r), (409, "already_org_member".into()));
    let r = call(&app, &alice, "POST", url, Some(json!({"user": "nobody"}))).await;
    assert_eq!(code(&r), (404, "user_not_found".into()));
    let r = call(&app, &alice, "POST", url, Some(json!({"user": "acme"}))).await;
    assert_eq!(
        code(&r),
        (400, "invalid_request".into()),
        "an organization is no member"
    );
    let r = call(
        &app,
        &alice,
        "POST",
        url,
        Some(json!({"user": "carol", "role": "boss"})),
    )
    .await;
    assert_eq!(code(&r), (400, "invalid_request".into()));
    let r = call(&app, &bob, "POST", url, Some(json!({"user": "carol"}))).await;
    assert_eq!(code(&r), (403, "not_org_owner".into()));
    let r = call(&app, &carol, "POST", url, Some(json!({"user": "carol"}))).await;
    assert_eq!(code(&r), (403, "not_org_owner".into()));
    let r = call(
        &app,
        &alice,
        "POST",
        "/v1/orgs/nope/members",
        Some(json!({"user": "carol"})),
    )
    .await;
    assert_eq!(code(&r), (404, "org_not_found".into()));
    call(
        &app,
        &alice,
        "POST",
        url,
        Some(json!({"user": "carol", "role": "owner"})),
    )
    .await;

    for who in [&alice, &bob, &carol] {
        assert_eq!(
            members(&app, who, "acme").await,
            [
                ("alice".to_string(), "owner".to_string()),
                ("bob".to_string(), "member".to_string()),
                ("carol".to_string(), "owner".to_string())
            ]
        );
    }

    let r = call(
        &app,
        &bob,
        "PATCH",
        "/v1/orgs/acme/members/bob",
        Some(json!({"role": "owner"})),
    )
    .await;
    assert_eq!(code(&r), (403, "not_org_owner".into()));
    let r = call(
        &app,
        &alice,
        "PATCH",
        "/v1/orgs/acme/members/nobody",
        Some(json!({"role": "owner"})),
    )
    .await;
    assert_eq!(code(&r), (404, "org_member_not_found".into()));
    let r = call(
        &app,
        &alice,
        "PATCH",
        "/v1/orgs/acme/members/bob",
        Some(json!({"role": "owner"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json::<api::OrgMember>().role, "owner");

    let r = call(&app, &bob, "DELETE", "/v1/orgs/acme/members/carol", None).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let r = call(&app, &alice, "DELETE", "/v1/orgs/acme/members/carol", None).await;
    assert_eq!(code(&r), (404, "org_member_not_found".into()));
    let r = call(&app, &carol, "GET", url, None).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);

    let r = call(&app, &alice, "GET", "/v1/orgs/acme/audit", None).await;
    let actions: Vec<_> = r
        .json::<api::AuditPage>()
        .entries
        .into_iter()
        .map(|e| e.action)
        .collect();
    assert_eq!(
        actions,
        [
            "org_member_removed",
            "org_member_role_changed",
            "org_member_added",
            "org_member_added",
            "org_created"
        ]
    );
}

#[tokio::test]
async fn the_last_owner_cannot_leave_or_be_demoted() {
    let app = server(OrgCreation::Anyone).await;
    let (alice, bob) = (token_for(&app, "alice").await, token_for(&app, "bob").await);
    create_org(&app, &alice, "acme").await;
    call(
        &app,
        &alice,
        "POST",
        "/v1/orgs/acme/members",
        Some(json!({"user": "bob"})),
    )
    .await;

    let r = call(
        &app,
        &alice,
        "PATCH",
        "/v1/orgs/acme/members/alice",
        Some(json!({"role": "member"})),
    )
    .await;
    assert_eq!(code(&r), (409, "last_org_owner".into()));
    let r = call(&app, &alice, "DELETE", "/v1/orgs/acme/members/alice", None).await;
    assert_eq!(code(&r), (409, "last_org_owner".into()));
    let r = call(&app, &bob, "DELETE", "/v1/orgs/acme/members/bob", None).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "a member may leave");
    assert_eq!(members(&app, &alice, "acme").await.len(), 1);
}

#[tokio::test]
async fn my_orgs_lists_the_callers_organizations_with_their_role() {
    let app = server(OrgCreation::Anyone).await;
    let (alice, bob) = (token_for(&app, "alice").await, token_for(&app, "bob").await);
    create_org(&app, &alice, "acme").await;
    create_org(&app, &bob, "beta").await;
    call(
        &app,
        &bob,
        "POST",
        "/v1/orgs/beta/members",
        Some(json!({"user": "alice"})),
    )
    .await;

    let r = call(&app, &alice, "GET", "/v1/me/orgs", None).await;
    assert_eq!(r.status, StatusCode::OK);
    let orgs: Vec<_> = r
        .json::<Vec<api::OrgInfo>>()
        .into_iter()
        .map(|o| (o.name, o.role.unwrap()))
        .collect();
    assert_eq!(
        orgs,
        [
            ("acme".to_string(), "owner".to_string()),
            ("beta".to_string(), "member".to_string())
        ]
    );
    let r = call(&app, "nobody", "GET", "/v1/me/orgs", None).await;
    assert!(r.json::<Vec<api::OrgInfo>>().is_empty());
}

#[tokio::test]
async fn removal_drops_direct_grants_and_direct_grants_need_membership() {
    let app = server(OrgCreation::Anyone).await;
    let (alice, bob) = (token_for(&app, "alice").await, token_for(&app, "bob").await);
    token_for(&app, "carol").await;
    create_org(&app, &alice, "acme").await;
    call(
        &app,
        &alice,
        "POST",
        "/v1/repos",
        Some(json!({"owner": "acme", "name": "game"})),
    )
    .await;
    call(
        &app,
        &alice,
        "POST",
        "/v1/orgs/acme/members",
        Some(json!({"user": "bob"})),
    )
    .await;

    let r = call(&app, &bob, "GET", "/v1/repos/acme/game/me", None).await;
    assert_eq!(
        r.status,
        StatusCode::NOT_FOUND,
        "the member role grants no access"
    );

    let r = call(
        &app,
        &alice,
        "PUT",
        "/v1/repos/acme/game/members/carol",
        Some(json!({"role": "reader"})),
    )
    .await;
    assert_eq!(code(&r), (409, "user_not_org_member".into()));
    let r = call(
        &app,
        &alice,
        "POST",
        "/v1/repos/acme/game/users",
        Some(json!({"username": "dave", "password": PASSWORD, "role": "reader"})),
    )
    .await;
    assert_eq!(code(&r), (409, "user_not_org_member".into()));
    let r = call(
        &app,
        &alice,
        "POST",
        "/v1/repos/acme/game/invites",
        Some(json!({"role": "reader", "hours": 1})),
    )
    .await;
    assert_eq!(code(&r), (400, "invalid_invite".into()));

    let r = call(
        &app,
        &alice,
        "PUT",
        "/v1/repos/acme/game/members/bob",
        Some(json!({"role": "writer"})),
    )
    .await;
    assert!(r.status.is_success(), "{:?}", r.status);
    let r = call(&app, &bob, "GET", "/v1/repos/acme/game/me", None).await;
    assert_eq!(r.status, StatusCode::OK);

    let r = call(&app, &alice, "DELETE", "/v1/orgs/acme/members/bob", None).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let r = call(&app, &bob, "GET", "/v1/repos/acme/game/me", None).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn an_organization_being_deleted_refuses_new_repositories_and_a_second_delete() {
    let (app, _, store) = server_with_store(OrgCreation::Anyone).await;
    create_org(&app, "alice", "acme").await;
    let now = chrono::Utc::now();
    let mark = store
        .mark_org_deleting(
            &UserId::new("acme"),
            now,
            now - chrono::Duration::seconds(60),
        )
        .await
        .unwrap();
    assert_eq!(mark, OrgDeleteMark::Set);

    let r = call(
        &app,
        "alice",
        "POST",
        "/v1/repos",
        Some(json!({"owner": "acme", "name": "game"})),
    )
    .await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::CONFLICT, "org_deleting")
    );
    let r = call(&app, "alice", "DELETE", "/v1/orgs/acme", None).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::CONFLICT, "org_deleting")
    );

    store
        .clear_org_deleting(&UserId::new("acme"))
        .await
        .unwrap();
    let r = call(
        &app,
        "alice",
        "POST",
        "/v1/repos",
        Some(json!({"owner": "acme", "name": "game"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED);
}
