//! Limit refusals keep their errors and queue owner events through the shared webhook store.

use std::sync::Arc;

use chrono::{Duration, TimeZone, Utc};
use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore, MemoryWebhookStore,
};
use pyn_core::{
    AccessConfig, AccessService, Clock, Credential, DeliveryQuery, Identity, Limits, ManualClock,
    NewSubscription, OrgRole, PynError, Registration, RegistrationMode, RepoPath, Repositories,
    Rules, UserId, WebhookSettings, WebhookStore,
};

use pyn_proto::{Event, EventData, LimitKind, OwnerLimitReachedData};

struct World {
    repos: Repositories,
    access: Arc<AccessService>,
    hooks: Arc<MemoryWebhookStore>,
    clock: Arc<ManualClock>,
}

fn who(name: &str) -> Identity {
    Identity {
        user: UserId::new(name),
        credential: Credential::Session,
    }
}

fn world(limits: Limits, upload_cap: u64) -> World {
    let clock = Arc::new(ManualClock::new(
        Utc.with_ymd_and_hms(2030, 1, 1, 12, 0, 0).unwrap(),
    ));
    let hooks = Arc::new(MemoryWebhookStore::new());
    let meta = Arc::new(MemoryMetadataStore::new());
    let audit = Arc::new(MemoryAuditStore::new());
    let access = Arc::new(
        AccessService::new(Arc::new(MemoryAccessStore::new()), clock.clone())
            .with_config(AccessConfig {
                default_limits: limits,
                public_url: "https://pyn.example".into(),
                registration: RegistrationMode::Open,
                require_email_verification: false,
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
        Rules::from_toml("[meta]\ndefault = \"shared\"\n[exclusive]\npaths = [\"Content/\"]\n")
            .unwrap(),
    )
    .with_default_max_locks(1)
    .unwrap()
    .with_unreferenced_upload_cap(upload_cap);
    World {
        repos,
        access,
        hooks,
        clock,
    }
}

async fn subscribe(w: &World) -> i64 {
    w.hooks
        .create_subscription(
            NewSubscription {
                name: "limits".into(),
                url: "https://example.test/hook".into(),
                event_types: vec!["owner.limit_reached".into()],
                enabled: true,
                created_by: UserId::new("root"),
            },
            w.clock.now(),
        )
        .await
        .unwrap()
        .id
}

async fn deliveries(w: &World, id: i64) -> usize {
    w.hooks
        .list_deliveries(
            id,
            &DeliveryQuery {
                limit: 100,
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .len()
}

#[tokio::test]
async fn repository_refusals_are_throttled_using_the_injected_clock() {
    let w = world(
        Limits {
            repositories: Some(1),
            ..Default::default()
        },
        100,
    );
    w.repos
        .create(&who("alice"), &UserId::new("alice"), "existing", None, None)
        .await
        .unwrap();
    let sub = subscribe(&w).await;
    for (advance, expected) in [
        (Duration::zero(), 1),
        (Duration::zero(), 1),
        (Duration::hours(1) - Duration::microseconds(1), 1),
        (Duration::microseconds(1), 2),
        (Duration::hours(1) + Duration::microseconds(1), 3),
    ] {
        w.clock.advance(advance);
        let err = w
            .repos
            .create(&who("alice"), &UserId::new("alice"), "no", None, None)
            .await
            .unwrap_err();
        assert!(matches!(&err, PynError::RepoLimitReached { owner, limit: 1 } if owner == "alice"));
        assert_eq!(err.code(), "repo_limit_reached");
        assert_eq!(
            err.to_string(),
            "repository limit reached: alice may own at most 1 repositories on this server"
        );
        assert_eq!(deliveries(&w, sub).await, expected);
    }
}

#[tokio::test]
async fn members_storage_and_non_owner_locks_keep_their_errors_and_attribute_the_resource_owner() {
    let w = world(
        Limits {
            members: Some(1),
            storage_bytes: Some(3),
            ..Default::default()
        },
        100,
    );
    let sub = subscribe(&w).await;
    w.access
        .register(Registration::new("bob", "correct horse battery"))
        .await
        .unwrap();
    w.access.create_org(&who("alice"), "studio").await.unwrap();
    for _ in 0..2 {
        let err = w
            .access
            .add_org_member(
                &who("alice"),
                &UserId::new("studio"),
                &UserId::new("bob"),
                OrgRole::Member,
            )
            .await
            .unwrap_err();
        assert!(matches!(&err, PynError::MemberLimitReached { org, limit: 1 } if org == "studio"));
        assert_eq!(err.code(), "member_limit_reached");
        assert_eq!(
            err.to_string(),
            "member limit reached: studio may have at most 1 members on this server"
        );
    }
    w.repos
        .create(&who("alice"), &UserId::new("alice"), "game", None, None)
        .await
        .unwrap();
    let open = w.repos.open("alice", "game").await.unwrap();
    open.service
        .checkout(
            &RepoPath::new("Content/one").unwrap(),
            &UserId::new("bob"),
            None,
        )
        .await
        .unwrap();
    let principal = w
        .access
        .principal_in(&open.record.id, &who("alice"))
        .await
        .unwrap();
    let content = open.service.upload_blob(b"four".to_vec()).await.unwrap();
    for _ in 0..2 {
        let err = open
            .service
            .checkout(
                &RepoPath::new("Content/two").unwrap(),
                &UserId::new("bob"),
                None,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, PynError::LockLimitReached { limit: 1 }));
        assert_eq!(err.code(), "lock_limit_reached");
        assert_eq!(
            err.to_string(),
            "lock limit reached: you already hold 1 lock(s) in this repository; unlock one before locking another"
        );
        let err = open
            .service
            .checkin(
                &principal,
                &RepoPath::new("file").unwrap(),
                content,
                None,
                "m".into(),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(&err, PynError::StorageLimitReached { owner, limit: 3, used: 0 } if owner == "alice")
        );
        assert_eq!(err.code(), "storage_limit_reached");
        assert_eq!(
            err.to_string(),
            "storage limit reached: alice may store at most 3 bytes on this server and uses 0"
        );
    }
    assert_eq!(deliveries(&w, sub).await, 3);
    let due = w
        .hooks
        .claim_due(100, Duration::minutes(1), w.clock.now())
        .await
        .unwrap();
    assert_eq!(due.len(), 3);
    let mut data = Vec::new();
    let mut ids = std::collections::HashSet::new();
    for item in due {
        let event: Event = serde_json::from_str(&item.event.payload).unwrap();
        assert_eq!(item.event.event_type, "owner.limit_reached");
        assert_eq!(event.event_type, item.event.event_type);
        assert_eq!(event.created_at, w.clock.now());
        assert_eq!(event.server, "https://pyn.example");
        assert_eq!(event.version, 1);
        assert!(!event.id.is_empty());
        assert!(ids.insert(event.id));
        data.push(
            serde_json::from_str::<serde_json::Value>(&item.event.payload).unwrap()["data"].clone(),
        );
    }
    for (owner, limit, value) in [
        ("studio", "members", 1),
        ("alice", "locks", 1),
        ("alice", "storage_bytes", 3),
    ] {
        assert!(
            data.contains(&serde_json::json!({ "owner": owner, "limit": limit, "value": value }))
        );
    }
}

#[tokio::test]
async fn upload_budget_refusals_are_events_of_the_repository_owner() {
    let w = world(Limits::default(), 0);
    let sub = subscribe(&w).await;
    w.repos
        .create(&who("alice"), &UserId::new("alice"), "game", None, None)
        .await
        .unwrap();
    let open = w.repos.open("alice", "game").await.unwrap();
    for _ in 0..2 {
        let err = open.service.upload_blob(b"x".to_vec()).await.unwrap_err();
        assert!(matches!(
            err,
            PynError::StorageLimitReached { limit: 0, .. }
        ));
    }
    assert_eq!(deliveries(&w, sub).await, 1);
    let due = w
        .hooks
        .claim_due(10, Duration::minutes(1), w.clock.now())
        .await
        .unwrap();
    let event: Event = serde_json::from_str(&due[0].event.payload).unwrap();
    assert_eq!(
        event.data,
        EventData::OwnerLimitReached(OwnerLimitReachedData {
            owner: "alice".into(),
            limit: LimitKind::StorageBytes,
            value: 0,
        })
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn independent_services_racing_a_limit_record_exactly_one_event() {
    let hooks = Arc::new(MemoryWebhookStore::new());
    pyn_core::contract::racing_limit_services_record_one_event(
        Arc::new(MemoryMetadataStore::new()),
        Arc::new(MemoryAccessStore::new()),
        [hooks.clone(), hooks],
    )
    .await;
}
