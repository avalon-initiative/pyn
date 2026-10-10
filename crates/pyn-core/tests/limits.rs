//! Per-owner limits and usage: opt-in, enforced with distinct errors, set by administrators, visible to the owner.

use std::collections::BTreeSet;
use std::sync::Arc;

use chrono::{TimeZone, Utc};
use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore,
};
use pyn_core::{
    AccessConfig, AccessService, AuditAction, AuditQuery, AuditScope, AuditStore, Credential,
    Identity, Limits, LimitsChange, ManualClock, ObjectStore, OrgRole, PynError, Registration,
    RegistrationMode, RepoPath, Repositories, Rules, ServiceScope, UserId,
};

struct World {
    repos: Repositories,
    access: Arc<AccessService>,
    objects: Arc<MemoryObjectStore>,
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
        AccessService::new(Arc::new(MemoryAccessStore::new()), clock.clone())
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
        objects,
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

fn change(
    repos: Option<Option<u64>>,
    members: Option<Option<u64>>,
    bytes: Option<Option<u64>>,
) -> LimitsChange {
    LimitsChange {
        repositories: repos,
        members,
        storage_bytes: bytes,
    }
}

async fn create(w: &World, who: &str, name: &str) -> Result<pyn_core::RepoRecord, PynError> {
    w.repos
        .create(&session(who), &user(who), name, None, None)
        .await
}

async fn checkin(w: &World, who: &str, repo: &str, path: &str, body: &str) -> Result<(), PynError> {
    let open = w.repos.open(who, repo).await.unwrap();
    let principal = w
        .access
        .principal_in(&open.record.id, &session(who))
        .await
        .unwrap();
    let content = w.objects.put(body.as_bytes().to_vec()).await.unwrap();
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

#[tokio::test]
async fn with_no_limits_configured_nothing_is_limited() {
    let w = world();
    for i in 0..12 {
        create(&w, "alice", &format!("r{i}")).await.unwrap();
    }
    checkin(&w, "alice", "r0", "big.bin", &"x".repeat(100_000))
        .await
        .unwrap();
    w.access
        .create_org(&session("alice"), "acme")
        .await
        .unwrap();
    for i in 0..20 {
        let name = format!("u{i}");
        signup(&w, &name).await;
        w.access
            .add_org_member(
                &session("alice"),
                &user("acme"),
                &user(&name),
                OrgRole::Member,
            )
            .await
            .unwrap();
    }

    let report = w
        .repos
        .owner_report(&session("alice"), &user("alice"))
        .await
        .unwrap();
    assert_eq!(report.limits.effective, Limits::default());
    assert_eq!(report.limits.own, Limits::default());
    assert_eq!(report.usage.repositories, 12);
    assert_eq!(report.usage.stored_bytes, 100_000);
    assert_eq!(report.usage.members, None);
    let org = w
        .repos
        .owner_report(&session("alice"), &user("acme"))
        .await
        .unwrap();
    assert_eq!(org.usage.members, Some(21));
    assert!(
        w.access
            .list_owner_limits(&admin(&w).await)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(w.access.default_limits(), Limits::default());
}

#[tokio::test]
async fn the_repository_limit_stops_creation_and_shrinking_deletes_nothing() {
    let w = world();
    let root = admin(&w).await;
    create(&w, "alice", "one").await.unwrap();
    create(&w, "alice", "two").await.unwrap();
    w.access
        .set_owner_limits(&root, &user("alice"), change(Some(Some(3)), None, None))
        .await
        .unwrap();
    create(&w, "alice", "three").await.unwrap();
    let err = create(&w, "alice", "four").await.unwrap_err();
    assert!(
        matches!(err, PynError::RepoLimitReached { limit: 3, .. }),
        "{err}"
    );
    assert_eq!(err.code(), "repo_limit_reached");
    create(&w, "bob", "one").await.unwrap();

    w.access
        .set_owner_limits(&root, &user("alice"), change(Some(Some(1)), None, None))
        .await
        .unwrap();
    assert_eq!(
        w.repos.list(&session("alice"), None).await.unwrap().len(),
        3
    );
    assert!(matches!(
        create(&w, "alice", "four").await.unwrap_err(),
        PynError::RepoLimitReached { limit: 1, .. }
    ));
    let open = w.repos.open("alice", "one").await.unwrap();
    w.repos
        .delete(&session("alice"), &open.record)
        .await
        .unwrap();
    for name in ["two", "three"] {
        let open = w.repos.open("alice", name).await.unwrap();
        w.repos
            .delete(&session("alice"), &open.record)
            .await
            .unwrap();
    }
    create(&w, "alice", "again").await.unwrap();

    w.access
        .set_owner_limits(&root, &user("alice"), change(Some(None), None, None))
        .await
        .unwrap();
    create(&w, "alice", "unbounded").await.unwrap();
}

#[tokio::test]
async fn the_server_default_applies_until_an_owner_has_its_own() {
    let w = world_with(Limits {
        repositories: Some(1),
        members: None,
        storage_bytes: Some(10),
    });
    let root = admin(&w).await;
    create(&w, "alice", "one").await.unwrap();
    assert!(matches!(
        create(&w, "alice", "two").await.unwrap_err(),
        PynError::RepoLimitReached { limit: 1, .. }
    ));
    let view = w
        .access
        .owner_limits(&session("alice"), &user("alice"))
        .await
        .unwrap();
    assert_eq!(view.own, Limits::default());
    assert_eq!(view.effective.repositories, Some(1));

    w.access
        .set_owner_limits(&root, &user("alice"), change(Some(Some(5)), None, None))
        .await
        .unwrap();
    create(&w, "alice", "two").await.unwrap();
    let view = w
        .access
        .owner_limits(&session("alice"), &user("alice"))
        .await
        .unwrap();
    assert_eq!(view.own.repositories, Some(5));
    assert_eq!(
        view.effective.storage_bytes,
        Some(10),
        "unset fields keep the default"
    );

    let err = checkin(&w, "alice", "one", "a.bin", "12345678901")
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            PynError::StorageLimitReached {
                limit: 10,
                used: 0,
                ..
            }
        ),
        "{err}"
    );
    assert_eq!(err.code(), "storage_limit_reached");
}

#[tokio::test]
async fn storage_is_capped_per_owner_across_repositories_and_never_shrunk() {
    let w = world();
    let root = admin(&w).await;
    create(&w, "alice", "one").await.unwrap();
    create(&w, "alice", "two").await.unwrap();
    checkin(&w, "alice", "one", "a", "123456").await.unwrap();
    w.access
        .set_owner_limits(&root, &user("alice"), change(None, None, Some(Some(10))))
        .await
        .unwrap();
    assert!(matches!(
        checkin(&w, "alice", "two", "b", "abcdefg")
            .await
            .unwrap_err(),
        PynError::StorageLimitReached {
            limit: 10,
            used: 6,
            ..
        }
    ));
    checkin(&w, "alice", "two", "b", "abcd").await.unwrap();

    w.access
        .set_owner_limits(&root, &user("alice"), change(None, None, Some(Some(1))))
        .await
        .unwrap();
    let report = w.repos.owner_report(&root, &user("alice")).await.unwrap();
    assert_eq!(
        report.usage.stored_bytes, 10,
        "lowering the cap removes nothing"
    );
    assert!(checkin(&w, "alice", "two", "c", "zz").await.is_err());
    let per_repo: Vec<_> = report
        .repositories
        .iter()
        .map(|(r, u)| (r.address(), u.stored_bytes, u.files))
        .collect();
    assert_eq!(
        per_repo,
        [("alice/one".into(), 6, 1), ("alice/two".into(), 4, 1)]
    );
}

#[tokio::test]
async fn an_organization_member_limit_applies_to_organizations_only() {
    let w = world();
    let root = admin(&w).await;
    for name in ["bob", "carol"] {
        signup(&w, name).await;
    }
    w.access
        .create_org(&session("alice"), "acme")
        .await
        .unwrap();
    w.access
        .set_owner_limits(&root, &user("acme"), change(None, Some(Some(2)), None))
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
    let err = w
        .access
        .add_org_member(
            &session("alice"),
            &user("acme"),
            &user("carol"),
            OrgRole::Member,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, PynError::MemberLimitReached { limit: 2, .. }),
        "{err}"
    );
    assert_eq!(err.code(), "member_limit_reached");

