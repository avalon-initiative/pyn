use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore,
};
use pyn_core::{
    AccessConfig, AccessService, AuthProvider, OrgCreation, RegistrationMode, Repositories, Rules,
    SystemClock, UserId,
};
use pyn_proto as api;
use pyn_server::auth::{BearerAuth, DevHeaderAuth};
use pyn_server::{AppState, router};
use serde_json::json;
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";

/// The app and a token for the administrator `root`.
async fn server_with_root(org_creation: OrgCreation) -> (Router, String) {
    let clock = Arc::new(SystemClock);
    let objects = Arc::new(MemoryObjectStore::new());
    let audit = Arc::new(MemoryAuditStore::new());
    let meta = Arc::new(MemoryMetadataStore::new());
    let access = Arc::new(
        AccessService::new(Arc::new(MemoryAccessStore::new()), clock.clone())
            .with_config(AccessConfig {
                registration: RegistrationMode::Open,
                require_email_verification: false,
                org_creation,
                ..AccessConfig::default()
            })
            .with_registry(meta.clone())
            .with_audit(audit.clone()),
    );
    let root = access.admin_for_tests(&UserId::new("root")).await.unwrap();
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
    (app, root)
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

async fn post(app: &Router, who: &str, uri: &str, body: serde_json::Value) -> Reply {
    call(app, who, "POST", uri, Some(body)).await
}

fn code(r: &Reply) -> (u16, String) {
    (r.status.as_u16(), r.code())
}

struct Org {
    app: Router,
    alice: String,
    bob: String,
    carol: String,
}

/// `acme` owned by alice with bob and carol as members and one repository, `acme/game`.
async fn acme() -> Org {
    let app = server(OrgCreation::Anyone).await;
    let (alice, bob, carol) = (
        token_for(&app, "alice").await,
        token_for(&app, "bob").await,
        token_for(&app, "carol").await,
    );
    post(&app, &alice, "/v1/orgs", json!({"name": "acme"})).await;
    for u in ["bob", "carol"] {
        post(&app, &alice, "/v1/orgs/acme/members", json!({"user": u})).await;
    }
    let r = post(
        &app,
        &alice,
        "/v1/repos",
        json!({"owner": "acme", "name": "game"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED);
    Org {
        app,
        alice,
        bob,
        carol,
    }
}

#[tokio::test]
async fn owners_create_update_and_delete_teams_and_members_list_them() {
    let o = acme().await;
    let (app, alice, bob) = (&o.app, &o.alice, &o.bob);
    let url = "/v1/orgs/acme/teams";

    let r = post(
        app,
        alice,
        url,
        json!({"slug": "art", "name": "Artists", "description": "pixels"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED);
    let t = r.json::<api::TeamInfo>();
    assert_eq!(
        (t.slug.as_str(), t.name.as_str(), t.description.as_str()),
        ("art", "Artists", "pixels")
    );
    assert!(t.members.is_empty() && t.repos.is_empty());
    let r = post(app, alice, url, json!({"slug": "eng"})).await;
    assert_eq!(
        r.json::<api::TeamInfo>().name,
        "eng",
        "the name defaults to the slug"
    );
    assert_eq!(
        code(&post(app, alice, url, json!({"slug": "art"})).await),
        (409, "team_exists".into())
    );
    assert_eq!(
        code(&post(app, alice, url, json!({"slug": "Bad Slug"})).await),
        (400, "invalid_request".into())
    );
    assert_eq!(
        code(&post(app, alice, url, json!({"slug": "x", "name": " "})).await),
        (400, "invalid_request".into())
    );
    assert_eq!(
        code(&post(app, bob, url, json!({"slug": "ops"})).await),
        (403, "not_org_owner".into())
    );
    assert_eq!(
        code(&post(app, alice, "/v1/orgs/nope/teams", json!({"slug": "ops"})).await),
        (404, "org_not_found".into())
    );

    let r = call(app, bob, "GET", url, None).await;
    assert_eq!(r.status, StatusCode::OK);
    let slugs: Vec<_> = r
        .json::<Vec<api::TeamInfo>>()
        .into_iter()
        .map(|t| t.slug)
        .collect();
    assert_eq!(slugs, ["art", "eng"]);
    let stranger = token_for(app, "dave").await;
    assert_eq!(
        code(&call(app, &stranger, "GET", url, None).await),
        (403, "not_org_member".into())
    );
    assert_eq!(
        code(&call(app, bob, "GET", "/v1/orgs/acme/teams/none", None).await),
        (404, "team_not_found".into())
    );

    let r = call(
        app,
        alice,
        "PATCH",
        "/v1/orgs/acme/teams/art",
        Some(json!({"description": "art and ui"})),
    )
    .await;
    let t = r.json::<api::TeamInfo>();
    assert_eq!(
        (t.name.as_str(), t.description.as_str()),
        ("Artists", "art and ui")
    );
    assert_eq!(
        code(
            &call(
                app,
                bob,
                "PATCH",
                "/v1/orgs/acme/teams/art",
                Some(json!({"name": "x"}))
            )
            .await
        ),
        (403, "not_org_owner".into())
    );

    assert_eq!(
        call(app, bob, "DELETE", "/v1/orgs/acme/teams/art", None)
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(app, alice, "DELETE", "/v1/orgs/acme/teams/eng", None)
            .await
            .status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        code(&call(app, alice, "DELETE", "/v1/orgs/acme/teams/eng", None).await),
        (404, "team_not_found".into())
    );
}

#[tokio::test]
async fn team_members_must_be_organization_members() {
    let o = acme().await;
    let (app, alice, bob) = (&o.app, &o.alice, &o.bob);
    post(app, alice, "/v1/orgs/acme/teams", json!({"slug": "art"})).await;
    let member = |u: &str| format!("/v1/orgs/acme/teams/art/members/{u}");
    token_for(app, "dave").await;

    assert_eq!(
        call(app, alice, "PUT", &member("bob"), None).await.status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(app, alice, "PUT", &member("bob"), None).await.status,
        StatusCode::NO_CONTENT,
        "putting a member again is harmless"
    );
    assert_eq!(
        code(&call(app, alice, "PUT", &member("dave"), None).await),
        (409, "user_not_org_member".into())
    );
    assert_eq!(
        code(&call(app, bob, "PUT", &member("carol"), None).await),
        (403, "not_org_owner".into())
    );
    assert_eq!(
        code(
            &call(
                app,
                alice,
                "PUT",
                "/v1/orgs/acme/teams/none/members/bob",
                None
            )
            .await
        ),
        (404, "team_not_found".into())
    );

    let t = call(app, bob, "GET", "/v1/orgs/acme/teams/art", None)
        .await
        .json::<api::TeamInfo>();
    assert_eq!(t.members, ["bob"]);
    assert_eq!(
        call(app, alice, "DELETE", &member("bob"), None)
            .await
            .status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        code(&call(app, alice, "DELETE", &member("bob"), None).await),
        (404, "team_member_not_found".into())
    );
    assert_eq!(
        code(&call(app, bob, "DELETE", &member("bob"), None).await),
        (403, "not_org_owner".into())
    );
}

#[tokio::test]
async fn a_team_grant_gives_its_members_access_and_shows_everywhere() {
    let o = acme().await;
    let (app, alice, bob, carol) = (&o.app, &o.alice, &o.bob, &o.carol);
    post(app, alice, "/v1/orgs/acme/teams", json!({"slug": "art"})).await;
    call(
        app,
        alice,
        "PUT",
        "/v1/orgs/acme/teams/art/members/bob",
        None,
    )
    .await;
    let grant = "/v1/repos/acme/game/teams/art";

    assert_eq!(
        code(&call(app, bob, "GET", "/v1/repos/acme/game", None).await),
        (404, "repo_not_found".into())
    );
    assert_eq!(
        call(app, alice, "PUT", grant, Some(json!({"role": "writer"})))
            .await
            .status,
        StatusCode::NO_CONTENT
    );

    let r = call(app, bob, "GET", "/v1/repos/acme/game", None).await;
    assert_eq!(r.json::<api::RepoInfo>().role.as_deref(), Some("writer"));
    let r = call(app, bob, "GET", "/v1/repos", None).await;
    let listed: Vec<_> = r
        .json::<Vec<api::RepoInfo>>()
        .into_iter()
        .map(|i| (i.owner, i.name, i.role))
        .collect();
    assert_eq!(
        listed,
        [(
            "acme".to_string(),
            "game".to_string(),
            Some("writer".to_string())
        )]
    );
    let r = call(app, bob, "GET", "/v1/repos/acme/game/me", None).await;
    assert!(
        r.json::<api::Me>()
            .permissions
            .contains(&"checkin".to_string())
    );
    assert_eq!(
        code(&call(app, carol, "GET", "/v1/repos/acme/game", None).await),
        (404, "repo_not_found".into())
    );

    let r = call(app, alice, "GET", "/v1/repos/acme/game/teams", None).await;
    let teams = r.json::<Vec<api::RepoTeam>>();
    assert_eq!(teams.len(), 1);
    assert_eq!(
        (teams[0].slug.as_str(), teams[0].role.as_str()),
        ("art", "writer")
    );
    assert_eq!(
        code(&call(app, bob, "GET", "/v1/repos/acme/game/teams", None).await).0,
        403
    );

    let members = call(app, alice, "GET", "/v1/repos/acme/game/members", None)
        .await
        .json::<Vec<api::Member>>();
    let rows: Vec<_> = members
        .iter()
        .map(|m| (m.user.as_str(), m.role.as_str(), m.source.as_str()))
        .collect();
    assert_eq!(
        rows,
        [("alice", "admin", "org_owner"), ("bob", "writer", "team")]
    );

    let t = call(app, alice, "GET", "/v1/orgs/acme/teams/art", None)
        .await
        .json::<api::TeamInfo>();
    assert_eq!(
        t.repos,
        [api::TeamRepo {
            repo: "acme/game".into(),
            role: "writer".into()
        }]
    );

    let r = call(app, alice, "PUT", grant, Some(json!({"role": "boss"}))).await;
    assert_eq!(code(&r), (400, "invalid_request".into()));
    let r = call(
        app,
        alice,
        "PUT",
        "/v1/repos/acme/game/teams/none",
        Some(json!({"role": "reader"})),
    )
    .await;
    assert_eq!(code(&r), (404, "team_not_found".into()));

    assert_eq!(
        call(app, alice, "DELETE", grant, None).await.status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        code(&call(app, bob, "GET", "/v1/repos/acme/game", None).await),
        (404, "repo_not_found".into())
    );
    assert_eq!(
        call(app, bob, "GET", "/v1/repos", None)
            .await
            .json::<Vec<api::RepoInfo>>()
            .len(),
        0
    );
}

#[tokio::test]
async fn only_someone_who_can_grant_the_role_may_set_a_team_role() {
    let o = acme().await;
    let (app, alice, bob, carol) = (&o.app, &o.alice, &o.bob, &o.carol);
    post(app, alice, "/v1/orgs/acme/teams", json!({"slug": "art"})).await;
    call(
        app,
        alice,
        "PUT",
        "/v1/repos/acme/game/members/bob",
        Some(json!({"role": "maintainer"})),
    )
    .await;
    call(
        app,
        alice,
        "PUT",
        "/v1/repos/acme/game/members/carol",
        Some(json!({"role": "writer"})),
    )
    .await;
    let grant = "/v1/repos/acme/game/teams/art";

    for who in [bob, carol] {
        let r = call(app, who, "PUT", grant, Some(json!({"role": "reader"}))).await;
        assert_eq!(r.status.as_u16(), 403, "needs manage_users");
    }
    call(
        app,
        alice,
        "PUT",
        "/v1/repos/acme/game/roles/maintainer",
        Some(json!({"permissions": ["read", "manage_users"]})),
    )
    .await;
    let r = call(app, bob, "PUT", grant, Some(json!({"role": "admin"}))).await;
    assert_eq!(
        code(&r),
        (403, "forbidden".into()),
        "no role above one's own"
    );
    let r = call(app, bob, "PUT", grant, Some(json!({"role": "reader"}))).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn teams_belong_to_organizations_and_repositories_of_users_have_none() {
    let o = acme().await;
    let (app, alice) = (&o.app, &o.alice);
    post(app, alice, "/v1/repos", json!({"name": "mine"})).await;
    let r = call(
        app,
        alice,
        "PUT",
        "/v1/repos/alice/mine/teams/art",
        Some(json!({"role": "reader"})),
    )
    .await;
    assert_eq!(code(&r), (400, "not_org_repo".into()));
    let r = call(app, alice, "GET", "/v1/repos/alice/mine/teams", None).await;
    assert!(r.json::<Vec<api::RepoTeam>>().is_empty());
}

#[tokio::test]
async fn token_access_follows_the_team_role_and_stops_with_it() {
    let o = acme().await;
    let (app, alice, bob) = (&o.app, &o.alice, &o.bob);
    post(app, alice, "/v1/orgs/acme/teams", json!({"slug": "art"})).await;
    call(
        app,
        alice,
        "PUT",
        "/v1/orgs/acme/teams/art/members/bob",
        None,
    )
    .await;
    call(
        app,
        alice,
        "PUT",
        "/v1/repos/acme/game/teams/art",
        Some(json!({"role": "reader"})),
    )
    .await;

    let r = call(app, bob, "GET", "/v1/repos/acme/game/locks", None).await;
    assert_eq!(r.status, StatusCode::OK);
    let r = post(
        app,
        bob,
        "/v1/repos/acme/game/checkout",
        json!({"path": "Content/a.umap"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "a reader cannot lock");
    call(
        app,
        alice,
        "PUT",
        "/v1/repos/acme/game/teams/art",
        Some(json!({"role": "writer"})),
    )
    .await;
    call(app, alice, "DELETE", "/v1/orgs/acme/members/bob", None).await;
    let r = call(app, bob, "GET", "/v1/repos/acme/game/locks", None).await;
    assert_eq!(
        code(&r),
        (404, "repo_not_found".into()),
        "leaving the organization ends team access"
    );
}

#[tokio::test]
async fn team_changes_are_in_the_organization_log_and_grants_in_the_repository_log() {
    let o = acme().await;
    let (app, alice) = (&o.app, &o.alice);
    post(app, alice, "/v1/orgs/acme/teams", json!({"slug": "art"})).await;
    call(
        app,
        alice,
        "PUT",
        "/v1/orgs/acme/teams/art/members/bob",
        None,
    )
    .await;
    call(
        app,
        alice,
        "PUT",
        "/v1/repos/acme/game/teams/art",
        Some(json!({"role": "reader"})),
    )
    .await;
    call(
        app,
        alice,
        "PUT",
        "/v1/repos/acme/game/teams/art",
        Some(json!({"role": "writer"})),
    )
    .await;
    call(app, alice, "DELETE", "/v1/repos/acme/game/teams/art", None).await;
    call(
        app,
        alice,
        "PUT",
        "/v1/repos/acme/game/teams/art",
        Some(json!({"role": "reader"})),
    )
    .await;
    call(
        app,
        alice,
        "DELETE",
        "/v1/orgs/acme/teams/art/members/bob",
        None,
    )
    .await;
    call(app, alice, "DELETE", "/v1/orgs/acme/teams/art", None).await;

    let actions = |uri: &'static str| async move {
        let r = call(app, alice, "GET", uri, None).await;
        let mut a: Vec<_> = r
            .json::<api::AuditPage>()
            .entries
            .into_iter()
            .map(|e| e.action)
            .collect();
        a.reverse();
        a
    };
    assert_eq!(
        actions("/v1/orgs/acme/audit").await,
        [
            "org_created",
            "org_member_added",
            "org_member_added",
            "team_created",
            "team_member_added",
            "team_access_set",
            "team_access_set",
            "team_access_removed",
            "team_access_set",
            "team_member_removed",
            "team_deleted",
        ]
    );
    assert_eq!(
        actions("/v1/repos/acme/game/audit").await,
        [
            "repo_created",
            "team_access_set",
            "team_access_set",
            "team_access_removed",
            "team_access_set",
            "team_access_removed",
        ],
        "deleting the team revokes its grant in the repository log"
    );
}
