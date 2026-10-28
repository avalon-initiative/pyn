use std::sync::Arc;

use chrono::{TimeZone, Utc};
use pyn_core::memory::{MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore};
use pyn_core::{
    AccountId, LimitKind, LimitReached, ManualClock, MemoryLimitEvents, PynError, RepoId, RepoPath,
    RepoService, Rules, ServiceConfig, UserId,
};

#[tokio::test]
async fn a_direct_service_records_limits_without_a_builder() {
    let events = Arc::new(MemoryLimitEvents::new());
    let service = RepoService::new(
        RepoId::new("game"),
        Rules::from_toml("[meta]\ndefault = \"exclusive\"\n").unwrap(),
        Arc::new(MemoryMetadataStore::new()),
        Arc::new(MemoryObjectStore::new()),
        Arc::new(MemoryAuditStore::new()),
        pyn_core::RepoServiceContext {
            owner: UserId::new("owner"),
            owner_id: AccountId::new("uowner"),
            clock: Arc::new(ManualClock::new(
                Utc.with_ymd_and_hms(2030, 1, 1, 12, 0, 0).unwrap(),
            )),
            limit_events: events.clone(),
        },
        ServiceConfig {
            max_locks: 1,
            ..ServiceConfig::default()
        },
    );
    let caller = UserId::new("bob");
    service
        .checkout(&RepoPath::new("one").unwrap(), &caller, None)
        .await
        .unwrap();
    assert!(events.recorded().is_empty());
    let err = service
        .checkout(&RepoPath::new("two").unwrap(), &caller, None)
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::LockLimitReached { limit: 1 }));
    assert_eq!(
        events.recorded(),
        vec![LimitReached {
            owner: UserId::new("owner"),
            owner_id: AccountId::new("uowner"),
            kind: LimitKind::Locks,
            value: 1,
        }]
    );
}