    let err = w
        .access
        .set_owner_limits(&root, &user("alice"), change(None, Some(Some(2)), None))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::InvalidRequest(_)), "{err}");
    let user_view = w
        .access
        .owner_limits(&session("alice"), &user("alice"))
        .await
        .unwrap();
    assert_eq!(user_view.effective.members, None);
}

#[tokio::test]
async fn limits_are_validated_and_unknown_owners_are_refused() {
    let w = world();
    let root = admin(&w).await;
    signup(&w, "alice").await;
    let err = w
        .access
        .set_owner_limits(
            &root,
            &user("alice"),
            change(Some(Some(u64::MAX)), None, None),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::InvalidRequest(_)), "{err}");
    let err = w
        .access
        .set_owner_limits(&root, &user("ghost"), change(Some(Some(1)), None, None))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::UserNotFound(_)), "{err}");
    w.access
        .set_owner_limits(&root, &user("alice"), change(Some(Some(0)), None, None))
        .await
        .unwrap();
    assert!(matches!(
        create(&w, "alice", "x").await.unwrap_err(),
        PynError::RepoLimitReached { limit: 0, .. }
    ));
}

#[tokio::test]
async fn only_administrators_and_scoped_service_credentials_change_limits() {
    let w = world();
    let root = admin(&w).await;
    w.access
        .create_org(&session("alice"), "acme")
        .await
        .unwrap();
    let set = |who: Identity| {
        let access = w.access.clone();
        async move {
            access
                .set_owner_limits(&who, &user("alice"), change(Some(Some(9)), None, None))
                .await
        }
    };
    assert!(matches!(
        set(session("alice")).await.unwrap_err(),
        PynError::ServerAdminRequired
    ));
    let wrong = service(&w, &[ServiceScope::ManageAccounts]).await;
    assert!(matches!(
        set(wrong).await.unwrap_err(),
        PynError::ServiceScopeRequired(ServiceScope::ManageLimits)
    ));
    w.access
        .revoke_service_credential(&root, "billing")
        .await
        .unwrap();
    let billing = service_named(&w, "limits", &[ServiceScope::ManageLimits]).await;
    set(billing.clone()).await.unwrap();
    set(root.clone()).await.unwrap();
    assert_eq!(w.access.list_owner_limits(&billing).await.unwrap().len(), 1);
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

#[tokio::test]
async fn limits_and_usage_are_visible_to_the_owner_and_to_administrators_only() {
    let w = world();
    let root = admin(&w).await;
    signup(&w, "bob").await;
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
    w.access
        .set_owner_limits(&root, &user("acme"), change(Some(Some(4)), None, None))
        .await
        .unwrap();

    let seen = w
        .access
        .owner_limits(&session("alice"), &user("acme"))
        .await
        .unwrap();
    assert_eq!(seen.effective.repositories, Some(4));
    for outsider in ["bob", "mallory"] {
        let err = w
            .access
            .owner_limits(&session(outsider), &user("acme"))
            .await
            .unwrap_err();
        assert!(
            matches!(err, PynError::NotOrgOwner(_) | PynError::Forbidden(_)),
            "{outsider}: {err}"
        );
    }
    let err = w
        .access
        .owner_limits(&session("bob"), &user("alice"))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::NotNamespaceOwner(_)), "{err}");
    w.access.owner_limits(&root, &user("acme")).await.unwrap();

    let reader = service_named(&w, "reader", &[ServiceScope::ManageLimits]).await;
    w.repos.owner_report(&reader, &user("acme")).await.unwrap();
    let blind = service_named(&w, "blind", &[ServiceScope::ManageAccounts]).await;
    assert!(w.repos.owner_report(&blind, &user("acme")).await.is_err());
}

