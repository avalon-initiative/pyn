//! What an administrator does in an access session names the administrator beside the account: in every audit event
//! and every lock webhook event it produces, inside the hash chain, and not at all for an ordinary request.

#[path = "common/cred.rs"]
mod cred;
use cred::{Cred, WithCred};
use std::collections::BTreeSet;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{TimeZone, Utc};
use http_body_util::BodyExt;
use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore, MemoryWebhookStore,
};
use pyn_core::{
    AccessConfig, AccessService, AccessStore, AuditAction, AuditEvent, AuditQuery, AuditScope,
    AuditStore, AuthProvider, Credential, Identity, ManualClock, RegistrationMode, Repositories,
    Rules, UserId, Visibility, WebhookSettings,
};
use pyn_proto as api;
use pyn_server::auth::{BearerAuth, DevHeaderAuth};
use pyn_server::events::WebhookLimitEvents;
use pyn_server::{AppState, router};
use serde_json::{Value, json};
use tower::ServiceExt;

struct World {
    app: Router,
    audit: Arc<MemoryAuditStore>,
}

async fn world() -> World {
    let clock = Arc::new(ManualClock::new(
        Utc.with_ymd_and_hms(2030, 1, 1, 12, 0, 0).unwrap(),
    ));
    let hooks = Arc::new(MemoryWebhookStore::new());
    let meta = Arc::new(MemoryMetadataStore::new().with_outbox(hooks.clone()));
    let store = Arc::new(MemoryAccessStore::new().with_outbox(hooks.clone()));
    let audit = Arc::new(MemoryAuditStore::new());
    let objects = Arc::new(MemoryObjectStore::new());
    let access = Arc::new(
        AccessService::new(
            store.clone(),
            clock.clone(),
            Arc::new(WebhookLimitEvents::new(
                hooks.clone(),
                "https://pyn.example".into(),
            )),
        )
        .with_config(AccessConfig {
            registration: RegistrationMode::Open,
            require_email_verification: false,
            ..AccessConfig::default()
        })
        .with_registry(meta.clone())
        .with_audit(audit.clone())
        .with_webhooks(hooks, WebhookSettings::default()),
    );
    access.admin_for_tests(&UserId::new("root")).await.unwrap();
    store
        .ensure_user(
            &UserId::new("alice"),
            Utc.with_ymd_and_hms(2030, 1, 1, 12, 0, 0).unwrap(),
        )
        .await
        .unwrap();
    let repos = Arc::new(Repositories::new(
        meta,
        objects.clone(),
        audit.clone(),
        access.clone(),
        clock,
        Rules::empty(),
    ));
    let alice = Identity {
        user: UserId::new("alice"),
        credential: Credential::Session,
    };
    repos
        .create(&alice, &alice.user, "game", Some(Visibility::Private), None)
        .await
        .unwrap();
    let app = router(AppState {
        repos,
        objects,
        access: access.clone(),
        auth: Arc::new(BearerAuth {
            access: access.clone(),
        }),
        dev_auth: Some(Arc::new(DevHeaderAuth) as Arc<dyn AuthProvider>),
        trust_forwarded: false,
        trusted_proxies: 1,
        updates: None,
        storage: Default::default(),
        certificate: Default::default(),
        ready: Default::default(),
    });
    World { app, audit }
}

struct Reply {
    status: StatusCode,
    body: Vec<u8>,
}

