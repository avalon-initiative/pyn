//! Uploaded objects are scoped to the repository that uploaded them, while the object store stays shared.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use chrono::Utc;

use super::push::{Files, build, objects, set};
use super::{RULES, Store, blob, harness, harness_on, other_repo, path, who};
use crate::AuditStore as _;
use crate::memory::MemoryObjectStore;
use crate::merge::{FastForward, MergeRequest, Resolution};
use crate::{
    ChunkedStore, FileMode, Keying, MetadataStore, ObjectId, ObjectStore, PynError, RepoId,
    RepoPath, RepoRecord, RepoService, RepoUpdate, RevisionId, Rules, ServiceConfig, UserId,
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
            crate::RepoServiceContext {
                owner: crate::UserId::new("owner"),
                clock: h.clock.clone(),
                limit_events: Arc::new(crate::MemoryLimitEvents::new()),
            },
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
        crate::RepoServiceContext {
            owner: crate::UserId::new("owner"),
            clock: h.clock.clone(),
            limit_events: Arc::new(crate::MemoryLimitEvents::new()),
        },
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
            crate::RepoServiceContext {
                owner: crate::UserId::new("owner"),
                clock: h.clock.clone(),
                limit_events: Arc::new(crate::MemoryLimitEvents::new()),
            },
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
    // Seven of the eight uploads wait for the budget gate, which releases them only once the first
    // upload's bytes are recorded, before its separate release call; their single reserve attempts
    // therefore all land inside the window that would double-count an unconsumed reservation.
    let store: Store = BudgetGateStore::new(store, 7);
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
        crate::RepoServiceContext {
            owner: crate::UserId::new("owner"),
            clock: h.clock.clone(),
            limit_events: Arc::new(crate::MemoryLimitEvents::new()),
        },
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
    // No reservation backs this synthetic record; 0 is never a live reservation id (both stores
    // number them from 1), so consuming it removes nothing.
    store
        .record_upload(
            &RepoId::new("elsewhere"),
            &id,
            1,
            100,
            crate::UploadReservation(0),
        )
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

/// Holds the budget race open for `parallel_uploads_never_pass_the_unreferenced_budget`: the first
/// `record_upload` forwards to the inner store, then keeps the upload parked until every
/// competitor's single `reserve_upload` attempt has completed. The park makes the race
/// deterministic: with a budget that counts an upload's bytes in both `unreferenced` and its
/// still-held reservation, every competitor is refused; with a budget that counts each byte once,
/// the first competitor still fits.
struct BudgetGateStore {
    inner: Store,
    /// Set by the first reserve; every later reserve belongs to a competitor and is held until the
    /// first record has landed, so no competitor can reserve before the window opens.
    first_reserve: AtomicBool,
    /// Claimed by the first `record_upload` before it forwards, so exactly one upload gates; the
    /// done flag is set only after the forward, so the held reserves wait for the landing.
    first_record: AtomicBool,
    /// Set once the first `record_upload` has reached the inner store.
    first_record_done: AtomicBool,
    /// Completed competitor reserve attempts (every reserve but the first).
    competitors: AtomicUsize,
    /// How many competitor attempts the gate waits for.
    want: usize,
}

impl BudgetGateStore {
    fn new(inner: Store, want: usize) -> Arc<Self> {
        Arc::new(Self {
            inner,
            first_reserve: AtomicBool::new(false),
            first_record: AtomicBool::new(false),
            first_record_done: AtomicBool::new(false),
            competitors: AtomicUsize::new(0),
            want,
        })
    }

    /// Waits until `ok` holds or 60s pass, then fails loudly instead of hanging. The contract
    /// feature builds without tokio's time driver, so the deadline uses a monotonic clock and the
    /// wait yields instead of sleeping.
    async fn wait_for(&self, ok: impl Fn(&Self) -> bool, what: &str) {
        let deadline = Instant::now() + Duration::from_secs(60);
        while !ok(self) {
            if Instant::now() >= deadline {
                panic!("budget gate ({what}) never opened within 60s");
            }
            tokio::task::yield_now().await;
        }
    }
}

#[async_trait::async_trait]
impl MetadataStore for BudgetGateStore {
    async fn acquire_lock(
        &self,
        repo: &RepoId,
        path: &RepoPath,
        owner: &UserId,
        now: chrono::DateTime<Utc>,
        expires_at: chrono::DateTime<Utc>,
        max_locks: u32,
    ) -> crate::Result<crate::Lock> {
        self.inner
            .acquire_lock(repo, path, owner, now, expires_at, max_locks)
            .await
    }

    async fn release_lock(
        &self,
        repo: &RepoId,
        path: &RepoPath,
        owner: &UserId,
        now: chrono::DateTime<Utc>,
    ) -> crate::Result<()> {
        self.inner.release_lock(repo, path, owner, now).await
    }

    async fn force_release_lock(
        &self,
        repo: &RepoId,
        path: &RepoPath,
        now: chrono::DateTime<Utc>,
    ) -> crate::Result<Option<crate::Lock>> {
        self.inner.force_release_lock(repo, path, now).await
    }

    async fn get_lock(
        &self,
        repo: &RepoId,
        path: &RepoPath,
        now: chrono::DateTime<Utc>,
    ) -> crate::Result<Option<crate::Lock>> {
        self.inner.get_lock(repo, path, now).await
    }

    async fn list_locks(
        &self,
        repo: &RepoId,
        now: chrono::DateTime<Utc>,
    ) -> crate::Result<Vec<crate::Lock>> {
        self.inner.list_locks(repo, now).await
    }

    async fn list_locks_of(
        &self,
        owner: &UserId,
        now: chrono::DateTime<Utc>,
    ) -> crate::Result<Vec<(RepoId, crate::Lock)>> {
        self.inner.list_locks_of(owner, now).await
    }

    async fn head_revision(
        &self,
        repo: &RepoId,
        path: &RepoPath,
    ) -> crate::Result<Option<crate::Revision>> {
        self.inner.head_revision(repo, path).await
    }

    async fn commit_changeset_on(
        &self,
        repo: &RepoId,
        branch: &str,
        items: Vec<crate::ChangesetItem>,
        now: chrono::DateTime<Utc>,
        cap: Option<crate::StorageCap>,
    ) -> crate::Result<Vec<crate::Revision>> {
        self.inner
            .commit_changeset_on(repo, branch, items, now, cap)
            .await
    }

    async fn push_commits_on(
        &self,
        repo: &RepoId,
        branch: &str,
        commits: Vec<crate::PushedCommit>,
        pusher: &UserId,
        now: chrono::DateTime<Utc>,
        cap: Option<crate::StorageCap>,
    ) -> crate::Result<Vec<crate::Revision>> {
        self.inner
            .push_commits_on(repo, branch, commits, pusher, now, cap)
            .await
    }

    async fn commit_merge_on(
        &self,
        repo: &RepoId,
        branch: &str,
        merge: crate::merge::MergeWrite,
        now: chrono::DateTime<Utc>,
        cap: Option<crate::StorageCap>,
    ) -> crate::Result<crate::merge::MergeWritten> {
        self.inner
            .commit_merge_on(repo, branch, merge, now, cap)
            .await
    }

    async fn fast_forward_branch(
        &self,
        repo: &RepoId,
        branch: &str,
        from: Option<&ObjectId>,
        to: &ObjectId,
    ) -> crate::Result<()> {
        self.inner.fast_forward_branch(repo, branch, from, to).await
    }

    async fn merge_bases(
        &self,
        repo: &RepoId,
        a: &ObjectId,
        b: &ObjectId,
    ) -> crate::Result<Vec<ObjectId>> {
        self.inner.merge_bases(repo, a, b).await
    }

    async fn list_branches(&self, repo: &RepoId) -> crate::Result<Vec<crate::Branch>> {
        self.inner.list_branches(repo).await
    }

    async fn get_branch(&self, repo: &RepoId, name: &str) -> crate::Result<Option<crate::Branch>> {
        self.inner.get_branch(repo, name).await
    }

    async fn create_branch(
        &self,
        repo: &RepoId,
        name: &str,
        at: &ObjectId,
        by: &UserId,
        now: chrono::DateTime<Utc>,
    ) -> crate::Result<crate::Branch> {
        self.inner.create_branch(repo, name, at, by, now).await
    }

    async fn delete_branch(
        &self,
        repo: &RepoId,
        name: &str,
        expected: Option<&ObjectId>,
    ) -> crate::Result<()> {
        self.inner.delete_branch(repo, name, expected).await
    }

    async fn rename_branch(
        &self,
        repo: &RepoId,
        from: &str,
        to: &str,
    ) -> crate::Result<crate::Branch> {
        self.inner.rename_branch(repo, from, to).await
    }

    async fn get_commit(
        &self,
        repo: &RepoId,
        id: &ObjectId,
    ) -> crate::Result<Option<crate::Commit>> {
        self.inner.get_commit(repo, id).await
    }

    async fn commits_by_prefix(
        &self,
        repo: &RepoId,
        prefix: &str,
        limit: usize,
    ) -> crate::Result<Vec<ObjectId>> {
        self.inner.commits_by_prefix(repo, prefix, limit).await
    }

    async fn commits(
        &self,
        repo: &RepoId,
        tip: Option<&ObjectId>,
        before: Option<&ObjectId>,
        limit: usize,
    ) -> crate::Result<Vec<crate::Commit>> {
        self.inner.commits(repo, tip, before, limit).await
    }

    async fn commits_matching(
        &self,
        repo: &RepoId,
        tip: Option<&ObjectId>,
        before: Option<&ObjectId>,
        filter: &crate::CommitFilter,
        limit: usize,
    ) -> crate::Result<Vec<crate::Commit>> {
        self.inner
            .commits_matching(repo, tip, before, filter, limit)
            .await
    }

    async fn missing_commits(
        &self,
        repo: &RepoId,
        tips: &[ObjectId],
        haves: &[ObjectId],
        after: Option<&ObjectId>,
        limit: usize,
        max_examined: usize,
    ) -> crate::Result<Vec<crate::Commit>> {
        self.inner
            .missing_commits(repo, tips, haves, after, limit, max_examined)
            .await
    }

    async fn is_ancestor(
        &self,
        repo: &RepoId,
        ancestor: &ObjectId,
        descendant: &ObjectId,
    ) -> crate::Result<bool> {
        self.inner.is_ancestor(repo, ancestor, descendant).await
    }

    async fn revision_at(
        &self,
        repo: &RepoId,
        commit: &ObjectId,
        path: &RepoPath,
    ) -> crate::Result<Option<crate::Revision>> {
        self.inner.revision_at(repo, commit, path).await
    }

    async fn commit_pointers(
        &self,
        repo: &RepoId,
        commit: &ObjectId,
    ) -> crate::Result<Vec<crate::Revision>> {
        self.inner.commit_pointers(repo, commit).await
    }

    async fn has_content(&self, repo: &RepoId, content: &ObjectId) -> crate::Result<bool> {
        self.inner.has_content(repo, content).await
    }

    async fn record_upload(
        &self,
        repo: &RepoId,
        id: &ObjectId,
        size: u64,
        cap: u64,
        reservation: crate::UploadReservation,
    ) -> crate::Result<()> {
        if self.first_record.swap(true, Ordering::SeqCst) {
            return self
                .inner
                .record_upload(repo, id, size, cap, reservation)
                .await;
        }
        // Forward first: the bytes land in `unreferenced` before the gate opens, so every
        // competitor's single reserve attempt happens between this record and this upload's
        // release — the window a double-counting budget gets wrong.
        let out = self
            .inner
            .record_upload(repo, id, size, cap, reservation)
            .await;
        self.first_record_done.store(true, Ordering::SeqCst);
        // Keep the upload parked here until every competitor's single reserve attempt has
        // completed; a budget that counts bytes once admits one of the seven here and refuses
        // the rest for real.
        self.wait_for(
            |g| g.competitors.load(Ordering::SeqCst) >= g.want,
            "record gate",
        )
        .await;
        out
    }

    async fn record_assembled(
        &self,
        repo: &RepoId,
        id: &ObjectId,
        size: u64,
        chunks: &[ObjectId],
        cap: u64,
    ) -> crate::Result<()> {
        self.inner
            .record_assembled(repo, id, size, chunks, cap)
            .await
    }

    async fn object_is_recorded(&self, id: &ObjectId) -> crate::Result<bool> {
        self.inner.object_is_recorded(id).await
    }

    async fn unknown_objects(
        &self,
        repo: &RepoId,
        ids: &[ObjectId],
    ) -> crate::Result<Vec<ObjectId>> {
        self.inner.unknown_objects(repo, ids).await
    }

    async fn reserve_upload(
        &self,
        repo: &RepoId,
        bytes: u64,
        cap: u64,
        now: chrono::DateTime<chrono::Utc>,
        ttl_secs: i64,
    ) -> crate::Result<crate::UploadReservation> {
        let competitor = self.first_reserve.swap(true, Ordering::SeqCst);
        if competitor && !self.first_record_done.load(Ordering::SeqCst) {
            self.wait_for(
                |g| g.first_record_done.load(Ordering::SeqCst),
                "reserve hold",
            )
            .await;
        }
        let out = self
            .inner
            .reserve_upload(repo, bytes, cap, now, ttl_secs)
            .await;
        if competitor {
            self.competitors.fetch_add(1, Ordering::SeqCst);
        }
        out
    }

    async fn release_upload(
        &self,
        repo: &RepoId,
        reservation: crate::UploadReservation,
    ) -> crate::Result<()> {
        self.inner.release_upload(repo, reservation).await
    }

    async fn unreferenced_bytes(&self, repo: &RepoId) -> crate::Result<u64> {
        self.inner.unreferenced_bytes(repo).await
    }

    async fn get_tree(&self, repo: &RepoId, id: &ObjectId) -> crate::Result<Option<Vec<u8>>> {
        self.inner.get_tree(repo, id).await
    }

    async fn snapshot(
        &self,
        repo: &RepoId,
        commit: &ObjectId,
    ) -> crate::Result<Vec<crate::Revision>> {
        self.inner.snapshot(repo, commit).await
    }

    async fn get_revision(
        &self,
        repo: &RepoId,
        path: &RepoPath,
        id: RevisionId,
    ) -> crate::Result<Option<crate::Revision>> {
        self.inner.get_revision(repo, path, id).await
    }

    async fn snapshot_page(
        &self,
        repo: &RepoId,
        commit: &ObjectId,
        prefix: Option<&str>,
        after: Option<&RepoPath>,
        limit: usize,
    ) -> crate::Result<Vec<crate::Revision>> {
        self.inner
            .snapshot_page(repo, commit, prefix, after, limit)
            .await
    }

    async fn head_counts(
        &self,
        repo: &RepoId,
        commit: &ObjectId,
    ) -> crate::Result<crate::HeadCounts> {
        self.inner.head_counts(repo, commit).await
    }

    async fn list_head_revisions(
        &self,
        repo: &RepoId,
        after: Option<&RepoPath>,
        limit: usize,
    ) -> crate::Result<Vec<crate::Revision>> {
        self.inner.list_head_revisions(repo, after, limit).await
    }

    async fn history(
        &self,
        repo: &RepoId,
        path: &RepoPath,
        within: Option<&ObjectId>,
    ) -> crate::Result<Vec<crate::Revision>> {
        self.inner.history(repo, path, within).await
    }

    async fn history_page(
        &self,
        repo: &RepoId,
        path: &RepoPath,
        within: Option<&ObjectId>,
        after: Option<RevisionId>,
        limit: usize,
    ) -> crate::Result<Vec<crate::Revision>> {
        self.inner
            .history_page(repo, path, within, after, limit)
            .await
    }

    async fn repo_history(
        &self,
        repo: &RepoId,
        within: Option<&ObjectId>,
        filter: Option<&crate::PathFilter>,
        before: Option<&crate::HistoryCursor>,
        limit: usize,
        budget: usize,
    ) -> crate::Result<crate::HistoryScan> {
        self.inner
            .repo_history(repo, within, filter, before, limit, budget)
            .await
    }

    async fn create_repo_capped(
        &self,
        repo: RepoRecord,
        max_repos: Option<u64>,
    ) -> crate::Result<RepoRecord> {
        self.inner.create_repo_capped(repo, max_repos).await
    }

    async fn find_repo(&self, owner: &UserId, name: &str) -> crate::Result<Option<RepoRecord>> {
        self.inner.find_repo(owner, name).await
    }

    async fn get_repo(&self, id: &RepoId) -> crate::Result<Option<RepoRecord>> {
        self.inner.get_repo(id).await
    }

    async fn list_repos(&self, owner: Option<&UserId>) -> crate::Result<Vec<RepoRecord>> {
        self.inner.list_repos(owner).await
    }

    async fn list_public_repos(
        &self,
        after: Option<(&UserId, &str)>,
        limit: usize,
    ) -> crate::Result<Vec<RepoRecord>> {
        self.inner.list_public_repos(after, limit).await
    }

    async fn update_repo(&self, id: &RepoId, update: RepoUpdate) -> crate::Result<RepoRecord> {
        self.inner.update_repo(id, update).await
    }

    async fn delete_repo(&self, id: &RepoId) -> crate::Result<bool> {
        self.inner.delete_repo(id).await
    }

    async fn owner_usage(&self, owner: &UserId) -> crate::Result<crate::Usage> {
        self.inner.owner_usage(owner).await
    }

    async fn repo_usage(&self, repo: &RepoId) -> crate::Result<crate::RepoUsage> {
        self.inner.repo_usage(repo).await
    }
}
