//! The HTTP contract of the owner freeze (decision #429): administrators and `manage_freeze` credentials set it,
//! members read it, and every write route under a frozen user or organization answers `owner_frozen`.

use std::collections::BTreeSet;
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
        Rules::from_toml("[meta]\ndefault = \"shared\"\n[exclusive]\npaths = [\"Content/\"]\n")
            .unwrap(),
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

    fn code(&self) -> String {
        serde_json::from_slice::<api::ErrorBody>(&self.body)
            .map(|e| e.code)
            .unwrap_or_default()
    }

    fn is(&self, status: StatusCode, code: &str) -> bool {
        self.status == status && self.code() == code
    }
}

/// `who` is a dev-header user name, or a bearer secret (`pyn_...` token or `pyns_...` service credential).
async fn send(app: &Router, who: &str, method: &str, uri: &str, body: Option<Value>) -> Reply {
    let builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    let builder = if who.starts_with("pyn_") || who.starts_with("pyns_") {
        builder.header("authorization", format!("Bearer {who}"))
    } else {
        builder.header(api::DEV_USER_HEADER, who)
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

async fn put(app: &Router, who: &str, uri: &str, bytes: &str) -> Reply {
    let builder = Request::builder().method("PUT").uri(uri);
    let builder = if who.starts_with("pyn_") {
        builder.header("authorization", format!("Bearer {who}"))
    } else {
        builder.header(api::DEV_USER_HEADER, who)
    };
    let r = app
        .clone()
        .oneshot(builder.body(Body::from(bytes.to_string())).unwrap())
        .await
        .unwrap();
    Reply {
        status: r.status(),
        body: r.into_body().collect().await.unwrap().to_bytes().to_vec(),
    }
}

async fn token_for(app: &Router, name: &str) -> String {
    let body = json!({"username": name, "password": PASSWORD});
    let r = send(app, "root", "POST", "/v1/register", Some(body.clone())).await;
    assert_eq!(r.status, StatusCode::CREATED);
    let r = send(app, "root", "POST", "/v1/login", Some(body)).await;
    r.json::<api::CreatedToken>().token
}

async fn service(app: &Router, name: &str, scopes: &[&str]) -> String {
    let r = send(
        app,
        "root",
        "POST",
        "/v1/admin/service-credentials",
        Some(json!({"name": name, "scopes": scopes})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED);
    r.json::<api::CreatedServiceCredential>().secret
}

async fn freeze(app: &Router, who: &str, owner: &str, reason: &str) -> Reply {
    send(
        app,
        who,
        "PUT",
        &format!("/v1/admin/owners/{owner}/freeze"),
        Some(json!({"reason": reason})),
    )
    .await
}

async fn unfreeze(app: &Router, who: &str, owner: &str) -> Reply {
    send(
        app,
        who,
        "DELETE",
        &format!("/v1/admin/owners/{owner}/freeze"),
        None,
    )
    .await
}

struct Case {
    operation: &'static str,
    method: &'static str,
    uri: String,
    body: Option<Value>,
}

fn case(operation: &'static str, method: &'static str, uri: String, body: Option<Value>) -> Case {
    Case {
        operation,
        method,
        uri,
        body,
    }
}

/// Every write route that works on `owner`, with a body valid enough to reach the freeze check.
fn write_cases(owner: &str, org: bool, content: &str) -> Vec<Case> {
    let repo = format!("/v1/repos/{owner}/game");
    let mut cases = vec![
        case(
            "create_repo",
            "POST",
            "/v1/repos".into(),
            Some(json!({"owner": owner, "name": "fresh"})),
        ),
        case(
            "update_repo",
            "PATCH",
            repo.clone(),
            Some(json!({"visibility": "public"})),
        ),
        case("delete_repo", "DELETE", repo.clone(), None),
        case(
            "checkout",
            "POST",
            format!("{repo}/checkout"),
            Some(json!({"path": "Content/x.umap"})),
        ),
        case(
            "checkin",
            "POST",
            format!("{repo}/checkin"),
            Some(json!({"path": "Source/a.cpp", "content": content, "message": "m"})),
        ),
        case(
            "changeset",
            "POST",
            format!("{repo}/changesets"),
            Some(
                json!({"entries": [{"path": "Source/b.cpp", "content": content}], "message": "m"}),
            ),
        ),
        case(
            "restore",
            "POST",
            format!("{repo}/restore"),
            Some(
                json!({"path": "Content/x.umap", "revision": 1, "base_revision": 2, "confirm": "Content/x.umap@r2"}),
            ),
        ),
        case(
            "push",
            "POST",
            format!("{repo}/push"),
            Some(json!({"branch": "side", "commits": [], "trees": []})),
        ),
        case(
            "create_branch",
            "POST",
            format!("{repo}/branches"),
            Some(json!({"name": "feat"})),
        ),
        case(
            "rename_branch",
            "PATCH",
            format!("{repo}/branches/old"),
            Some(json!({"name": "older"})),
        ),
        case(
            "delete_branch",
            "DELETE",
            format!("{repo}/branches/old"),
            None,
        ),
        case(
            "merge",
            "POST",
            format!("{repo}/merge"),
            Some(json!({"source": "old", "target": "main"})),
        ),
        case(
            "assemble_object",
            "POST",
            format!("{repo}/objects/assemble"),
            Some(json!({"chunks": []})),
        ),
        case(
            "add_user",
            "POST",
            format!("{repo}/users"),
            Some(json!({"username": "erin", "password": PASSWORD, "role": "reader"})),
        ),
        case(
            "create_invite",
            "POST",
            format!("{repo}/invites"),
            Some(json!({"role": "reader", "hours": 2})),
        ),
        case(
            "revoke_invite",
            "DELETE",
            format!("{repo}/invites/nope"),
            None,
        ),
        case(
            "set_member",
            "PUT",
            format!("{repo}/members/dave"),
            Some(json!({"role": "reader"})),
        ),
        case(
            "set_role",
            "PUT",
            format!("{repo}/roles/reader"),
            Some(json!({"permissions": ["read"]})),
        ),
    ];
    if org {
        let base = format!("/v1/orgs/{owner}");
        cases.extend([
            case(
                "add_member",
                "POST",
                format!("{base}/members"),
                Some(json!({"user": "carol"})),
            ),
            case(
                "cancel_invitation",
                "DELETE",
                format!("{base}/invitations/carol"),
                None,
            ),
            case(
                "accept_invitation",
                "POST",
                format!("/v1/me/invitations/{owner}/accept"),
                None,
            ),
            case(
                "set_member_role",
                "PATCH",
                format!("{base}/members/bob"),
                Some(json!({"role": "owner"})),
            ),
            case(
                "remove_member",
                "DELETE",
                format!("{base}/members/bob"),
                None,
            ),
            case(
                "set_repo_policy",
                "PUT",
                format!("{base}/repo-policy"),
                Some(json!({"member_creation": "both"})),
            ),
            case(
                "set_creation_rule",
                "PUT",
                format!("{base}/repo-policy/rules/allow/role/member"),
                Some(json!({"scope": "both"})),
            ),
            case(
                "remove_creation_rule",
                "DELETE",
                format!("{base}/repo-policy/rules/allow/role/member"),
                None,
            ),
            case(
                "create_team",
                "POST",
                format!("{base}/teams"),
                Some(json!({"slug": "code"})),
            ),
            case(
                "update_team",
                "PATCH",
                format!("{base}/teams/art"),
                Some(json!({"name": "Art"})),
            ),
            case("delete_team", "DELETE", format!("{base}/teams/art"), None),
            case(
                "add_team_member",
                "PUT",
                format!("{base}/teams/art/members/alice"),
                None,
            ),
            case(
                "remove_team_member",
                "DELETE",
                format!("{base}/teams/art/members/bob"),
                None,
            ),
            case(
                "set_repo_team",
                "PUT",
                format!("{repo}/teams/art"),
                Some(json!({"role": "admin"})),
            ),
            case(
                "remove_repo_team",
                "DELETE",
                format!("{repo}/teams/art"),
                None,
            ),
            case("delete_org", "DELETE", base, None),
        ]);
    }
    cases
}

const UPLOADS: [&str; 2] = ["put_object", "put_chunk"];

/// Operations that stay open under a freeze, and why.
const EXEMPT: &[(&str, &str)] = &[
    ("release", "releasing a lock continues"),
    ("force_unlock", "removing a lock continues"),
    ("fetch", "a read"),
    ("preview_merge", "a read"),
    ("preview_policy", "a read"),
    ("missing_objects", "a read"),
    ("create_org", "makes a new owner, not a write under one"),
    ("decline_invitation", "declining only removes an offer"),
    ("admin_create_org", "an operator action"),
    ("admin_delete_org", "an operator action"),
    ("set_owner_limits", "an operator action"),
    (
        "set_repo_storage_cap",
        "refused for the owner's own request, open to an operator (tests/repo_storage_cap.rs)",
    ),
    ("create_service_credential", "an operator action"),
    ("revoke_service_credential", "an operator action"),
    ("grant_admin", "an operator action"),
    ("revoke_admin", "an operator action"),
    ("approve", "an operator action"),
    ("disable", "an operator action"),
    ("enable", "an operator action"),
    ("freeze_owner", "an operator action"),
    ("unfreeze_owner", "an operator action"),
    ("create_webhook", "server-wide, not under an owner"),
    ("update_webhook", "server-wide, not under an owner"),
    ("delete_webhook", "server-wide, not under an owner"),
    ("rotate_webhook_secret", "server-wide, not under an owner"),
    ("retire_webhook_secret", "server-wide, not under an owner"),
    ("redeliver_webhook", "server-wide, not under an owner"),
    ("add_key", "credentials stay manageable"),
    ("delete_key", "credentials stay manageable"),
    ("create_token", "credentials stay manageable"),
    ("revoke_token", "credentials stay manageable"),
    ("unlink_identity", "credentials stay manageable"),
    ("link", "credentials stay manageable"),
    ("change_password", "credentials stay manageable"),
    ("login", "sign-in is unchanged"),
    ("sign_in", "sign-in is unchanged"),
    ("sign_out", "sign-in is unchanged"),
    (
        "register",
        "sign-up; an invitation to a frozen owner is refused in core",
    ),
    ("resend_verification", "sign-up"),
    ("verify_email", "sign-up"),
    ("complete_setup", "first-run setup"),
];

#[tokio::test]
async fn every_write_operation_is_either_refused_under_a_freeze_or_listed_as_exempt() {
    let app = server().await;
    let doc: Value =
        serde_json::from_slice(&send(&app, "root", "GET", "/openapi.json", None).await.body)
            .unwrap();
    let mut writes = BTreeSet::new();
    for (_, item) in doc["paths"].as_object().unwrap() {
        for (method, op) in item.as_object().unwrap() {
            if method != "get" {
                writes.insert(op["operationId"].as_str().unwrap().to_string());
            }
        }
    }
    let mut covered: BTreeSet<String> = UPLOADS.iter().map(|s| s.to_string()).collect();
    covered.extend(
        write_cases("acme", true, "c")
            .into_iter()
            .chain(write_cases("alice", false, "c"))
            .map(|c| c.operation.to_string()),
    );
    covered.extend(EXEMPT.iter().map(|(op, _)| op.to_string()));
    let unclassified: Vec<_> = writes.difference(&covered).collect();
    assert!(
        unclassified.is_empty(),
        "a write operation is neither tested under a freeze nor listed as exempt: {unclassified:?}"
    );
    let stale: Vec<_> = covered.difference(&writes).collect();
    assert!(stale.is_empty(), "no such operation: {stale:?}");
}

struct Fixture {
    app: Router,
    alice: String,
    bob: String,
    content: String,
}

async fn fixture() -> Fixture {
    let app = server().await;
    let alice = token_for(&app, "alice").await;
    let bob = token_for(&app, "bob").await;
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
    let r = send(
        &app,
        &alice,
        "POST",
        "/v1/orgs/acme/teams",
        Some(json!({"slug": "art"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED);
    send(
        &app,
        &alice,
        "PUT",
        "/v1/orgs/acme/teams/art/members/bob",
        None,
    )
    .await;
    let mut content = String::new();
    for owner in ["alice", "acme"] {
        let r = send(
            &app,
            &alice,
            "POST",
            "/v1/repos",
            Some(json!({"owner": owner, "name": "game"})),
        )
        .await;
        assert_eq!(r.status, StatusCode::CREATED, "{owner}");
        let r = put(
            &app,
            &alice,
            &format!("/v1/repos/{owner}/game/objects"),
            "before",
        )
        .await;
        content = r.json::<api::PutObjectResponse>().content;
        let r = send(
            &app,
            &alice,
            "POST",
            &format!("/v1/repos/{owner}/game/checkin"),
            Some(json!({"path": "Source/a.cpp", "content": content, "message": "first"})),
        )
        .await;
        assert_eq!(r.status, StatusCode::OK, "{owner}: {}", r.code());
        let r = send(
            &app,
            &alice,
            "POST",
            &format!("/v1/repos/{owner}/game/branches"),
            Some(json!({"name": "old"})),
        )
        .await;
        assert!(r.status.is_success(), "{owner}: {}", r.code());
    }
    let r = send(
        &app,
        &alice,
        "PUT",
        "/v1/repos/acme/game/teams/art",
        Some(json!({"role": "writer"})),
    )
    .await;
    assert!(r.status.is_success(), "{}", r.code());
    Fixture {
        app,
        alice,
        bob,
        content,
    }
}

async fn repo_state(app: &Router, who: &str, owner: &str) -> Vec<Value> {
    let mut seen = Vec::new();
    for uri in [
        format!("/v1/repos/{owner}/game"),
        format!("/v1/repos/{owner}/game/branches"),
        format!("/v1/repos/{owner}/game/locks"),
        format!("/v1/repos/{owner}/game/members"),
        format!("/v1/repos/{owner}/game/invites"),
        format!("/v1/repos/{owner}/game/roles"),
        format!("/v1/repos/{owner}/game/audit"),
        format!("/v1/repos?owner={owner}"),
    ] {
        let r = send(app, who, "GET", &uri, None).await;
        assert_eq!(r.status, StatusCode::OK, "{uri}: {}", r.code());
        seen.push(serde_json::from_slice(&r.body).unwrap());
    }
    if owner == "acme" {
        for uri in [
            "/v1/orgs/acme/members",
            "/v1/orgs/acme/teams",
            "/v1/orgs/acme/repo-policy",
            "/v1/orgs/acme/audit",
        ] {
            let r = send(app, who, "GET", uri, None).await;
            assert_eq!(r.status, StatusCode::OK, "{uri}: {}", r.code());
            seen.push(serde_json::from_slice(&r.body).unwrap());
        }
    }
    seen
}

#[tokio::test]
async fn every_write_route_is_refused_for_a_frozen_user_and_organization_and_works_again_after() {
    let f = fixture().await;
    let root = "root";
    let billing = service(&f.app, "billing", &["manage_freeze"]).await;

    for (owner, org) in [("alice", false), ("acme", true)] {
        let r = if owner == "alice" {
            freeze(&f.app, &billing, owner, "plan lapsed").await
        } else {
            freeze(&f.app, root, owner, "abuse report").await
        };
        assert_eq!(r.status, StatusCode::OK, "{owner}");
        let frozen_before = repo_state(&f.app, &f.alice, owner).await;

        for actor in [f.alice.as_str(), "alice"] {
            for c in write_cases(owner, org, &f.content) {
                let r = send(&f.app, actor, c.method, &c.uri, c.body.clone()).await;
                assert!(
                    r.is(StatusCode::FORBIDDEN, "owner_frozen"),
                    "{owner} {} as {} {} {}: {} {}",
                    c.operation,
                    if actor == "alice" { "session" } else { "token" },
                    c.method,
                    c.uri,
                    r.status,
                    r.code()
                );
            }
            for route in UPLOADS {
                let uri = match route {
                    "put_object" => format!("/v1/repos/{owner}/game/objects"),
                    _ => format!("/v1/repos/{owner}/game/objects/chunk"),
                };
                let r = put(&f.app, actor, &uri, "more").await;
                assert!(
                    r.is(StatusCode::FORBIDDEN, "owner_frozen"),
                    "{owner} {route}"
                );
            }
        }
        if org {
            // A member with write access is refused too, not only the owner.
            let r = send(
                &f.app,
                &f.bob,
                "POST",
                "/v1/repos/acme/game/checkout",
                Some(json!({"path": "Content/x.umap"})),
            )
            .await;
            assert!(r.is(StatusCode::FORBIDDEN, "owner_frozen"), "{}", r.code());
        }
        assert_eq!(
            repo_state(&f.app, &f.alice, owner).await,
            frozen_before,
            "{owner}: a refused write leaves nothing behind"
        );

        let r = unfreeze(&f.app, root, owner).await;
        assert_eq!(r.status, StatusCode::OK);
        for c in write_cases(owner, org, &f.content) {
            let r = send(&f.app, &f.alice, c.method, &c.uri, c.body.clone()).await;
            assert_ne!(r.code(), "owner_frozen", "{owner} {}", c.operation);
        }
    }
}

#[tokio::test]
async fn reads_sign_in_and_releasing_a_lock_continue_under_a_freeze() {
    let f = fixture().await;
    let r = send(
        &f.app,
        &f.alice,
        "POST",
        "/v1/repos/alice/game/checkout",
        Some(json!({"path": "Content/held.umap"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.code());
    let held = f.alice.as_str();
    assert_eq!(
        freeze(&f.app, "root", "alice", "plan lapsed").await.status,
        StatusCode::OK
    );

    for uri in [
        "/v1/repos/alice/game",
        "/v1/repos/alice/game/files",
        "/v1/repos/alice/game/tree",
        "/v1/repos/alice/game/summary",
        "/v1/repos/alice/game/history?path=Source/a.cpp",
        "/v1/repos/alice/game/commits",
        "/v1/repos/alice/game/snapshot",
        "/v1/repos/alice/game/locks",
        "/v1/repos/alice/game/branches",
        "/v1/repos/alice/game/verify",
        "/v1/repos/alice/game/usage",
        "/v1/me",
        "/v1/me/locks",
        "/v1/owners/alice/limits",
        "/v1/owners/alice/usage",
        "/v1/owners/alice/freeze",
    ] {
        let r = send(&f.app, held, "GET", uri, None).await;
        assert_eq!(r.status, StatusCode::OK, "{uri}: {}", r.code());
    }
    let r = send(
        &f.app,
        held,
        "POST",
        "/v1/repos/alice/game/fetch",
        Some(json!({"wants": [], "haves": []})),
    )
    .await;
    assert_ne!(r.code(), "owner_frozen");
    let r = send(
        &f.app,
        held,
        "POST",
        "/v1/repos/alice/game/objects/missing",
        Some(json!({"hashes": []})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);

    let login = send(
        &f.app,
        "root",
        "POST",
        "/v1/login",
        Some(json!({"username": "alice", "password": PASSWORD})),
    )
    .await;
    assert_eq!(login.status, StatusCode::OK);
    let r = send(
        &f.app,
        held,
        "POST",
        "/v1/repos/alice/game/release",
        Some(json!({"path": "Content/held.umap"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.code());
    let r = send(&f.app, held, "GET", "/v1/repos/alice/game/locks", None).await;
    assert_eq!(r.json::<Vec<api::Lock>>().len(), 0);
    let r = send(
        &f.app,
        held,
        "POST",
        "/v1/tokens",
        Some(json!({"name": "ci", "repos": ["alice/game"], "permissions": ["read"]})),
    )
    .await;
    assert_ne!(r.code(), "owner_frozen");
    let r = send(
        &f.app,
        held,
        "POST",
        "/v1/orgs",
        Some(json!({"name": "mine"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.code());
}

#[tokio::test]
async fn the_state_and_the_administrative_routes_enforce_who_may_set_and_see_it() {
    let f = fixture().await;
    let outsider = token_for(&f.app, "mallory").await;
    let accounts = service(&f.app, "accounts", &["manage_accounts"]).await;
    let billing = service(&f.app, "billing", &["manage_freeze"]).await;

    let r = freeze(&f.app, &f.alice, "acme", "x").await;
    assert!(r.is(StatusCode::FORBIDDEN, "server_admin_required"));
    let r = freeze(&f.app, &accounts, "acme", "x").await;
    assert!(r.is(StatusCode::FORBIDDEN, "service_scope_required"));
    let r = unfreeze(&f.app, &accounts, "acme").await;
    assert!(r.is(StatusCode::FORBIDDEN, "service_scope_required"));
    let r = freeze(&f.app, "root", "acme", "   ").await;
    assert!(r.is(StatusCode::BAD_REQUEST, "invalid_request"));
    let r = freeze(&f.app, "root", "nobody", "x").await;
    assert!(r.is(StatusCode::NOT_FOUND, "user_not_found"));

    let r = send(&f.app, &f.bob, "GET", "/v1/owners/acme/freeze", None).await;
    let calm = r.json::<api::OwnerFreeze>();
    assert!(!calm.frozen && calm.frozen_at.is_none() && calm.reason.is_none());
    assert_eq!(calm.kind, api::OwnerKind::Org);

    let r = freeze(&f.app, &billing, "acme", "plan lapsed").await;
    assert_eq!(r.status, StatusCode::OK);
    let set = r.json::<api::OwnerFreeze>();
    assert!(set.frozen && set.frozen_at.is_some());
    assert_eq!(set.reason.as_deref(), Some("plan lapsed"));
    for viewer in [&f.alice, &f.bob, &billing] {
        let r = send(&f.app, viewer, "GET", "/v1/owners/acme/freeze", None).await;
        assert_eq!(r.json::<api::OwnerFreeze>(), set);
    }
    let r = send(&f.app, &outsider, "GET", "/v1/owners/acme/freeze", None).await;
    assert!(r.is(StatusCode::FORBIDDEN, "not_namespace_owner"));
    let r = send(&f.app, &accounts, "GET", "/v1/owners/acme/freeze", None).await;
    assert!(r.is(StatusCode::FORBIDDEN, "service_scope_required"));
    let r = send(&f.app, &f.bob, "GET", "/v1/owners/alice/freeze", None).await;
    assert!(r.is(StatusCode::FORBIDDEN, "not_namespace_owner"));

    let r = freeze(&f.app, "root", "acme", "abuse report").await;
    assert_eq!(
        r.json::<api::OwnerFreeze>().reason.as_deref(),
        Some("abuse report")
    );
    let r = unfreeze(&f.app, &billing, "acme").await;
    assert!(!r.json::<api::OwnerFreeze>().frozen);
    let r = unfreeze(&f.app, &billing, "acme").await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(!r.json::<api::OwnerFreeze>().frozen);

    let r = send(&f.app, "root", "GET", "/v1/admin/audit", None).await;
    let actions: Vec<_> = r
        .json::<api::AuditPage>()
        .entries
        .into_iter()
        .filter(|e| e.action.starts_with("owner_"))
        .map(|e| (e.action, e.detail))
        .collect();
    assert_eq!(
        actions,
        [
            ("owner_unfrozen".to_string(), "acme unfrozen".to_string()),
            (
                "owner_frozen".to_string(),
                "acme frozen: abuse report".to_string()
            ),
            (
                "owner_frozen".to_string(),
                "acme frozen: plan lapsed".to_string()
            ),
        ]
    );
    let r = send(&f.app, &f.alice, "GET", "/v1/orgs/acme/audit", None).await;
    let org_actors: Vec<_> = r
        .json::<api::AuditPage>()
        .entries
        .into_iter()
        .filter(|e| e.action.starts_with("owner_"))
        .map(|e| e.actor)
        .collect();
    assert_eq!(org_actors, ["@service:billing", "root", "@service:billing"]);
}
