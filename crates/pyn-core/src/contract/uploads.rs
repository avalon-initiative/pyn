//! Uploaded objects are scoped to the repository that uploaded them, while the object store stays shared.

use std::sync::Arc;

use super::push::{Files, build, objects, set};
use super::{RULES, Store, blob, harness, harness_on, other_repo, path, who};
use crate::AuditStore as _;
use crate::memory::MemoryObjectStore;
use crate::merge::{FastForward, MergeRequest, Resolution};
use crate::{
    ChunkedStore, FileMode, Keying, ObjectId, ObjectStore, PynError, RepoId, RepoService, Rules,
    ServiceConfig,
};

async fn chunk(svc: &RepoService, bytes: &'static [u8]) -> crate::Result<ObjectId> {
    svc.upload(
        Box::pin(std::io::Cursor::new(bytes)),
        Keying::Raw,
        None,
        bytes.len() as u64,
    )
    .await
}

pub async fn an_object_is_usable_only_by_the_repository_that_uploaded_it(store: Store) {
    let h = harness(store);
    let other = other_repo(&h);
    let a = path("Source/a.cpp");
    let c = blob(&h, "shared bytes").await;

    for err in [
        other
            .svc
            .checkin(&who("alice"), &a, c, None, "x".into())
            .await
            .unwrap_err(),
        other
            .svc
            .checkin_changeset(
                &who("alice"),
                vec![crate::ChangeEntry {
                    path: a.clone(),
                    content: c,
                    file_mode: FileMode::Regular,
                    base: None,
                }],
                "x".into(),
            )
            .await
            .unwrap_err(),
    ] {
        assert!(matches!(err, PynError::ObjectMissing(_)), "{err}");
    }
    assert!(other.svc.head(&a).await.unwrap().is_none());

    h.svc
        .checkin(&who("alice"), &a, c, None, "x".into())
        .await
        .unwrap();
    let read = other.svc.read_blob(&c, None).await.err().unwrap();
    assert!(matches!(read, PynError::ObjectNotFound(_)), "{read}");
}

pub async fn uploads_are_reported_missing_per_repository_and_dedup_in_the_store(store: Store) {
    let h = harness(store);
    let other = other_repo(&h);
    let c = blob(&h, "same bytes").await;
    let ghost = ObjectId::from_hex(pyn_object::Algorithm::Sha256, &"00".repeat(32)).unwrap();

    assert!(h.svc.missing_objects(&[c]).await.unwrap().is_empty());
    let from_other = other.svc.missing_objects(&[c, ghost]).await.unwrap();
    assert_eq!(from_other, vec![c, ghost]);
    assert_eq!(h.svc.missing_objects(&[ghost]).await.unwrap(), vec![ghost]);

    let again = other.svc.upload_blob(b"same bytes".to_vec()).await.unwrap();
    assert_eq!(again, c);
    assert!(other.svc.missing_objects(&[c]).await.unwrap().is_empty());
    other
        .svc
        .checkin(&who("alice"), &path("Source/a.cpp"), c, None, "x".into())
        .await
        .unwrap();

    let third = harness_on(
        h.store.clone(),
        h.objects.clone(),
        RepoId::new("third"),
        RULES,
    );
    assert_eq!(third.svc.missing_objects(&[c]).await.unwrap(), vec![c]);
}

pub async fn a_push_needs_its_blobs_uploaded_to_the_repository(store: Store) {
    let h = harness(store);
    let other = other_repo(&h);
    let c = blob(&h, "pushed").await;
    let mut files = Files::new();
    set(&mut files, "Source/a.cpp", c);
    let commit = build(&files, None, "one", 1);

    let err = other
        .svc
        .push_commits(&who("alice"), "main", objects(&[&commit]))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::ObjectMissing(_)), "{err}");
    assert!(other.svc.head_of("main").await.unwrap().is_none());

    other.svc.upload_blob(b"pushed".to_vec()).await.unwrap();
    other
        .svc
        .push_commits(&who("alice"), "main", objects(&[&commit]))
        .await
        .unwrap();
}

