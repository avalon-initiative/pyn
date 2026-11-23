//! Every event type the server can raise is in the closed set a webhook may subscribe to.

use std::sync::Arc;

use chrono::{TimeZone, Utc};
use pyn_core::memory::MemoryWebhookStore;
use pyn_core::{
    AccountId, AccountKind, DomainEvent, LimitEvents, Limits, RepoId, RepoPath, UserId, Visibility,
};
use pyn_proto::{self as api, KNOWN_EVENT_TYPES};
use pyn_server::events::WebhookLimitEvents;

fn every_domain_event() -> Vec<DomainEvent> {
    let now = Utc.with_ymd_and_hms(2030, 1, 1, 0, 0, 0).unwrap();
    let u = || UserId::new("alice");
    let id = || AccountId::new("u1");
    let repo = || RepoId::new("r1");
    let path = || RepoPath::new("a.bin").unwrap();
    vec![
        DomainEvent::AccountCreated {
            user: u(),
            user_id: id(),
            pending_approval: false,
        },
        DomainEvent::AccountApproved {
            user: u(),
            user_id: id(),
            actor: u(),
        },
        DomainEvent::AccountDisabled {
            user: u(),
            user_id: id(),
            actor: u(),
            reason: None,
        },
        DomainEvent::AccountEnabled {
            user: u(),
            user_id: id(),
            actor: u(),
        },
        DomainEvent::AccountDeletionScheduled {
            user: u(),
            user_id: id(),
            actor: u(),
            by_owner: true,
            reason: None,
            purge_at: now,
        },
        DomainEvent::AccountDeletionRestored {
            user: u(),
            user_id: id(),
        },
        DomainEvent::AccountDeleted {
            user: u(),
            user_id: id(),
            actor: u(),
            reason: None,
        },
        DomainEvent::AccountRenamed {
            old_name: u(),
            new_name: u(),
            user_id: id(),
            actor: u(),
        },
        DomainEvent::OrganizationCreated {
            name: u(),
            org_id: id(),
            owner: u(),
            owner_id: id(),
            actor: u(),
        },
        DomainEvent::OrganizationDeleted {
            name: u(),
            org_id: id(),
            actor: u(),
        },
        DomainEvent::RepositoryCreated {
            repo_id: repo(),
            owner: u(),
            owner_id: id(),
            name: "g".into(),
            visibility: Visibility::Private,
            actor: u(),
        },
        DomainEvent::RepositoryRenamed {
            repo_id: repo(),
            owner: u(),
            owner_id: id(),
            name: "g".into(),
            previous_name: "h".into(),
            actor: u(),
        },
        DomainEvent::RepositoryDeleted {
            repo_id: repo(),
            owner: u(),
            owner_id: id(),
            name: "g".into(),
            actor: u(),
        },
        DomainEvent::RepositoryStorageCapChanged {
            repo_id: repo(),
            owner: u(),
            owner_id: id(),
            name: "g".into(),
            storage_cap: None,
            actor: u(),
        },
        DomainEvent::OwnerLimitsChanged {
            owner: u(),
            owner_id: id(),
            kind: AccountKind::User,
            limits: Limits::default(),
            actor: u(),
        },
        DomainEvent::OwnerFrozen {
            owner: u(),
            owner_id: id(),
            kind: AccountKind::User,
            reason: "r".into(),
            actor: u(),
        },
        DomainEvent::OwnerUnfrozen {
            owner: u(),
            owner_id: id(),
            kind: AccountKind::User,
            actor: u(),
        },
        DomainEvent::LockAcquired {
            owner: u(),
            owner_id: id(),
            repo: "g".into(),
            repo_id: repo(),
            path: path(),
            holder: u(),
            expires_at: now,
            acting_admin: None,
        },
        DomainEvent::LockReleased {
            owner: u(),
            owner_id: id(),
            repo: "g".into(),
            repo_id: repo(),
            path: path(),
            holder: u(),
            reason: None,
            acting_admin: None,
        },
        DomainEvent::LockExpired {
            owner: u(),
            owner_id: id(),
            repo: "g".into(),
            repo_id: repo(),
            path: path(),
            holder: u(),
            expires_at: now,
        },
        DomainEvent::LockForced {
            owner: u(),
            owner_id: id(),
            repo: "g".into(),
            repo_id: repo(),
            path: path(),
            holder: u(),
            actor: u(),
            reason: None,
            acting_admin: None,
        },
    ]
}

#[test]
fn every_event_the_server_can_raise_is_in_the_set() {
    let now = Utc.with_ymd_and_hms(2030, 1, 1, 0, 0, 0).unwrap();
    let events = WebhookLimitEvents::new(Arc::new(MemoryWebhookStore::new()), "https://p".into());
    let mut raised: Vec<String> = every_domain_event()
        .into_iter()
        .map(|e| events.draft(e, now).unwrap().event_type)
        .collect();
    raised.push("owner.limit_reached".into());
    for t in &raised {
        assert!(
            KNOWN_EVENT_TYPES.contains(&t.as_str()),
            "{t} is not in the set"
        );
    }
    for t in KNOWN_EVENT_TYPES {
        assert!(
            raised.iter().any(|r| r == t),
            "{t} is in the set but nothing raises it"
        );
        assert!(
            api::event_type_description(t).is_some_and(|d| !d.is_empty()),
            "{t}"
        );
    }
}
