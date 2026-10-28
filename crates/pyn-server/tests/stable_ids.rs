//! Users, organizations and repositories keep an id that a rename does not change and a delete never frees.

use std::sync::Arc;

use chrono::{Duration, TimeZone, Utc};
use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore, MemoryWebhookStore,
};
use pyn_core::{
    AccessConfig, AccessService, AccessStore, Credential, Identity, ManualClock, NewSubscription,
    Registration, RegistrationMode, RepoPath, RepoUpdate, Repositories, Rules, UserId,
    WebhookSettings, WebhookStore,
};
use pyn_proto::{Event, EventData};

fn who(name: &str) -> Identity {
    Identity {
        user: UserId::new(name),
        credential: Credential::Session,
    }
}

#[tokio::test]
async fn events_carry_ids_that_survive_a_rename_and_are_never_reused() {
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
            require_approval: false,
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
        Rules::from_toml("[meta]\ndefault = \"exclusive\"\n").unwrap(),
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
    let alice_id = access.account_id(&alice.user).await.unwrap().to_string();
    let org = access.create_org(&alice, "acme").await.unwrap();
    let org_id = org.id.to_string();
    let repo = repos
        .create(&alice, &alice.user, "game", None, None)
        .await
        .unwrap();
    let repo_id = repo.id.to_string();
    let open = repos.open("alice", "game").await.unwrap();
    open.service
        .checkout(&RepoPath::new("a.bin").unwrap(), &alice.user, None)
        .await
        .unwrap();
    let renamed = repos
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
    assert_eq!(renamed.id.as_str(), repo_id);
    repos.delete(&alice, &renamed).await.unwrap();
    let again = repos
        .create(&alice, &alice.user, "game", None, None)
        .await
        .unwrap();
    assert_ne!(again.id.as_str(), repo_id);
    repos.delete(&alice, &again).await.unwrap();
    access.delete_org(&alice, &org.user).await.unwrap();
    let org_again = access.create_org(&alice, "acme").await.unwrap();
    assert_ne!(org_again.id.to_string(), org_id);
    access
        .freeze_owner(&root, &org_again.user, "plan lapsed")
        .await
        .unwrap();
    access.unfreeze_owner(&root, &org_again.user).await.unwrap();
    access
        .freeze_owner(&root, &alice.user, "abuse report")
        .await
        .unwrap();
    access
        .disable_account(&root, &alice.user, None)
        .await
        .unwrap();

    let due = hooks
        .claim_due(100, Duration::minutes(1), now + Duration::hours(1))
        .await
        .unwrap();
    let events: Vec<EventData> = due
        .iter()
        .map(|d| {
            (
                d.event.id,
                serde_json::from_str::<Event>(&d.event.payload).unwrap(),
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>()
        .into_values()
        .map(|e| e.data)
        .collect();
    let find = |f: &dyn Fn(&EventData) -> bool| events.iter().filter(|e| f(e)).count();

    assert!(events.iter().any(|e| matches!(e,
        EventData::AccountCreated(d) if d.user == "alice" && d.user_id == alice_id)));
    assert!(events.iter().any(|e| matches!(e,
        EventData::OrganizationCreated(d) if d.org_id == org_id && d.owner_id == alice_id)));
    assert!(events.iter().any(|e| matches!(e,
        EventData::RepositoryCreated(d) if d.repo_id == repo_id && d.owner_id == alice_id)));
    assert!(events.iter().any(|e| matches!(e,
        EventData::LockAcquired(d) if d.repo_id == repo_id && d.owner_id == alice_id)));
    assert!(events.iter().any(|e| matches!(e,
        EventData::RepositoryRenamed(d)
            if d.repo_id == repo_id && d.name == "game2" && d.previous_name == "game")));
    assert!(events.iter().any(|e| matches!(e,
        EventData::RepositoryDeleted(d) if d.repo_id == repo_id && d.name == "game2")));
    assert!(events.iter().any(|e| matches!(e,
        EventData::RepositoryDeleted(d) if d.repo_id == again.id.as_str())));
    assert_eq!(
        find(&|e| matches!(e, EventData::OrganizationDeleted(d) if d.org_id == org_id)),
        1
    );
    assert!(events.iter().any(|e| matches!(e,
        EventData::OrganizationCreated(d) if d.org_id == org_again.id.as_str())));
    assert!(events.iter().any(|e| matches!(e,
        EventData::AccountDisabled(d) if d.user_id == alice_id)));
    assert!(events.iter().any(|e| matches!(e,
        EventData::OwnerFrozen(d)
            if d.owner_id == org_again.id.as_str()
                && d.owner == "acme"
                && d.kind == pyn_proto::OwnerKind::Org
                && d.reason == "plan lapsed"
                && d.actor == "root")));
    assert!(events.iter().any(|e| matches!(e,
        EventData::OwnerUnfrozen(d) if d.owner_id == org_again.id.as_str() && d.actor == "root")));
    assert!(events.iter().any(|e| matches!(e,
        EventData::OwnerFrozen(d)
            if d.owner_id == alice_id && d.kind == pyn_proto::OwnerKind::User)));
}