pub async fn a_merge_resolution_needs_content_the_repository_knows(store: Store) {
    let h = harness(store);
    let other = other_repo(&h);
    let bin = path("Source/img.bin");
    let put = |branch: &'static str, bytes: Vec<u8>, base: Option<u64>| {
        let (svc, bin) = (h.svc.clone(), bin.clone());
        async move {
            let c = svc.upload_blob(bytes).await.unwrap();
            let base = base.map(crate::RevisionId);
            svc.checkin_on(
                branch,
                &who("alice"),
                &bin,
                c,
                FileMode::Regular,
                base,
                "b".into(),
            )
            .await
            .unwrap()
        }
    };
    put("main", vec![0, 1, 2, 3], None).await;
    h.svc
        .create_branch(&who("alice"), "feature", Some("main"))
        .await
        .unwrap();
    put("feature", vec![0, 1, 2, 9], Some(1)).await;
    put("main", vec![0, 7, 2, 3], Some(1)).await;

    let request = |content| MergeRequest {
        source: "feature".into(),
        target: Some("main".into()),
        fast_forward: FastForward::Allow,
        message: None,
        expect_target: None,
        resolutions: vec![Resolution {
            path: bin.clone(),
            content,
            file_mode: FileMode::Regular,
        }],
    };
    let foreign = other.svc.upload_blob(vec![5, 5, 5]).await.unwrap();
    let err = h
        .svc
        .merge(&who("alice"), request(foreign))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::ObjectMissing(_)), "{err}");

    let own = h.svc.upload_blob(vec![5, 5, 5]).await.unwrap();
    assert_eq!(own, foreign);
    assert!(h.svc.merge(&who("alice"), request(own)).await.is_ok());
}

pub async fn assembling_needs_chunks_the_repository_uploaded(store: Store) {
    let shared: Arc<dyn ObjectStore> =
        Arc::new(ChunkedStore::new(Arc::new(MemoryObjectStore::new())));
    let make = |repo: &str| {
        let h = harness(store.clone());
        RepoService::new(
            RepoId::new(repo),
            Rules::from_toml(RULES).unwrap(),
            store.clone(),
            shared.clone(),
            h.audit.clone(),
            h.clock.clone(),
            ServiceConfig::default(),
        )
    };
    let (mine, theirs) = (make("game"), make("other"));
    let c1 = chunk(&mine, b"first half ").await.unwrap();
    let c2 = chunk(&mine, b"second half").await.unwrap();

    let err = theirs.assemble(&[c1, c2], None).await.unwrap_err();
    assert!(matches!(err, PynError::ObjectMissing(_)), "{err}");
    let id = mine.assemble(&[c1, c2], None).await.unwrap();
    assert!(mine.missing_objects(&[id]).await.unwrap().is_empty());
    assert_eq!(
        theirs.missing_objects(&[id, c1]).await.unwrap(),
        vec![id, c1]
    );

    chunk(&theirs, b"first half ").await.unwrap();
    chunk(&theirs, b"second half").await.unwrap();
    assert_eq!(theirs.assemble(&[c1, c2], None).await.unwrap(), id);
}

pub async fn unreferenced_uploads_are_capped_per_repository(store: Store) {
    let h = harness(store.clone());
    let svc = RepoService::new(
        RepoId::new("capped"),
        Rules::from_toml(RULES).unwrap(),
        store.clone(),
        h.objects.clone(),
        h.audit.clone(),
        h.clock.clone(),
        ServiceConfig::default(),
    )
    .with_unreferenced_cap(10);
    let id = RepoId::new("capped");
    let a = svc.upload_blob(b"abcd".to_vec()).await.unwrap();
    assert_eq!(svc.upload_blob(b"abcd".to_vec()).await.unwrap(), a);
    assert_eq!(store.unreferenced_bytes(&id).await.unwrap(), 4);

    let err = svc.upload_blob(b"efghijkl".to_vec()).await.unwrap_err();
    assert!(
        matches!(err, PynError::StorageLimitReached { limit: 10, .. }),
        "{err}"
    );
    svc.upload_blob(b"efghi".to_vec()).await.unwrap();
    assert_eq!(store.unreferenced_bytes(&id).await.unwrap(), 9);

    svc.checkin(&who("alice"), &path("Source/a.cpp"), a, None, "x".into())
        .await
        .unwrap();
    assert_eq!(store.unreferenced_bytes(&id).await.unwrap(), 5);
    svc.upload_blob(b"jklmn".to_vec()).await.unwrap();
}

fn capped_service(h: &super::Harness, store: &Store, repo: &str, cap: u64) -> Arc<RepoService> {
    Arc::new(
        RepoService::new(
            RepoId::new(repo),
            Rules::from_toml(RULES).unwrap(),
            store.clone(),
            h.objects.clone(),
            h.audit.clone(),
            h.clock.clone(),
            ServiceConfig::default(),
        )
        .with_unreferenced_cap(cap),
    )
}

