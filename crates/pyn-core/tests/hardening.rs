//! Bounds on stored-object loads, concurrent text merges and audit detail.

use std::collections::BTreeSet;
use std::ops::Range;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore,
};
use pyn_core::merge::{FastForward, MergeRequest};
use pyn_core::{
    AccessService, AuditAction, AuditQuery, AuditScope, AuditStore, Credential, FileMode, Identity,
    Keying, ManualClock, ObjectId, ObjectRead, ObjectReader, ObjectStore, Permission, Principal,
    PynError, RepoId, RepoPath, RepoService, Rules, ServiceConfig, UserId,
};

const RULES: &str = "[meta]\ndefault = \"shared\"\n";

/// An object store whose reads fail the test: a bounded load must refuse on the length alone.
struct NoReads(MemoryObjectStore);

#[async_trait]
impl ObjectStore for NoReads {
    async fn put_stream(
        &self,
        reader: ObjectReader,
        keying: Keying,
        max: Option<u64>,
    ) -> pyn_core::Result<ObjectId> {
        self.0.put_stream(reader, keying, max).await
    }

    async fn read(
        &self,
        hash: &ObjectId,
        _range: Option<Range<u64>>,
    ) -> pyn_core::Result<Option<ObjectRead>> {
        Ok(self.0.size(hash).await?.map(|total| ObjectRead {
            reader: Box::pin(Unreadable),
            total,
            range: 0..total,
        }))
    }

    async fn exists(&self, hash: &ObjectId) -> pyn_core::Result<bool> {
        self.0.exists(hash).await
    }

    async fn size(&self, hash: &ObjectId) -> pyn_core::Result<Option<u64>> {
        self.0.size(hash).await
    }

    async fn delete(&self, hash: &ObjectId) -> pyn_core::Result<bool> {
        self.0.delete(hash).await
    }
}

struct Unreadable;

impl tokio::io::AsyncRead for Unreadable {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        _: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        panic!("the object was read");
    }
}

fn who(name: &str) -> Principal {
    Principal::unrestricted(UserId::new(name))
}

fn service(objects: Arc<dyn ObjectStore>) -> Arc<RepoService> {
    let clock = Arc::new(ManualClock::new(
        Utc.with_ymd_and_hms(2026, 10, 8, 9, 0, 0).unwrap(),
    ));
    Arc::new(RepoService::new(
        RepoId::new("game"),
        Rules::from_toml(RULES).unwrap(),
        Arc::new(MemoryMetadataStore::new()),
        objects,
        Arc::new(MemoryAuditStore::new()),
        clock,
        ServiceConfig::default(),
    ))
}

#[tokio::test]
async fn get_limited_refuses_a_long_object_without_reading_it() {
    let store = NoReads(MemoryObjectStore::new());
    let id = store.put(vec![b'x'; 100]).await.unwrap();
    let err = store.get_limited(&id, 99).await.unwrap_err();
    assert!(
        matches!(err, PynError::ObjectTooLarge { limit: 99 }),
        "{err}"
    );
    let plain = MemoryObjectStore::new();
    let id = plain.put(vec![b'x'; 100]).await.unwrap();
    assert_eq!(
        plain.get_limited(&id, 100).await.unwrap().unwrap().len(),
        100
    );
    assert!(plain.get_limited(&id, 99).await.is_err());
}

#[tokio::test]
async fn an_oversize_policy_is_refused_without_reading_it() {
    let svc = service(Arc::new(NoReads(MemoryObjectStore::new())));
    let mut text = String::from(RULES);
    text.push('#');
    text.push_str(&"x".repeat(1 << 20));
    let id = svc.upload_blob(text.into_bytes()).await.unwrap();
    let policy = RepoPath::new(".pyn/pyn.toml").unwrap();
    let err = svc
        .checkin(&who("alice"), &policy, id, None, "policy".into())
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::InvalidRules(_)), "{err}");
}

#[tokio::test]
async fn text_merges_wait_for_a_free_slot() {
    let slots = Arc::new(tokio::sync::Semaphore::new(0));
    let clock = Arc::new(ManualClock::new(
        Utc.with_ymd_and_hms(2026, 10, 8, 9, 0, 0).unwrap(),
    ));
    let svc = Arc::new(
        RepoService::new(
            RepoId::new("game"),
            Rules::from_toml(RULES).unwrap(),
            Arc::new(MemoryMetadataStore::new()),
            Arc::new(MemoryObjectStore::new()),
            Arc::new(MemoryAuditStore::new()),
            clock,
            ServiceConfig::default(),
        )
        .with_merge_slots(slots.clone()),
    );
    let (alice, a) = (who("alice"), RepoPath::new("Source/a.cpp").unwrap());
    let put = |text: &'static str, base: Option<u64>, branch: &'static str| {
        let (svc, alice, a) = (svc.clone(), alice.clone(), a.clone());
        async move {
            let c = svc.upload_blob(text.as_bytes().to_vec()).await.unwrap();
            svc.checkin_on(
                branch,
                &alice,
                &a,
                c,
                FileMode::Regular,
                base.map(pyn_core::RevisionId),
                "m".into(),
            )
            .await
            .unwrap();
        }
    };
    put("1\n2\n3\n", None, "main").await;
    svc.create_branch(&alice, "feature", Some("main"))
        .await
        .unwrap();
    put("1\n2\n3\nfeature\n", Some(1), "feature").await;
    put("ours\n2\n3\n", Some(1), "main").await;

    let request = MergeRequest {
        source: "feature".into(),
        target: Some("main".into()),
        fast_forward: FastForward::Allow,
        message: None,
        expect_target: None,
        resolutions: Vec::new(),
    };
    let waiting = tokio::time::timeout(Duration::from_millis(200), svc.merge_preview(&request));
    assert!(waiting.await.is_err(), "the merge ran without a slot");
    slots.add_permits(1);
    let report = svc.merge_preview(&request).await.unwrap();
    assert!(report.conflicts.is_empty());
}

#[tokio::test]
async fn a_token_on_many_repositories_writes_a_bounded_audit_detail() {
    let clock = Arc::new(ManualClock::new(
        Utc.with_ymd_and_hms(2026, 10, 8, 9, 0, 0).unwrap(),
    ));
    let audit = Arc::new(MemoryAuditStore::new());
    let svc =
        AccessService::new(Arc::new(MemoryAccessStore::new()), clock).with_audit(audit.clone());
    let root = UserId::new("root");
    svc.admin_for_tests(&root).await.unwrap();
    let repos: Vec<RepoId> = (0..40).map(|n| RepoId::new(format!("r{n}"))).collect();
    for repo in &repos {
        svc.add_creator(repo, &root).await.unwrap();
    }
    let actor = Identity {
        user: root,
        credential: Credential::Session,
    };
    let perms: BTreeSet<Permission> = [Permission::Read].into();
    svc.create_token(&actor, "ci", perms, repos.clone(), None)
        .await
        .unwrap();
    let events = audit
        .list(
            &AuditScope::Repo(repos[0].clone()),
            &AuditQuery {
                limit: 10,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let created = events
        .iter()
        .find(|e| e.action == AuditAction::TokenCreated)
        .unwrap();
    assert!(created.detail.len() < 400, "{}", created.detail);
    assert!(created.detail.contains("and 32 more"), "{}", created.detail);
}
