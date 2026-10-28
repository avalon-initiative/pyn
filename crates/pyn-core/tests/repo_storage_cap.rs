//! A repository's own storage cap and usage breakdown: who may set and read them, the error at the cap, and the
//! rules that already hold for owner limits (nothing deleted, frozen owners, audit).

use std::collections::BTreeSet;
use std::sync::Arc;

use chrono::{TimeZone, Utc};
use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore,
};
use pyn_core::{
    AccessConfig, AccessService, AuditAction, AuditQuery, AuditScope, AuditStore, Credential,
    Identity, Limits, ManualClock, OrgRole, PynError, Registration, RegistrationMode, RepoPath,
    Repositories, Role, Rules, ServiceScope, UserId,
};

struct World {
    repos: Repositories,
    access: Arc<AccessService>,
    audit: Arc<MemoryAuditStore>,
}

fn world_with(default_limits: Limits) -> World {
    let clock = Arc::new(ManualClock::new(
        Utc.with_ymd_and_hms(2026, 10, 8, 9, 0, 0).unwrap(),
    ));
    let audit = Arc::new(MemoryAuditStore::new());
    let meta = Arc::new(MemoryMetadataStore::new());
    let objects = Arc::new(MemoryObjectStore::new());
    let access = Arc::new(
        AccessService::new(
            Arc::new(MemoryAccessStore::new()),
            clock.clone(),
            Arc::new(pyn_core::MemoryLimitEvents::new()),
        )
        .with_config(AccessConfig {
            default_limits,
            registration: RegistrationMode::Open,
            require_email_verification: false,
            ..AccessConfig::default()
        })
        .with_registry(meta.clone())
        .with_audit(audit.clone()),
    );
    let repos = Repositories::new(
        meta,
        objects.clone(),
        audit.clone(),
        access.clone(),
        clock,
        Rules::from_toml("[meta]\ndefault = \"shared\"\n").unwrap(),
    );
    World {
        repos,
        access,
        audit,
    }
}

fn world() -> World {
    world_with(Limits::default())
}

async fn signup(w: &World, name: &str) {
    w.access
        .register(Registration::new(name, "correct horse battery"))
        .await
        .unwrap();
}

fn user(name: &str) -> UserId {
    UserId::new(name)
}

fn session(name: &str) -> Identity {
    Identity {
        user: user(name),
        credential: Credential::Session,
    }
}

async fn admin(w: &World) -> Identity {
    w.access.admin_for_tests(&user("root")).await.unwrap();
    session("root")
}

async fn service(w: &World, scopes: &[ServiceScope]) -> Identity {
    let root = admin(w).await;
    let (_, secret) = w
        .access
        .create_service_credential(
            &root,
            "billing",
            scopes.iter().copied().collect::<BTreeSet<_>>(),
        )
        .await
        .unwrap();
    w.access.identify(&secret).await.unwrap()
}

async fn create(w: &World, who: &str, name: &str) -> pyn_core::RepoRecord {
    w.repos
        .create(&session(who), &user(who), name, None, None)
        .await
        .unwrap()
}

async fn checkin(w: &World, who: &str, repo: &str, path: &str, body: &str) -> Result<(), PynError> {
    let open = w.repos.open(who, repo).await.unwrap();
    let principal = w
        .access
        .principal_in(&open.record.id, &session(who))
        .await
        .unwrap();
    let content = open
        .service
        .upload_blob(body.as_bytes().to_vec())
        .await
        .unwrap();
    open.service
        .checkin(
            &principal,
            &RepoPath::new(path).unwrap(),
            content,
            None,
            "m".into(),
        )
        .await
        .map(|_| ())
}

async fn set(
    w: &World,
    who: &Identity,
    owner: &str,
    name: &str,
    cap: Option<u64>,
) -> Result<(), PynError> {
    w.repos
        .set_storage_cap(who, &user(owner), name, cap)
        .await
        .map(|_| ())
}

#[tokio::test]
async fn a_repository_cap_stops_growth_in_that_repository_only() {
    let w = world();
    signup(&w, "alice").await;
    create(&w, "alice", "one").await;
    create(&w, "alice", "two").await;
    checkin(&w, "alice", "one", "a", "123456").await.unwrap();
    set(&w, &session("alice"), "alice", "one", Some(8))
        .await
        .unwrap();

    let err = checkin(&w, "alice", "one", "b", "abcdef")
        .await
        .unwrap_err();
    assert!(
        matches!(&err, PynError::RepoStorageLimitReached { repository, limit: 8, used: 6 }
            if repository == "alice/one"),
        "{err}"
    );
    assert!(err.to_string().contains("alice/one"), "{err}");
    checkin(&w, "alice", "one", "c", "123456").await.unwrap();
    checkin(&w, "alice", "two", "b", "abcdefghijkl")
        .await
        .unwrap();

    // Lowering the cap below use deletes nothing and refuses only growth.
    set(&w, &session("alice"), "alice", "one", Some(1))
        .await
        .unwrap();
    let alice = session("alice");
    let report = w.repos.owner_report(&alice, &user("alice")).await.unwrap();
    let rows: Vec<_> = report
        .repositories
        .iter()
        .map(|(r, u)| (r.address(), u.stored_bytes, r.storage_cap))
        .collect();
    assert_eq!(
        rows,
        [
            ("alice/two".into(), 12, None),
            ("alice/one".into(), 6, Some(1))
        ],
        "largest first, with the cap"
    );
    assert!(checkin(&w, "alice", "one", "d", "zz").await.is_err());

    set(&w, &session("alice"), "alice", "one", None)
        .await
        .unwrap();
    checkin(&w, "alice", "one", "d", "zz").await.unwrap();
}

