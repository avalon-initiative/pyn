//! Account, organization, repository and limit changes queue their webhook events with the change.

use std::sync::Arc;

use chrono::{DateTime, Duration, TimeZone, Utc};

use super::{Access, Store, Webhooks};
use crate::memory::{MemoryAuditStore, MemoryObjectStore};
use crate::{
    AccessConfig, AccessService, Credential, DeliveryQuery, EventDraft, Identity, LimitsChange,
    ManualClock, MemoryLimitEvents, NewSubscription, PynError, Registration, RegistrationMode,
    RepoUpdate, Repositories, Rules, UserId, WebhookSettings,
};

fn t0() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2030, 1, 1, 12, 0, 0).unwrap()
}

fn who(name: &str) -> Identity {
    Identity {
        user: UserId::new(name),
        credential: Credential::Session,
    }
}

struct World {
    clock: Arc<ManualClock>,
    access: Arc<AccessService>,
    repos: Repositories,
    hooks: Webhooks,
}

async fn world(meta: Store, access_store: Access, hooks: Webhooks) -> World {
    let clock = Arc::new(ManualClock::new(t0()));
    let audit = Arc::new(MemoryAuditStore::new());
    let access = Arc::new(
        AccessService::new(
            access_store,
            clock.clone(),
            Arc::new(MemoryLimitEvents::new()),
        )
        .with_config(AccessConfig {
            registration: RegistrationMode::Open,
            require_email_verification: false,
            require_approval: true,
            ..AccessConfig::default()
        })
        .with_registry(meta.clone())
        .with_audit(audit.clone())
        .with_webhooks(hooks.clone(), WebhookSettings::default()),
    );
    let repos = Repositories::new(
        meta,
        Arc::new(MemoryObjectStore::new()),
        audit,
        access.clone(),
        clock.clone(),
        Rules::empty(),
    );
    hooks
        .create_subscription(
            NewSubscription {
                name: "all".into(),
                url: "https://example.test/hook".into(),
                event_types: vec![],
                enabled: true,
                created_by: UserId::new("root"),
            },
            t0(),
        )
        .await
        .unwrap();
    World {
        clock,
        access,
        repos,
        hooks,
    }
}

async fn queued(hooks: &Webhooks) -> Vec<String> {
    let due = hooks
        .claim_due(1000, Duration::minutes(1), t0() + Duration::hours(1))
        .await
        .unwrap();
    let mut events: Vec<_> = due.into_iter().map(|d| d.event).collect();
    events.sort_by_key(|e| e.id);
    events.into_iter().map(|e| e.event_type).collect()
}

pub async fn lock_changes_queue_their_events(meta: Store, access_store: Access, hooks: Webhooks) {
    let w = world(meta, access_store.clone(), hooks).await;
    let (alice, bob) = (who("alice"), who("bob"));
    for user in ["alice", "bob"] {
        access_store
            .ensure_user(&UserId::new(user), t0())
            .await
            .unwrap();
    }
    w.repos
        .create(&alice, &alice.user, "game", None, None)
        .await
        .unwrap();
    let svc = w.repos.open("alice", "game").await.unwrap().service;
    let path = crate::RepoPath::try_from("Content/a.umap".to_string()).unwrap();

    svc.checkout(&path, &alice.user, None).await.unwrap();
    svc.checkout(&path, &alice.user, None).await.unwrap();
    assert!(svc.checkout(&path, &bob.user, None).await.is_err());
    svc.release(&path, &alice.user).await.unwrap();
    assert!(svc.release(&path, &alice.user).await.is_err());
    svc.checkout(&path, &alice.user, None).await.unwrap();
    svc.force_unlock(&path, &bob.user, "stuck").await.unwrap();
    svc.checkout(&path, &alice.user, None).await.unwrap();
    let record = w.repos.open("alice", "game").await.unwrap().record;
    let principal = w.access.principal(&record.id, &alice.user).await.unwrap();
    let content = svc.upload_blob(b"v1".to_vec()).await.unwrap();
    let rev = svc
        .checkin(&principal, &path, content, None, "v1".into())
        .await
        .unwrap();
    svc.checkout(&path, &alice.user, Some(rev.id))
        .await
        .unwrap();
    w.clock.advance(Duration::days(30));
    svc.checkout(&path, &bob.user, Some(rev.id)).await.unwrap();

    let due = w
        .hooks
        .claim_due(1000, Duration::minutes(1), t0() + Duration::days(31))
        .await
        .unwrap();
    let mut events: Vec<_> = due.into_iter().map(|d| d.event).collect();
    events.sort_by_key(|e| e.id);
    let locks: Vec<String> = events
        .into_iter()
        .map(|e| e.event_type)
        .filter(|t| t.starts_with("lock."))
        .collect();
    assert_eq!(
        locks,
        [
            "lock.acquired",
            "lock.released",
            "lock.acquired",
            "lock.forced",
            "lock.acquired",
            "lock.released",
            "lock.acquired",
            "lock.expired",
            "lock.acquired",
        ]
    );
}

