//! A scaled clock expires a lease in real seconds: the lock frees, another user takes it, and the old holder is refused.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore,
};
use pyn_core::{
    AccessConfig, AccessService, AccessStore, Credential, Identity, RegistrationMode, RepoPath,
    Repositories, Rules, ScaledClock, UserId,
};

fn who(name: &str) -> Identity {
    Identity {
        user: UserId::new(name),
        credential: Credential::Session,
    }
}

#[tokio::test]
async fn a_scaled_clock_expires_a_lease_in_seconds() {
    // 8 hour default lease passes in about 0.3 s
    let clock = Arc::new(ScaledClock::new(100_000.0));
    let meta = Arc::new(MemoryMetadataStore::new());
    let store = Arc::new(MemoryAccessStore::new());
    let audit = Arc::new(MemoryAuditStore::new());
    let access = Arc::new(
        AccessService::new(
            store.clone(),
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
    for user in ["alice", "bob"] {
        store
            .ensure_user(&UserId::new(user), Utc::now())
            .await
            .unwrap();
    }
    let repos = Repositories::new(
        meta,
        Arc::new(MemoryObjectStore::new()),
        audit,
        access.clone(),
        clock,
        Rules::empty(),
    );
    let (alice, bob) = (who("alice"), who("bob"));
    repos
        .create(&alice, &alice.user, "game", None, None)
        .await
        .unwrap();
    let opened = repos.open("alice", "game").await.unwrap();
    let svc = opened.service;
    let path = RepoPath::try_from("Content/a.umap".to_string()).unwrap();

    svc.checkout(&path, &alice.user, None).await.unwrap();
    assert!(svc.checkout(&path, &bob.user, None).await.is_err());

    tokio::time::sleep(Duration::from_secs(2)).await;

    let principal = access
        .principal(&opened.record.id, &alice.user)
        .await
        .unwrap();
    let content = svc.upload_blob(b"v1".to_vec()).await.unwrap();
    assert!(
        svc.checkin(&principal, &path, content, None, "late".into())
            .await
            .is_err()
    );
    svc.checkout(&path, &bob.user, None).await.unwrap();
}