pub async fn upload_reservations_hold_the_budget_until_released_or_expired(store: Store) {
    let repo = RepoId::new("budget");
    let now = chrono::Utc::now();
    let a = store.reserve_upload(&repo, 6, 10, now, 60).await.unwrap();
    let err = store
        .reserve_upload(&repo, 6, 10, now, 60)
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            PynError::StorageLimitReached {
                limit: 10,
                used: 6,
                ..
            }
        ),
        "{err}"
    );
    store.release_upload(&repo, a).await.unwrap();
    store.release_upload(&repo, a).await.unwrap();
    let b = store.reserve_upload(&repo, 6, 10, now, 60).await.unwrap();
    assert!(store.reserve_upload(&repo, 5, 10, now, 60).await.is_err());
    store.reserve_upload(&repo, 4, 10, now, 60).await.unwrap();

    let later = now + chrono::Duration::seconds(61);
    store
        .reserve_upload(&repo, 10, 10, later, 60)
        .await
        .unwrap();
    store.release_upload(&repo, b).await.unwrap();
    let other = RepoId::new("budget-other");
    store.reserve_upload(&other, 10, 10, now, 60).await.unwrap();
    assert_eq!(store.unreferenced_bytes(&repo).await.unwrap(), 0);
}

pub async fn parallel_uploads_never_pass_the_unreferenced_budget(store: Store) {
    let h = harness(store.clone());
    let svc = capped_service(&h, &store, "parallel", 10);
    let tasks: Vec<_> = (0..8u8)
        .map(|n| {
            let svc = svc.clone();
            tokio::spawn(async move { svc.upload_blob(vec![b'a' + n; 4]).await })
        })
        .collect();
    let mut stored = 0;
    for task in tasks {
        match task.await.unwrap() {
            Ok(_) => stored += 1,
            Err(e) => assert!(matches!(e, PynError::StorageLimitReached { .. }), "{e}"),
        }
    }
    assert_eq!(stored, 2);
    let id = RepoId::new("parallel");
    assert_eq!(store.unreferenced_bytes(&id).await.unwrap(), 8);
    svc.upload_blob(b"zz".to_vec()).await.unwrap();
}

pub async fn a_rejected_upload_removes_only_the_bytes_it_wrote(store: Store) {
    let h = harness(store.clone());
    let svc = capped_service(&h, &store, "rejected", 1000);
    let reader = |b: &'static [u8]| -> crate::ObjectReader { Box::pin(std::io::Cursor::new(b)) };

    let fresh = svc
        .upload(reader(b"fresh"), Keying::Raw, None, 3)
        .await
        .unwrap_err();
    assert!(
        matches!(
            fresh,
            PynError::LengthMismatch {
                declared: 3,
                actual: 5
            }
        ),
        "{fresh}"
    );
    let key = {
        let mut hasher = crate::KeyHasher::new(Keying::Raw);
        hasher.update(b"fresh").unwrap();
        hasher.finish().unwrap()
    };
    assert!(!h.objects.exists(&key).await.unwrap());

    let kept = svc
        .upload(reader(b"fresh"), Keying::Raw, None, 5)
        .await
        .unwrap();
    assert_eq!(kept, key);
    svc.upload(reader(b"fresh"), Keying::Raw, None, 3)
        .await
        .unwrap_err();
    assert!(h.objects.exists(&key).await.unwrap());
    assert!(svc.missing_objects(&[key]).await.unwrap().is_empty());
}

/// Runs `then` once after the first `put_stream_tracked`, standing in for another repository winning the race.
struct RacingStore {
    inner: Arc<MemoryObjectStore>,
    then: tokio::sync::Mutex<Option<Arc<RepoService>>>,
}

#[async_trait::async_trait]
impl ObjectStore for RacingStore {
    async fn put_stream(
        &self,
        reader: crate::ObjectReader,
        keying: Keying,
        max: Option<u64>,
    ) -> crate::Result<ObjectId> {
        self.inner.put_stream(reader, keying, max).await
    }

    async fn put_stream_tracked(
        &self,
        reader: crate::ObjectReader,
        keying: Keying,
        max: Option<u64>,
    ) -> crate::Result<crate::Stored> {
        let stored = self.inner.put_stream_tracked(reader, keying, max).await?;
        if let Some(other) = self.then.lock().await.take() {
            other.upload_blob(b"racing bytes".to_vec()).await?;
        }
        Ok(stored)
    }