#[tokio::test]
async fn changes_are_recorded_in_the_server_and_organization_logs() {
    let w = world();
    let root = admin(&w).await;
    w.access
        .create_org(&session("alice"), "acme")
        .await
        .unwrap();
    w.access
        .set_owner_limits(
            &root,
            &user("acme"),
            change(Some(Some(4)), None, Some(Some(1000))),
        )
        .await
        .unwrap();
    w.access
        .set_owner_limits(&root, &user("alice"), change(Some(Some(2)), None, None))
        .await
        .unwrap();
    let query = AuditQuery {
        limit: 10,
        ..Default::default()
    };
    let server = w.audit.list(&AuditScope::Server, &query).await.unwrap();
    let limit_events: Vec<_> = server
        .iter()
        .filter(|e| e.action == AuditAction::OwnerLimitsChanged)
        .map(|e| (e.actor.to_string(), e.detail.clone()))
        .collect();
    assert_eq!(
        limit_events,
        [
            (
                "root".into(),
                "alice: repositories 2, members default, storage bytes default".into()
            ),
            (
                "root".into(),
                "acme: repositories 4, members default, storage bytes 1000".into()
            ),
        ]
    );
    let org = w
        .audit
        .list(&AuditScope::Org(user("acme")), &query)
        .await
        .unwrap();
    assert!(
        org.iter()
            .any(|e| e.action == AuditAction::OwnerLimitsChanged)
    );
    let alice = w
        .audit
        .list(&AuditScope::Org(user("alice")), &query)
        .await
        .unwrap();
    assert!(alice.is_empty(), "a user has no log of their own");
}