impl Reply {
    fn ok(self, what: &str) -> Value {
        assert!(
            self.status.is_success(),
            "{what}: {} {}",
            self.status,
            String::from_utf8_lossy(&self.body)
        );
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

async fn send(w: &World, cred: &Cred, method: &str, uri: &str, body: Option<Value>) -> Reply {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .cred(cred)
        .body(Body::from(body.map(|b| b.to_string()).unwrap_or_default()))
        .unwrap();
    let r = w.app.clone().oneshot(req).await.unwrap();
    Reply {
        status: r.status(),
        body: r.into_body().collect().await.unwrap().to_bytes().to_vec(),
    }
}

async fn upload(w: &World, cred: &Cred, content: &[u8]) -> String {
    let req = Request::builder()
        .method("PUT")
        .uri("/v1/repos/alice/game/objects")
        .header("content-length", content.len())
        .cred(cred)
        .body(Body::from(content.to_vec()))
        .unwrap();
    let r = w.app.clone().oneshot(req).await.unwrap();
    let body = r.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice::<api::PutObjectResponse>(&body)
        .unwrap()
        .content
}

async fn repo_log(w: &World) -> Vec<AuditEvent> {
    let info = send(w, &Cred::dev("alice"), "GET", "/v1/repos/alice/game", None)
        .await
        .ok("repo");
    let scope = AuditScope::Repo(pyn_core::RepoId::new(info["id"].as_str().unwrap()));
    let mut events = w
        .audit
        .list(
            &scope,
            &AuditQuery {
                limit: 500,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    events.reverse();
    pyn_core::integrity::check_events(&scope, events).unwrap()
}

/// Every audited write a repository route can make, as a caller sends them: a table of `(method, uri, body)` built
/// from what the earlier steps returned.
async fn write_everything(w: &World, who: &Cred) {
    let repo = "/v1/repos/alice/game";
    let (a, b, policy) = ("Content/a.umap", "Source/b.cpp", ".pyn/pyn.toml");
    let v1 = upload(w, who, b"v1").await;
    let v2 = upload(w, who, b"v2").await;
    let shared = upload(
        w,
        who,
        b"[meta]\ndefault = \"shared\"\n[exclusive]\npaths = [\"Content/\"]\n",
    )
    .await;

    let steps: Vec<(&str, &str, Value)> = vec![
        ("checkout", "/checkout", json!({"path": a})),
        ("release", "/release", json!({"path": a})),
        ("checkout again", "/checkout", json!({"path": a})),
        (
            "checkin",
            "/checkin",
            json!({"path": a, "content": v1, "base_revision": null, "message": "first"}),
        ),
        (
            "checkout at r1",
            "/checkout",
            json!({"path": a, "base_revision": 1}),
        ),
        (
            "second check-in",
            "/checkin",
            json!({"path": a, "content": v2, "base_revision": 1, "message": "second"}),
        ),
        (
            "checkout at r2",
            "/checkout",
            json!({"path": a, "base_revision": 2}),
        ),
        (
            "force unlock",
            "/force-unlock",
            json!({"path": a, "reason": "stuck"}),
        ),
        (
            "checkout for the restore",
            "/checkout",
            json!({"path": a, "base_revision": 2}),
        ),
        (
            "restore",
            "/restore",
            json!({"path": a, "revision": 1, "base_revision": 2, "confirm": format!("{a}@r2")}),
        ),
        (
            "checkout for the delete",
            "/checkout",
            json!({"path": a, "base_revision": 3}),
        ),
        ("checkout the new path", "/checkout", json!({"path": b})),
        (
            "changeset with a deletion",
            "/changesets",
            json!({
                "entries": [{"path": b, "content": v1}],
                "deletions": [{"path": a, "base_revision": 3}],
                "message": "move",
            }),
        ),
        (
            "policy check-in that converts a path",
            "/checkin",
            json!({"path": policy, "content": shared, "base_revision": null, "message": "policy"}),
        ),
        (
            "LFS lock",
            "/info/lfs/locks",
            json!({"path": "Content/b.umap"}),
        ),
        ("branch", "/branches", json!({"name": "feat"})),
        (
            "check-in on the branch",
            "/checkin",
            json!({"path": "Source/c.cpp", "content": v1, "base_revision": null, "message": "c", "branch": "feat"}),
        ),
        (
            "merge",
            "/merge",
            json!({"source": "feat", "target": "main"}),
        ),
    ];
    for (what, uri, body) in steps {
        send(w, who, "POST", &format!("{repo}{uri}"), Some(body))
            .await
            .ok(what);
    }
    let lfs_id = lfs_lock_id(w, who).await;
    send(
        w,
        who,
        "POST",
        &format!("{repo}/info/lfs/locks/{lfs_id}/unlock"),
        Some(json!({"force": false})),
    )
    .await
    .ok("LFS unlock");
    send(
        w,
        who,
        "PATCH",
        &format!("{repo}/branches/feat"),
        Some(json!({"name": "feat2"})),
    )
    .await
    .ok("rename branch");
    send(w, who, "DELETE", &format!("{repo}/branches/feat2"), None)
        .await
        .ok("delete branch");
}

async fn lfs_lock_id(w: &World, who: &Cred) -> String {
    let r = send(w, who, "GET", "/v1/repos/alice/game/info/lfs/locks", None)
        .await
        .ok("list LFS locks");
    r["locks"][0]["id"].as_str().expect("a lock").to_string()
}

async fn start_session(w: &World) -> Cred {
    let r = send(
        w,
        &Cred::dev("root"),
        "POST",
        "/v1/admin/users/alice/access-sessions",
        Some(json!({"reason": "abuse_report"})),
    )
    .await
    .ok("start");
    Cred::access_session(r["secret"].as_str().unwrap())
}

async fn lock_events(w: &World) -> Vec<api::Event> {
    let r = send(
        w,
        &Cred::dev("alice"),
        "GET",
        "/v1/repos/alice/game/locks/events?after=0",
        None,
    )
    .await;
    let page = serde_json::from_slice::<api::LockStreamPage>(&r.body).expect("a lock stream page");
    page.events.into_iter().map(|e| e.event).collect()
}

/// Audit actions a repository route reaches under a session, each with the table step that produces it.
const REACHABLE: &[AuditAction] = &[
    AuditAction::Checkout,
    AuditAction::Release,
    AuditAction::Checkin,
    AuditAction::Restore,
    AuditAction::ForceUnlock,
    AuditAction::PolicyChanged,
    AuditAction::ModeConverted,
    AuditAction::BranchCreated,
    AuditAction::BranchMerged,
    AuditAction::BranchRenamed,
    AuditAction::BranchDeleted,
    AuditAction::Delete,
];

/// Actions no access session can produce, because the routes that write them are refused to it (`access_session_not_allowed`,
/// `docs/concepts/access.md#what-a-session-reaches`), are not caused by a request, or record the session itself.
const UNREACHABLE: &[AuditAction] = &[
    AuditAction::MemberAdded,
    AuditAction::RoleChanged,
    AuditAction::RolePermissionsChanged,
    AuditAction::TokenCreated,
    AuditAction::TokenRevoked,
    AuditAction::RepoCreated,
    AuditAction::RepoUpdated,
    AuditAction::RepoDeleted,
    AuditAction::AccountApproved,
    AuditAction::AccountDisabled,
    AuditAction::AccountEnabled,
    AuditAction::AccountDeletionScheduled,
    AuditAction::AccountDeletionRestored,
    AuditAction::AccountDeleted,
    AuditAction::OrgCreated,
    AuditAction::OrgDeleted,
    AuditAction::OrgMemberAdded,
    AuditAction::OrgMemberRemoved,
    AuditAction::OrgMemberRoleChanged,
    AuditAction::OrgProfileUpdated,
    AuditAction::OrgAvatarSet,
    AuditAction::OrgAvatarRemoved,
    AuditAction::OrgMemberInvited,
    AuditAction::OrgInvitationAccepted,
    AuditAction::OrgInvitationDeclined,
    AuditAction::OrgInvitationCancelled,
    AuditAction::OrgInvitationExpired,
    AuditAction::TeamCreated,
    AuditAction::TeamUpdated,
    AuditAction::TeamDeleted,
    AuditAction::TeamMemberAdded,
    AuditAction::TeamMemberRemoved,
    AuditAction::TeamAccessSet,
    AuditAction::TeamAccessRemoved,
    AuditAction::RepoCreationPolicyChanged,
    AuditAction::RepoCreationRuleSet,
    AuditAction::RepoCreationRuleRemoved,
    AuditAction::OrgSettingChanged,
    AuditAction::ServiceCredentialCreated,
    AuditAction::ServiceCredentialRevoked,
    AuditAction::ServiceCredentialRefused,
    AuditAction::ServerSetupCompleted,
    AuditAction::ServerAccessModeChanged,
    AuditAction::AdminGranted,
    AuditAction::AdminRevoked,
    AuditAction::AccountRenamed,
    AuditAction::EmailAdded,
    AuditAction::EmailVerified,
    AuditAction::EmailPrimaryChanged,
    AuditAction::EmailRemoved,
    AuditAction::TwoFactorAdded,
    AuditAction::TwoFactorRemoved,
    AuditAction::RecoveryCodesRegenerated,
    AuditAction::RecoveryCodeUsed,
    AuditAction::TwoFactorReset,
    AuditAction::PasskeyAdded,
    AuditAction::PasskeyRemoved,
    AuditAction::PasskeyCloneSuspected,
    AuditAction::ServerTwoFactorRequirementChanged,
    AuditAction::OrgTwoFactorRequirementChanged,
    AuditAction::OwnerLimitsChanged,
    AuditAction::RepoStorageCapChanged,
    AuditAction::OwnerFrozen,
    AuditAction::OwnerUnfrozen,
    AuditAction::ExternalAccountCreated,
    AuditAction::ExternalIdentityLinked,
    AuditAction::ExternalIdentityUnlinked,
    AuditAction::InviteCreated,
    AuditAction::InviteRevoked,
    AuditAction::VersionStranded,
    AuditAction::WebhookCreated,
    AuditAction::WebhookUpdated,
    AuditAction::WebhookDeleted,
    AuditAction::WebhookSecretRotated,
    AuditAction::WebhookSecretRetired,
    AuditAction::WebhookRedelivered,
    AuditAction::AccessSessionStarted,
    AuditAction::AccessSessionReached,
    AuditAction::AccessSessionEnded,
];

#[test]
fn every_audit_action_is_classified_as_reachable_by_a_session_or_not() {
    let reachable: BTreeSet<_> = REACHABLE.iter().map(|a| a.as_str()).collect();
    let unreachable: BTreeSet<_> = UNREACHABLE.iter().map(|a| a.as_str()).collect();
    assert!(reachable.is_disjoint(&unreachable));
    let all: BTreeSet<_> = AuditAction::ALL.iter().map(|a| a.as_str()).collect();
    let classified: BTreeSet<_> = reachable.union(&unreachable).copied().collect();
    assert_eq!(
        all, classified,
        "a new audit action needs a place in REACHABLE (and a step in write_everything) or in UNREACHABLE"
    );
}

#[tokio::test]
async fn every_audited_write_a_session_makes_names_the_administrator() {
    let w = world().await;
    let session = start_session(&w).await;
    let before = repo_log(&w).await.len();
    write_everything(&w, &session).await;

    let events = repo_log(&w).await.split_off(before);
    let produced: BTreeSet<_> = events.iter().map(|e| e.action.as_str()).collect();
    for action in REACHABLE {
        assert!(
            produced.contains(action.as_str()),
            "the table never produced {action}; produced {produced:?}"
        );
    }
    for e in &events {
        assert!(
            REACHABLE.contains(&e.action),
            "{} is not classified reachable",
            e.action
        );
        assert_eq!(
            e.actor.as_str(),
            "alice",
            "{}: the account is the actor",
            e.action
        );
        assert_eq!(
            e.acting_admin.as_ref().map(UserId::as_str),
            Some("root"),
            "{} lost the administrator",
            e.action
        );
    }

    let data: Vec<_> = lock_events(&w).await.into_iter().map(|e| e.data).collect();
    let mut seen = BTreeSet::new();
    for d in &data {
        let (kind, admin) = match d {
            api::EventData::LockAcquired(d) => ("acquired", d.acting_admin.as_deref()),
            api::EventData::LockReleased(d) => ("released", d.acting_admin.as_deref()),
            api::EventData::LockForced(d) => ("forced", d.acting_admin.as_deref()),
            other => panic!("unexpected {other:?}"),
        };
        assert_eq!(admin, Some("root"), "lock.{kind}");
        seen.insert(kind);
    }
    assert_eq!(seen, BTreeSet::from(["acquired", "released", "forced"]));
}

#[tokio::test]
async fn the_audit_view_shows_the_administrator_beside_the_account() {
    let w = world().await;
    let session = start_session(&w).await;
    send(
        &w,
        &session,
        "POST",
        "/v1/repos/alice/game/checkout",
        Some(json!({"path": "Content/a.umap"})),
    )
    .await
    .ok("checkout");
    let page = send(
        &w,
        &Cred::dev("alice"),
        "GET",
        "/v1/repos/alice/game/audit",
        None,
    )
    .await
    .ok("audit");
    let entry = &page["entries"][0];
    assert_eq!(entry["action"], "checkout");
    assert_eq!(entry["actor"], "alice");
    assert_eq!(entry["acting_admin"], "root");
}

#[tokio::test]
async fn an_ordinary_request_records_no_administrator_and_hashes_as_before() {
    let w = world().await;
    let alice = Cred::dev("alice");
    write_everything(&w, &alice).await;

    let events = repo_log(&w).await;
    assert!(events.len() > 10);
    for e in &events {
        assert_eq!(e.acting_admin, None, "{}", e.action);
        assert!(!e.hash.is_empty());
    }
    let page = send(
        &w,
        &alice,
        "GET",
        "/v1/repos/alice/game/audit?limit=100",
        None,
    )
    .await
    .ok("audit");
    for entry in page["entries"].as_array().unwrap() {
        assert!(
            entry.get("acting_admin").is_none(),
            "the field is absent, not null: {entry}"
        );
    }
    for e in lock_events(&w).await {
        let json = serde_json::to_value(&e).unwrap();
        assert!(json["data"].get("acting_admin").is_none(), "{json}");
    }
}

/// The lock calls also accept a bare `&UserId` (tests, the memory fixtures); a handler passing `.user` would record
/// the account alone, so none may.
#[test]
fn handlers_pass_the_caller_not_its_user_name_to_the_lock_calls() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut offenders = Vec::new();
    for entry in std::fs::read_dir(&src).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        for call in [
            ".checkout_on(",
            ".checkout(",
            ".release(",
            ".force_unlock(",
            ".lock_path(",
        ] {
            for (at, _) in text.match_indices(call) {
                let args = text[at..].lines().take(6).collect::<Vec<_>>().join(" ");
                let end = args.find(".await").unwrap_or(args.len());
                if args[..end].contains(".user") {
                    offenders.push(format!("{}: {}", path.display(), &args[..end]));
                }
            }
        }
    }
    assert!(offenders.is_empty(), "{offenders:#?}");
}