#[tokio::test]
async fn the_owner_an_administrator_and_a_manage_limits_credential_set_the_cap() {
    let w = world();
    for name in ["alice", "bob", "carol"] {
        signup(&w, name).await;
    }
    create(&w, "alice", "game").await;
    w.access
        .create_org(&session("alice"), "acme")
        .await
        .unwrap();
    w.access
        .add_org_member(
            &session("alice"),
            &user("acme"),
            &user("bob"),
            OrgRole::Member,
        )
        .await
        .unwrap();
    create(&w, "acme", "tools").await;

    let denied = |e: PynError| matches!(e, PynError::NotNamespaceOwner(_));
    assert!(denied(
        set(&w, &session("bob"), "alice", "game", Some(1))
            .await
            .unwrap_err()
    ));
    assert!(denied(
        set(&w, &session("carol"), "alice", "nothing", Some(1))
            .await
            .unwrap_err()
    ));
    assert!(matches!(
        set(&w, &session("bob"), "acme", "tools", Some(1))
            .await
            .unwrap_err(),
        PynError::NotOrgOwner(_)
    ));
    let billing = service(&w, &[ServiceScope::ManageLimits]).await;
    let freeze_only = service_named(&w, "other", &[ServiceScope::ManageFreeze]).await;
    assert!(matches!(
        set(&w, &freeze_only, "alice", "game", Some(1))
            .await
            .unwrap_err(),
        PynError::ServiceScopeRequired(_)
    ));
    assert!(matches!(
        set(&w, &session("alice"), "alice", "missing", Some(1))
            .await
            .unwrap_err(),
        PynError::RepoNotFound(_)
    ));
    assert!(matches!(
        set(&w, &session("alice"), "alice", "game", Some(u64::MAX))
            .await
            .unwrap_err(),
        PynError::InvalidRequest(_)
    ));

    set(&w, &session("alice"), "alice", "game", Some(10))
        .await
        .unwrap();
    set(&w, &session("alice"), "acme", "tools", Some(20))
        .await
        .unwrap();
    set(&w, &billing, "alice", "game", Some(30)).await.unwrap();
    let root = admin(&w).await;
    set(&w, &root, "acme", "tools", Some(40)).await.unwrap();
    let record = w.repos.open("alice", "game").await.unwrap().record;
    assert_eq!(record.storage_cap, Some(30));
    assert_eq!(
        w.repos
            .open("acme", "tools")
            .await
            .unwrap()
            .record
            .storage_cap,
        Some(40)
    );

    let log = w
        .audit
        .list(
            &AuditScope::Repo(record.id.clone()),
            &AuditQuery {
                limit: 50,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let changes: Vec<_> = log
        .iter()
        .filter(|e| e.action == AuditAction::RepoStorageCapChanged)
        .map(|e| (e.actor.to_string(), e.detail.clone()))
        .collect();
    assert_eq!(
        changes,
        [
            (
                "@service:billing".to_string(),
                "alice/game: storage cap 30 bytes".to_string()
            ),
            (
                "alice".to_string(),
                "alice/game: storage cap 10 bytes".to_string()
            ),
        ]
    );
}

#[tokio::test]
async fn the_breakdown_is_for_repository_admins() {
    let w = world();
    for name in ["alice", "bob"] {
        signup(&w, name).await;
    }
    let record = create(&w, "alice", "game").await;
    checkin(&w, "alice", "game", "Art/a.png", "0123456789")
        .await
        .unwrap();
    checkin(&w, "alice", "game", "notes", "abc").await.unwrap();
    let owner = w
        .access
        .principal_in(&record.id, &session("alice"))
        .await
        .unwrap();
    w.access
        .set_user_role(&owner, &record.id, &user("bob"), Role::Writer)
        .await
        .unwrap();

    let report = w
        .repos
        .repo_breakdown(&session("alice"), &record)
        .await
        .unwrap();
    let folders: Vec<_> = report
        .folders
        .iter()
        .map(|f| (f.folder.as_str(), f.stored_bytes))
        .collect();
    assert_eq!(folders, [("Art", 10), ("", 3)]);
    assert!(matches!(
        w.repos
            .repo_breakdown(&session("bob"), &record)
            .await
            .unwrap_err(),
        PynError::Forbidden(_)
    ));
}

#[tokio::test]
async fn a_frozen_owner_cannot_change_a_cap_but_an_operator_can() {
    let w = world();
    signup(&w, "alice").await;
    create(&w, "alice", "game").await;
    let root = admin(&w).await;
    w.access
        .freeze_owner(&root, &user("alice"), "plan lapsed")
        .await
        .unwrap();
    assert!(matches!(
        set(&w, &session("alice"), "alice", "game", Some(5))
            .await
            .unwrap_err(),
        PynError::OwnerFrozen { .. }
    ));
    assert_eq!(
        w.repos
            .open("alice", "game")
            .await
            .unwrap()
            .record
            .storage_cap,
        None
    );
    set(&w, &root, "alice", "game", Some(5)).await.unwrap();
    assert_eq!(
        w.repos
            .open("alice", "game")
            .await
            .unwrap()
            .record
            .storage_cap,
        Some(5)
    );
}

async fn service_named(w: &World, name: &str, scopes: &[ServiceScope]) -> Identity {
    let root = admin(w).await;
    let (_, secret) = w
        .access
        .create_service_credential(&root, name, scopes.iter().copied().collect::<BTreeSet<_>>())
        .await
        .unwrap();
    w.access.identify(&secret).await.unwrap()
}