    async fn read(
        &self,
        hash: &ObjectId,
        range: Option<std::ops::Range<u64>>,
    ) -> crate::Result<Option<crate::ObjectRead>> {
        self.inner.read(hash, range).await
    }

    async fn exists(&self, hash: &ObjectId) -> crate::Result<bool> {
        self.inner.exists(hash).await
    }

    async fn size(&self, hash: &ObjectId) -> crate::Result<Option<u64>> {
        self.inner.size(hash).await
    }

    async fn delete(&self, hash: &ObjectId) -> crate::Result<bool> {
        self.inner.delete(hash).await
    }
}

pub async fn a_failed_upload_keeps_an_object_another_repository_recorded(store: Store) {
    let h = harness(store.clone());
    let other = other_repo(&h);
    let racing = Arc::new(RacingStore {
        inner: h.objects.clone(),
        then: tokio::sync::Mutex::new(Some(other.svc.clone())),
    });
    let failing = RepoService::new(
        RepoId::new("failing"),
        Rules::from_toml(RULES).unwrap(),
        store.clone(),
        racing,
        h.audit.clone(),
        h.clock.clone(),
        ServiceConfig::default(),
    );

    let err = failing
        .upload(
            Box::pin(std::io::Cursor::new(b"racing bytes".to_vec())),
            Keying::Blob { len: 12 },
            None,
            3,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::LengthMismatch { .. }), "{err}");

    let id = pyn_object::hash_object(
        pyn_object::Algorithm::Sha256,
        pyn_object::Kind::Blob,
        b"racing bytes",
    )
    .unwrap();
    assert!(h.objects.exists(&id).await.unwrap());
    assert!(other.svc.missing_objects(&[id]).await.unwrap().is_empty());
    other
        .svc
        .checkin(&who("alice"), &path("Source/a.cpp"), id, None, "x".into())
        .await
        .unwrap();
    assert!(other.svc.read_blob(&id, None).await.is_ok());
}

pub async fn an_object_is_reported_referenced_once_any_repository_recorded_it(store: Store) {
    let id = ObjectId::from_hex(pyn_object::Algorithm::Sha256, &"11".repeat(32)).unwrap();
    assert!(!store.object_is_recorded(&id).await.unwrap());
    store
        .record_upload(&RepoId::new("elsewhere"), &id, 1, 100)
        .await
        .unwrap();
    assert!(store.object_is_recorded(&id).await.unwrap());
}

pub async fn a_policy_file_over_the_size_bound_is_refused(store: Store) {
    let h = harness(store);
    let policy = path(".pyn/pyn.toml");
    let mut text = String::from("[meta]\ndefault = \"shared\"\n#");
    text.push_str(&"x".repeat(crate::rules::MAX_POLICY_BYTES as usize));
    let big = h.svc.upload_blob(text.into_bytes()).await.unwrap();
    let err = h
        .svc
        .checkin(&who("alice"), &policy, big, None, "big".into())
        .await
        .unwrap_err();
    assert!(
        matches!(err, PynError::InvalidRules(ref m) if m.contains("larger than")),
        "{err}"
    );
    assert!(
        h.svc
            .preview_policy(
                &who("alice"),
                &"#".repeat(crate::rules::MAX_POLICY_BYTES as usize + 1)
            )
            .await
            .is_err()
    );
}

pub async fn audit_details_carry_a_bounded_message(store: Store) {
    let h = harness(store);
    let message = "m".repeat(60_000);
    let entries: Vec<_> = {
        let mut out = Vec::new();
        for n in 0..5 {
            out.push(crate::ChangeEntry {
                path: path(&format!("Source/f{n}.cpp")),
                content: blob(&h, &format!("body {n}")).await,
                file_mode: FileMode::Regular,
                base: None,
            });
        }
        out
    };
    h.svc
        .checkin_changeset(&who("alice"), entries, message)
        .await
        .unwrap();
    let events = h
        .audit
        .list(
            &crate::AuditScope::Repo(RepoId::new("game")),
            &crate::AuditQuery {
                limit: 100,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let checkins: Vec<_> = events
        .iter()
        .filter(|e| e.action == crate::AuditAction::Checkin)
        .collect();
    assert_eq!(checkins.len(), 5);
    assert!(
        checkins.iter().all(|e| e.detail.len() < 400),
        "{checkins:?}"
    );
}
