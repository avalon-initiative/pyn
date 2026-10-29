//! Account, organization, repository and limit changes queue wire events with the change.

use std::sync::Arc;

use chrono::{Duration, TimeZone, Utc};
use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore, MemoryWebhookStore,
};
use pyn_core::{
    AccessConfig, AccessService, AccessStore, Credential, Identity, LimitsChange, ManualClock,
    NewSubscription, Registration, RegistrationMode, RepoUpdate, Repositories, Rules, UserId,
    WebhookSettings, WebhookStore,
};
use pyn_proto::{Event, EventData, OwnerKind};

fn who(name: &str) -> Identity {
    Identity {
        user: UserId::new(name),
        credential: Credential::Session,
    }
}

#[tokio::test]
async fn lifecycle_changes_queue_decodable_events() {
    let now = Utc.with_ymd_and_hms(2030, 1, 1, 12, 0, 0).unwrap();
    let clock = Arc::new(ManualClock::new(now));
    let hooks = Arc::new(MemoryWebhookStore::new());
    let meta = Arc::new(MemoryMetadataStore::new().with_outbox(hooks.clone()));
    let store = Arc::new(MemoryAccessStore::new().with_outbox(hooks.clone()));
    let audit = Arc::new(MemoryAuditStore::new());
    let access = Arc::new(
        AccessService::new(
            store.clone(),
            clock.clone(),
            Arc::new(pyn_server::events::WebhookLimitEvents::new(
                hooks.clone(),
                "https://pyn.example".into(),
            )),
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
            now,
        )
        .await
        .unwrap();
    store.ensure_user(&UserId::new("root"), now).await.unwrap();
    store.set_admin(&UserId::new("root"), true).await.unwrap();
    let (root, alice) = (who("root"), who("alice"));

    access
        .register(Registration::new("alice", "correct horse battery"))
        .await
        .unwrap();
    access.approve_account(&root, &alice.user).await.unwrap();
    access.create_org(&alice, "acme").await.unwrap();
    let repo = repos
        .create(&alice, &alice.user, "game", None, None)
        .await
        .unwrap();
    repos
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
    repos
        .set_storage_cap(&alice, &alice.user, "game2", Some(1 << 30))
        .await
        .unwrap();
    access
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
    access
        .disable_account(&root, &alice.user, Some("spam"))
        .await
        .unwrap();

    let due = hooks
        .claim_due(100, Duration::minutes(1), now + Duration::hours(1))
        .await
        .unwrap();
    let mut events: Vec<Event> = due
        .iter()
        .map(|d| (d.event.id, serde_json::from_str(&d.event.payload).unwrap()))
        .collect::<std::collections::BTreeMap<_, _>>()
        .into_values()
        .collect();
    assert!(events.iter().all(|e| e.server == "https://pyn.example"));
    let data: Vec<EventData> = events.drain(..).map(|e| e.data).collect();
    assert!(
        matches!(&data[0], EventData::AccountCreated(d) if d.user == "alice" && d.pending_approval)
    );
    assert!(
        matches!(&data[1], EventData::AccountApproved(d) if d.user == "alice" && d.actor == "root")
    );
    assert!(
        matches!(&data[2], EventData::OrganizationCreated(d) if d.name == "acme" && d.owner == "alice" && d.actor == "alice")
    );
    assert!(
        matches!(&data[3], EventData::RepositoryCreated(d) if d.owner == "alice" && d.name == "game")
    );
    assert!(
        matches!(&data[4], EventData::RepositoryRenamed(d) if d.name == "game2" && d.previous_name == "game")
    );
    assert!(matches!(&data[5], EventData::RepositoryStorageCapChanged(d)
        if d.owner == "alice" && d.name == "game2" && d.storage_cap == Some(1 << 30) && d.actor == "alice"
            && d.repo_id == repo.id.to_string()));
    assert!(matches!(&data[6], EventData::OwnerLimitsChanged(d)
        if d.owner == "acme" && d.kind == OwnerKind::Org && d.limits.repositories == Some(3) && d.actor == "root"));
    assert!(matches!(&data[7], EventData::AccountDisabled(d)
        if d.user == "alice" && d.reason.as_deref() == Some("spam")));
    assert_eq!(data.len(), 8);
}
