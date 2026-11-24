//! Repository creation can make the first commit hold the starter policy.

use std::sync::Arc;

use chrono::{TimeZone, Utc};

use super::{Access, Store};
use crate::memory::{MemoryAuditStore, MemoryObjectStore};
use crate::rules::starter_policy;
use crate::{
    AccessService, Credential, Identity, ManualClock, MemoryLimitEvents, Mode, RepoPath,
    Repositories, Rules, UserId,
};

fn alice() -> Identity {
    Identity {
        user: UserId::new("alice"),
        credential: Credential::Session,
    }
}

async fn repositories(meta: Store, access_store: Access) -> (Repositories, Arc<AccessService>) {
    let at = Utc.with_ymd_and_hms(2030, 1, 1, 12, 0, 0).unwrap();
    let clock = Arc::new(ManualClock::new(at));
    access_store
        .ensure_user(&UserId::new("alice"), at)
        .await
        .unwrap();
    let access = Arc::new(
        AccessService::new(
            access_store,
            clock.clone(),
            Arc::new(MemoryLimitEvents::new()),
        )
        .with_registry(meta.clone()),
    );
    let repos = Repositories::new(
        meta,
        Arc::new(MemoryObjectStore::new()),
        Arc::new(MemoryAuditStore::new()),
        access.clone(),
        clock,
        Rules::empty(),
    );
    (repos, access)
}

pub async fn a_starter_creation_commits_the_policy_file_first(meta: Store, access_store: Access) {
    let (repos, _) = repositories(meta, access_store).await;
    let who = alice();
    for (name, default) in [("shared", Mode::Shared), ("locked", Mode::Exclusive)] {
        repos
            .create_with_starter(&who, &who.user, name, None, None, Some(default))
            .await
            .unwrap();
        let svc = repos.open("alice", name).await.unwrap().service;
        let policy = svc.active_policy().await.unwrap();
        assert!(policy.from_file && policy.commit.is_some(), "{name}");
        assert_eq!(
            policy
                .rules
                .mode_for(&RepoPath::new("notes/none.unknown").unwrap()),
            default
        );
        let history = svc
            .history(&RepoPath::new(".pyn/pyn.toml").unwrap())
            .await
            .unwrap();
        assert_eq!(history.len(), 1, "{name}");
        assert_eq!(history[0].author, who.user);
        let text = starter_policy(default).into_bytes();
        let stored = svc.upload_blob(text).await.unwrap();
        assert_eq!(history[0].content, stored, "{name}: the starter text");
    }
}

pub async fn creation_without_a_starter_leaves_the_repository_empty(
    meta: Store,
    access_store: Access,
) {
    let (repos, _) = repositories(meta, access_store).await;
    let who = alice();
    repos
        .create(&who, &who.user, "empty", None, None)
        .await
        .unwrap();
    let svc = repos.open("alice", "empty").await.unwrap().service;
    let policy = svc.active_policy().await.unwrap();
    assert!(!policy.from_file && policy.commit.is_none());
    assert!(
        svc.history(&RepoPath::new(".pyn/pyn.toml").unwrap())
            .await
            .unwrap()
            .is_empty()
    );
}