pub async fn changes_queue_their_events_in_order(
    meta: Store,
    access_store: Access,
    hooks: Webhooks,
) {
    let w = world(meta, access_store.clone(), hooks).await;
    access_store
        .ensure_user(&UserId::new("root"), t0())
        .await
        .unwrap();
    access_store
        .set_admin(&UserId::new("root"), true)
        .await
        .unwrap();
    let root = who("root");

    let signup = w
        .access
        .register(Registration::new("alice", "correct horse battery"))
        .await
        .unwrap();
    assert_eq!(signup.user, UserId::new("alice"));
    w.access
        .approve_account(&root, &UserId::new("alice"))
        .await
        .unwrap();
    let alice = who("alice");
    w.access.create_org(&alice, "acme").await.unwrap();
    let repo = w
        .repos
        .create(&alice, &alice.user, "game", None, None)
        .await
        .unwrap();
    w.repos
        .update(
            &alice,
            &repo,
            RepoUpdate {
                name: Some("game2".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let repo = w.repos.open("alice", "game2").await.unwrap().record;
    w.repos
        .update(
            &alice,
            &repo,
            RepoUpdate {
                visibility: Some(crate::Visibility::Public),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    w.access
        .set_owner_limits(
            &root,
            &UserId::new("acme"),
            LimitsChange {
                repositories: Some(Some(3)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    w.access
        .freeze_owner(&root, &UserId::new("acme"), "plan lapsed")
        .await
        .unwrap();
    w.access
        .unfreeze_owner(&root, &UserId::new("acme"))
        .await
        .unwrap();
    w.repos.delete(&alice, &repo).await.unwrap();
    w.access
        .disable_account(&root, &UserId::new("alice"), Some("spam"))
        .await
        .unwrap();
    w.access
        .enable_account(&root, &UserId::new("alice"))
        .await
        .unwrap();
    w.access
        .delete_org(&alice, &UserId::new("acme"))
        .await
        .unwrap();

    assert_eq!(
        queued(&w.hooks).await,
        [
            "account.created",
            "account.approved",
            "organization.created",
            "repository.created",
            "repository.renamed",
            "owner.limits_changed",
            "owner.frozen",
            "owner.unfrozen",
            "repository.deleted",
            "account.disabled",
            "account.enabled",
            "organization.deleted",
        ]
    );
}

pub async fn refused_changes_queue_no_event(meta: Store, access_store: Access, hooks: Webhooks) {
    let w = world(meta, access_store.clone(), hooks).await;
    access_store
        .ensure_user(&UserId::new("root"), t0())
        .await
        .unwrap();
    access_store
        .set_admin(&UserId::new("root"), true)
        .await
        .unwrap();
    let root = who("root");
    let alice = who("alice");
    w.access
        .register(Registration::new("alice", "correct horse battery"))
        .await
        .unwrap();
    w.access.approve_account(&root, &alice.user).await.unwrap();
    w.access.create_org(&alice, "acme").await.unwrap();
    let game = w
        .repos
        .create(&alice, &alice.user, "game", None, None)
        .await
        .unwrap();
    w.repos
        .create(&alice, &alice.user, "tool", None, None)
        .await
        .unwrap();
    let before = count(&w.hooks).await;
    assert_eq!(before, 5);

    let taken = |e: PynError| matches!(e, PynError::UserExists(_) | PynError::RepoExists(_));
    assert!(taken(
        w.access
            .register(Registration::new("alice", "correct horse battery"))
            .await
            .unwrap_err()
    ));
    assert!(taken(
        w.access.create_org(&alice, "acme").await.unwrap_err()
    ));
    assert!(taken(
        w.repos
            .create(&alice, &alice.user, "game", None, None)
            .await
            .unwrap_err()
    ));
    assert!(taken(
        w.repos
            .update(
                &alice,
                &game,
                RepoUpdate {
                    name: Some("tool".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err()
    ));
    assert!(w.access.enable_account(&root, &alice.user).await.is_err());
    assert!(w.access.approve_account(&root, &alice.user).await.is_err());
    assert!(
        w.access
            .disable_account(&root, &root.user, None)
            .await
            .is_err()
    );
    assert_eq!(count(&w.hooks).await, before);

    w.access
        .freeze_owner(&root, &alice.user, "plan lapsed")
        .await
        .unwrap();
    let frozen = count(&w.hooks).await;
    assert_eq!(frozen, before + 1);
    let is_frozen = |e: PynError| matches!(e, PynError::OwnerFrozen { .. });
    assert!(is_frozen(
        w.repos
            .create(&alice, &alice.user, "fresh", None, None)
            .await
            .unwrap_err()
    ));
    assert!(is_frozen(
        w.repos
            .update(
                &alice,
                &game,
                RepoUpdate {
                    name: Some("renamed".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err()
    ));
    assert!(is_frozen(w.repos.delete(&alice, &game).await.unwrap_err()));
    assert_eq!(count(&w.hooks).await, frozen);
    w.access.unfreeze_owner(&root, &alice.user).await.unwrap();
    let before = frozen + 1;
    assert_eq!(count(&w.hooks).await, before);
    assert!(
        access_store
            .delete_org_with_events(&UserId::new("nobody"), &[draft("organization.deleted")])
            .await
            .map(|deleted| !deleted)
            .unwrap()
    );
    assert!(
        !access_store
            .set_signup_with_events(
                &UserId::new("nobody"),
                crate::SignupStage::Complete,
                &[draft("account.approved")],
            )
            .await
            .unwrap()
    );
    assert_eq!(count(&w.hooks).await, before);
}

fn draft(event_type: &str) -> EventDraft {
    EventDraft {
        event_type: event_type.into(),
        payload: "{}".into(),
        at: t0(),
    }
}

async fn count(hooks: &Webhooks) -> usize {
    let sub = hooks.list_subscriptions().await.unwrap()[0].id;
    hooks
        .list_deliveries(
            sub,
            &DeliveryQuery {
                limit: 1000,
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .len()
}
