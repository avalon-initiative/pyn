//! Behaviour every `MetadataStore` must satisfy, run against the store passed in.

use std::sync::Arc;

use chrono::{Duration, TimeZone, Utc};

use crate::access::{
    AccountKind, AccountStatus, InviteId, InviteRecord, NewAccount, OrgRole, Permission, Principal,
    Role, RoleDefinitions, RoleSource, SessionRecord, SignupStage, SshKeyRecord, TeamRecord,
    TokenId, TokenRecord, VerificationRecord,
};
use crate::clock::Clock;
use crate::memory::{MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore};
use crate::ratelimit::RateLimitStore;
use crate::{AccessService, AccessStore, Credential, Identity, OrgMemberChange, RepoMember};
use crate::{
    AuditAction, AuditQuery, AuditScope, AuditStore, ContentHash, ManualClock, MetadataStore,
    ObjectStore, PynError, RepoId, RepoPath, RepoRecord, RepoService, RepoSettings, RepoUpdate,
    RevisionId, Rules, ServiceConfig, UserId, Visibility,
};

pub type Store = Arc<dyn MetadataStore>;
pub type Access = Arc<dyn AccessStore>;
pub type Audit = Arc<dyn AuditStore>;
pub type RateLimits = Arc<dyn RateLimitStore>;

const RULES: &str = "[meta]\ndefault = \"shared\"\n[exclusive]\npaths = [\"Content/\"]\n";

struct Harness {
    svc: Arc<RepoService>,
    objects: Arc<MemoryObjectStore>,
    audit: Arc<MemoryAuditStore>,
    clock: Arc<ManualClock>,
}

fn harness(store: Store) -> Harness {
    let objects = Arc::new(MemoryObjectStore::new());
    let audit = Arc::new(MemoryAuditStore::new());
    let clock = Arc::new(ManualClock::new(
        Utc.with_ymd_and_hms(2026, 10, 8, 9, 0, 0).unwrap(),
    ));
    let svc = Arc::new(RepoService::new(
        RepoId::new("game"),
        Rules::from_toml(RULES).unwrap(),
        store,
        objects.clone(),
        audit.clone(),
        clock.clone(),
        ServiceConfig::default(),
    ));
    Harness {
        svc,
        objects,
        audit,
        clock,
    }
}

fn path(s: &str) -> RepoPath {
    RepoPath::new(s).unwrap()
}

fn user(s: &str) -> UserId {
    UserId::new(s)
}

fn who(s: &str) -> Principal {
    Principal::unrestricted(user(s))
}

async fn blob(h: &Harness, s: &str) -> ContentHash {
    h.objects.put(s.as_bytes().to_vec()).await.unwrap()
}

pub async fn second_user_cannot_take_a_held_lock(store: Store) {
    let h = harness(store);
    let map = path("Content/Dungeon.umap");
    h.svc.checkout(&map, &user("alice"), None).await.unwrap();
    let err = h.svc.checkout(&map, &user("bob"), None).await.unwrap_err();
    assert!(
        matches!(err, PynError::LockHeld { ref owner, .. } if owner == &user("alice")),
        "{err}"
    );
}

pub async fn revisions_keep_the_mode_they_were_made_under(store: Store) {
    let h = harness(store);
    let (src, map) = (path("Source/a.cpp"), path("Content/a.umap"));
    let c = blob(&h, "x").await;
    let shared = h
        .svc
        .checkin(&who("alice"), &src, c.clone(), None, "s".into())
        .await
        .unwrap();
    h.svc.checkout(&map, &user("alice"), None).await.unwrap();
    let exclusive = h
        .svc
        .checkin(&who("alice"), &map, c, None, "e".into())
        .await
        .unwrap();
    assert_eq!(shared.mode, Some(crate::Mode::Shared));
    assert_eq!(exclusive.mode, Some(crate::Mode::Exclusive));
    let stored = h.svc.history(&map).await.unwrap();
    assert_eq!(stored[0].mode, Some(crate::Mode::Exclusive));
}

pub async fn racing_checkouts_have_exactly_one_winner(store: Store) {
    let h = harness(store);
    let map = path("Content/Dungeon.umap");
    let tasks: Vec<_> = (0..16)
        .map(|i| {
            let svc = h.svc.clone();
            let map = map.clone();
            tokio::spawn(async move {
                svc.checkout(&map, &user(&format!("u{i}")), None)
                    .await
                    .is_ok()
            })
        })
        .collect();
    let mut wins = 0;
    for t in tasks {
        wins += usize::from(t.await.unwrap());
    }
    assert_eq!(wins, 1);
}

pub async fn holder_can_renew_and_the_lease_moves_forward(store: Store) {
    let h = harness(store);
    let map = path("Content/Dungeon.umap");
    let first = h.svc.checkout(&map, &user("alice"), None).await.unwrap();
    h.clock.advance(Duration::hours(3));
    let renewed = h.svc.checkout(&map, &user("alice"), None).await.unwrap();
    assert_eq!(renewed.acquired_at, first.acquired_at);
    assert_eq!(renewed.expires_at, first.expires_at + Duration::hours(3));
}

pub async fn expired_lease_frees_the_file(store: Store) {
    let h = harness(store);
    let map = path("Content/Dungeon.umap");
    h.svc.checkout(&map, &user("alice"), None).await.unwrap();
    h.clock.advance(Duration::hours(8));
    assert!(h.svc.locks().await.unwrap().is_empty());
    h.svc.checkout(&map, &user("bob"), None).await.unwrap();
}

pub async fn expired_holder_cannot_check_in(store: Store) {
    let h = harness(store);
    let map = path("Content/Dungeon.umap");
    h.svc.checkout(&map, &user("alice"), None).await.unwrap();
    h.clock.advance(Duration::hours(9));
    let c = blob(&h, "v1").await;
    let err = h
        .svc
        .checkin(&who("alice"), &map, c, None, "late".into())
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::LockRequired(_)), "{err}");
}

pub async fn exclusive_checkin_requires_the_lock(store: Store) {
    let h = harness(store);
    let map = path("Content/Dungeon.umap");
    let c = blob(&h, "v1").await;
    let err = h
        .svc
        .checkin(&who("alice"), &map, c.clone(), None, "sneaky".into())
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::LockRequired(_)), "{err}");

    h.svc.checkout(&map, &user("alice"), None).await.unwrap();
    let err = h
        .svc
        .checkin(&who("bob"), &map, c, None, "bypass".into())
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::LockHeld { .. }), "{err}");
    assert!(
        h.svc.head(&map).await.unwrap().is_none(),
        "rejected checkins must not create revisions"
    );
}

pub async fn checkin_creates_a_revision_and_releases_the_lock(store: Store) {
    let h = harness(store);
    let map = path("Content/Dungeon.umap");
    h.svc.checkout(&map, &user("alice"), None).await.unwrap();
    let c1 = blob(&h, "v1").await;
    let r1 = h
        .svc
        .checkin(&who("alice"), &map, c1, None, "first".into())
        .await
        .unwrap();
    assert_eq!(r1.id, RevisionId(1));
    assert!(h.svc.locks().await.unwrap().is_empty());

    h.svc
        .checkout(&map, &user("bob"), Some(RevisionId(1)))
        .await
        .unwrap();
    let c2 = blob(&h, "v2").await;
    let r2 = h
        .svc
        .checkin(&who("bob"), &map, c2, Some(RevisionId(1)), "second".into())
        .await
        .unwrap();
    assert_eq!(r2.id, RevisionId(2));
    let hist = h.svc.history(&map).await.unwrap();
    assert_eq!(
        hist.iter().map(|r| r.author.as_str()).collect::<Vec<_>>(),
        ["alice", "bob"]
    );
}

pub async fn checkout_from_a_stale_copy_is_refused(store: Store) {
    let h = harness(store);
    let map = path("Content/Dungeon.umap");
    h.svc.checkout(&map, &user("alice"), None).await.unwrap();
    let c = blob(&h, "v1").await;
    h.svc
        .checkin(&who("alice"), &map, c, None, "first".into())
        .await
        .unwrap();

    let err = h.svc.checkout(&map, &user("bob"), None).await.unwrap_err();
    assert!(
        matches!(
            err,
            PynError::StaleBase {
                actual: Some(RevisionId(1)),
                ..
            }
        ),
        "{err}"
    );
    assert!(
        h.svc.locks().await.unwrap().is_empty(),
        "a refused checkout must not take the lock"
    );
}

pub async fn shared_paths_cannot_be_checked_out_and_reject_stale_checkins(store: Store) {
    let h = harness(store);
    let src = path("Source/Player.cpp");
    let err = h
        .svc
        .checkout(&src, &user("alice"), None)
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::NotExclusive(_)), "{err}");

    let c1 = blob(&h, "a").await;
    h.svc
        .checkin(&who("alice"), &src, c1, None, "a".into())
        .await
        .unwrap();
    let c2 = blob(&h, "b").await;
    let err = h
        .svc
        .checkin(&who("bob"), &src, c2, None, "b".into())
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::StaleBase { .. }), "{err}");
}

pub async fn release_is_holder_only(store: Store) {
    let h = harness(store);
    let map = path("Content/Dungeon.umap");
    h.svc.checkout(&map, &user("alice"), None).await.unwrap();
    assert!(matches!(
        h.svc.release(&map, &user("bob")).await,
        Err(PynError::NotLockHolder(_))
    ));
    h.svc.release(&map, &user("alice")).await.unwrap();
    h.svc.checkout(&map, &user("bob"), None).await.unwrap();
}

pub async fn checkin_of_unknown_content_is_refused(store: Store) {
    let h = harness(store);
    let src = path("Source/a.cpp");
    let ghost = ContentHash::new("00".repeat(32));
    let err = h
        .svc
        .checkin(&who("alice"), &src, ghost, None, "x".into())
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::ObjectMissing(_)), "{err}");
}

pub async fn files_lists_heads_and_locks(store: Store) {
    let h = harness(store);
    let (a, m, n) = (
        path("Source/a.cpp"),
        path("Content/m.umap"),
        path("Content/n.umap"),
    );
    for (i, text) in ["one", "two"].into_iter().enumerate() {
        let c = blob(&h, text).await;
        let base = (i > 0).then_some(RevisionId(i as u64));
        h.svc
            .checkin(&who("alice"), &a, c, base, text.into())
            .await
            .unwrap();
    }
    h.svc.checkout(&n, &user("bob"), None).await.unwrap();
    let c = blob(&h, "n").await;
    h.svc
        .checkin(&who("bob"), &n, c, None, "n".into())
        .await
        .unwrap();
    h.svc.checkout(&m, &user("alice"), None).await.unwrap();

    let files = h.svc.files(None, 100).await.unwrap();
    let summary: Vec<_> = files
        .iter()
        .map(|f| {
            (
                f.path.as_str(),
                f.revision.map(|r| r.0),
                f.lock.as_ref().map(|l| l.owner.as_str()),
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            ("Content/m.umap", None, Some("alice")),
            ("Content/n.umap", Some(1), None),
            ("Source/a.cpp", Some(2), None),
        ]
    );

    let page = h.svc.files(Some(&path("Content/m.umap")), 1).await.unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].path.as_str(), "Content/n.umap");
}

fn rules_with_mixed_folder() -> Rules {
    Rules::from_toml(
        "[meta]\ndefault = \"shared\"\n[exclusive]\npaths = [\"Content/\", \"Mixed/lock.bin\"]\n",
    )
    .unwrap()
}

fn tree_service(store: &Store, h: &Harness) -> RepoService {
    RepoService::new(
        RepoId::new("game"),
        rules_with_mixed_folder(),
        store.clone(),
        h.objects.clone(),
        h.audit.clone(),
        h.clock.clone(),
        ServiceConfig::default(),
    )
}

pub async fn tree_lists_a_directory_with_last_change_mode_and_lock(store: Store) {
    let h = harness(store.clone());
    let svc = tree_service(&store, &h);
    assert!(svc.tree(None).await.unwrap().is_empty());

    for (p, text, by) in [
        ("README.md", "r", "alice"),
        ("Source/a.cpp", "a", "alice"),
        ("Source/deep/b.cpp", "b", "bob"),
        ("Mixed/lock.bin", "l", "bob"),
        ("Mixed/free.bin", "f", "bob"),
    ] {
        let p = path(p);
        let c = blob(&h, text).await;
        if svc.mode_for(&p) == crate::Mode::Exclusive {
            svc.checkout(&p, &user(by), None).await.unwrap();
        }
        svc.checkin(&who(by), &p, c, None, format!("add {text}"))
            .await
            .unwrap();
        h.clock.advance(Duration::minutes(1));
    }
    svc.checkout(&path("Content/m.umap"), &user("carol"), None)
        .await
        .unwrap();

    let root = svc.tree(None).await.unwrap();
    let summary: Vec<_> = root
        .iter()
        .map(|e| {
            (
                e.name.as_str(),
                e.kind,
                e.mode,
                e.last_change.as_ref().map(|r| r.message.as_str()),
                e.lock.as_ref().map(|l| l.owner.as_str()),
            )
        })
        .collect();
    use crate::tree::{EntryKind::*, EntryMode::*};
    assert_eq!(
        summary,
        [
            ("Content", Folder, Exclusive, None, None),
            ("Mixed", Folder, Mixed, Some("add f"), None),
            ("Source", Folder, Shared, Some("add b"), None),
            ("README.md", File, Shared, Some("add r"), None),
        ]
    );

    let source = svc.tree(Some(&path("Source"))).await.unwrap();
    let names: Vec<_> = source.iter().map(|e| (e.path.as_str(), e.kind)).collect();
    assert_eq!(names, [("Source/deep", Folder), ("Source/a.cpp", File)]);

    let content = svc.tree(Some(&path("Content"))).await.unwrap();
    assert_eq!(content.len(), 1);
    assert_eq!(content[0].lock.as_ref().unwrap().owner, user("carol"));
    assert!(content[0].last_change.is_none());

    let mixed = svc.tree(Some(&path("Mixed"))).await.unwrap();
    let modes: Vec<_> = mixed.iter().map(|e| (e.name.as_str(), e.mode)).collect();
    assert_eq!(modes, [("free.bin", Shared), ("lock.bin", Exclusive)]);
}

pub async fn tree_rejects_missing_directories_and_files(store: Store) {
    let h = harness(store);
    let c = blob(&h, "a").await;
    h.svc
        .checkin(&who("alice"), &path("Source/a.cpp"), c, None, "a".into())
        .await
        .unwrap();
    let missing = h.svc.tree(Some(&path("Nope"))).await.unwrap_err();
    assert!(matches!(missing, PynError::PathNotFound(_)), "{missing}");
    let file = h.svc.tree(Some(&path("Source/a.cpp"))).await.unwrap_err();
    assert!(matches!(file, PynError::InvalidRequest(_)), "{file}");
}

pub async fn summary_counts_files_locks_and_file_activity(store: Store) {
    let h = harness(store);
    let (a, m) = (path("Source/a.cpp"), path("Content/m.umap"));
    let fresh = h.svc.summary(10).await.unwrap();
    assert_eq!((fresh.files, fresh.updated_at), (0, None));
    assert_eq!(fresh.default_branch, "main");

    let c = blob(&h, "a").await;
    h.svc
        .checkin(&who("alice"), &a, c, None, "a".into())
        .await
        .unwrap();
    h.clock.advance(Duration::minutes(1));
    h.svc.checkout(&m, &user("bob"), None).await.unwrap();
    let c = blob(&h, "m").await;
    h.svc
        .checkin(&who("bob"), &m, c, None, "m".into())
        .await
        .unwrap();
    h.clock.advance(Duration::minutes(1));
    h.svc
        .checkout(&m, &user("bob"), Some(RevisionId(1)))
        .await
        .unwrap();
    h.svc.force_unlock(&m, &user("maya"), "away").await.unwrap();
    h.svc
        .checkout(&m, &user("carol"), Some(RevisionId(1)))
        .await
        .unwrap();

    let s = h.svc.summary(3).await.unwrap();
    assert_eq!((s.files, s.exclusive_files, s.shared_files), (2, 1, 1));
    assert_eq!(s.branch_count, 1);
    assert_eq!(s.updated_at, Some(h.clock.now() - Duration::minutes(1)));
    let locks: Vec<_> = s
        .locks
        .iter()
        .map(|l| (l.path.as_str(), l.owner.as_str()))
        .collect();
    assert_eq!(locks, [("Content/m.umap", "carol")]);
    let acts: Vec<_> = s
        .activity
        .iter()
        .map(|e| (e.actor.as_str(), e.action))
        .collect();
    assert_eq!(
        acts,
        [
            ("carol", AuditAction::Checkout),
            ("maya", AuditAction::ForceUnlock),
            ("bob", AuditAction::Checkout),
        ]
    );
}

pub async fn old_revisions_can_be_read_back(store: Store) {
    let h = harness(store);
    let a = path("Source/a.cpp");
    for (i, text) in ["one", "two", "three"].into_iter().enumerate() {
        let c = blob(&h, text).await;
        let base = (i > 0).then_some(RevisionId(i as u64));
        h.svc
            .checkin(&who("alice"), &a, c, base, text.into())
            .await
            .unwrap();
    }

    let (rev, bytes) = h.svc.read(&a, Some(RevisionId(1))).await.unwrap();
    assert_eq!((rev.id, bytes), (RevisionId(1), b"one".to_vec()));
    let (rev, bytes) = h.svc.read(&a, None).await.unwrap();
    assert_eq!((rev.id, bytes), (RevisionId(3), b"three".to_vec()));
    let err = h.svc.read(&a, Some(RevisionId(9))).await.unwrap_err();
    assert!(matches!(err, PynError::RevisionNotFound { .. }), "{err}");
}

async fn exclusive_history(h: &Harness, path: &RepoPath, texts: &[&str]) {
    for (i, text) in texts.iter().enumerate() {
        let base = (i > 0).then_some(RevisionId(i as u64));
        h.svc.checkout(path, &user("alice"), base).await.unwrap();
        let c = blob(h, text).await;
        h.svc
            .checkin(&who("alice"), path, c, base, (*text).into())
            .await
            .unwrap();
    }
}

async fn commit(h: &Harness, p: &RepoPath, text: &str) {
    let base = h.svc.head(p).await.unwrap().map(|r| r.id);
    if h.svc.mode_for(p) == crate::Mode::Exclusive {
        h.svc.checkout(p, &user("alice"), base).await.unwrap();
    }
    let c = blob(h, text).await;
    h.svc
        .checkin(&who("alice"), p, c, base, text.into())
        .await
        .unwrap();
}

/// Commits across paths; the first two share a timestamp to exercise the tie-break.
async fn seed_repo_history(h: &Harness) {
    for (p, text, tick) in [
        ("Source/a.ts", "a1", false),
        ("Content/m.umap", "m1", false),
        ("Source/deep/b.ts", "b1", true),
        ("Source/a.ts", "a2", true),
        ("README.md", "r1", true),
        ("Content/m.umap", "m2", true),
    ] {
        if tick {
            h.clock.advance(Duration::minutes(1));
        }
        commit(h, &path(p), text).await;
    }
}

fn labels(revs: &[crate::Revision]) -> Vec<String> {
    revs.iter()
        .map(|r| format!("{}@{}", r.path, r.id))
        .collect()
}

pub async fn repo_history_is_newest_first_across_paths(store: Store) {
    let h = harness(store);
    seed_repo_history(&h).await;
    let page = h.svc.repo_history(None, None, None).await.unwrap();
    assert_eq!(
        labels(&page.revisions),
        [
            "Content/m.umap@2",
            "README.md@1",
            "Source/a.ts@2",
            "Source/deep/b.ts@1",
            "Source/a.ts@1",
            "Content/m.umap@1"
        ],
        "equal timestamps order by path descending"
    );
    assert!(page.next.is_none());
    let other = h.svc.history(&path("Source/a.ts")).await.unwrap();
    assert_eq!(
        labels(&other),
        ["Source/a.ts@1", "Source/a.ts@2"],
        "one path stays oldest first"
    );
}

pub async fn repo_history_filters_by_glob(store: Store) {
    let h = harness(store);
    seed_repo_history(&h).await;
    for (pattern, want) in [
        (
            "*.ts",
            vec!["Source/a.ts@2", "Source/deep/b.ts@1", "Source/a.ts@1"],
        ),
        ("Source/*.ts", vec!["Source/a.ts@2", "Source/a.ts@1"]),
        (
            "Source/**",
            vec!["Source/a.ts@2", "Source/deep/b.ts@1", "Source/a.ts@1"],
        ),
        (
            "Content/*.umap",
            vec!["Content/m.umap@2", "Content/m.umap@1"],
        ),
        ("*.nothing", vec![]),
    ] {
        let f = crate::PathFilter::new(pattern).unwrap();
        let page = h.svc.repo_history(Some(&f), None, None).await.unwrap();
        assert_eq!(labels(&page.revisions), want, "{pattern}");
    }
}

pub async fn repo_history_pages_without_gaps_or_repeats(store: Store) {
    let h = harness(store);
    seed_repo_history(&h).await;
    let all = labels(
        &h.svc
            .repo_history(None, None, None)
            .await
            .unwrap()
            .revisions,
    );
    for size in [1, 2, 4] {
        let mut seen = Vec::new();
        let mut cursor = None;
        loop {
            let page = h
                .svc
                .repo_history(None, cursor.as_ref(), Some(size))
                .await
                .unwrap();
            assert!(page.revisions.len() <= size);
            seen.extend(labels(&page.revisions));
            match page.next {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        assert_eq!(seen, all, "page size {size}");
    }

    let f = crate::PathFilter::new("*.ts").unwrap();
    let first = h.svc.repo_history(Some(&f), None, Some(2)).await.unwrap();
    assert_eq!(
        labels(&first.revisions),
        ["Source/a.ts@2", "Source/deep/b.ts@1"]
    );
    let second = h
        .svc
        .repo_history(Some(&f), first.next.as_ref(), Some(2))
        .await
        .unwrap();
    assert_eq!(labels(&second.revisions), ["Source/a.ts@1"]);
    assert!(second.next.is_none());
}

pub async fn repo_history_is_per_repository(store: Store) {
    let h = harness(store.clone());
    let other = service_in(&store, &h, "other");
    seed_repo_history(&h).await;
    assert!(
        other
            .repo_history(None, None, None)
            .await
            .unwrap()
            .revisions
            .is_empty()
    );
    assert_eq!(
        h.svc
            .repo_history(None, None, None)
            .await
            .unwrap()
            .revisions
            .len(),
        6
    );
}

pub async fn restore_appends_a_checkpoint_with_the_old_content(store: Store) {
    let h = harness(store);
    let m = path("Content/m.umap");
    exclusive_history(&h, &m, &["one", "two", "three"]).await;

    h.svc
        .checkout(&m, &user("bob"), Some(RevisionId(3)))
        .await
        .unwrap();
    let r = h
        .svc
        .restore(
            &who("bob"),
            &m,
            RevisionId(1),
            RevisionId(3),
            "Content/m.umap@r3",
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        (r.id, r.restored_from, r.author.as_str()),
        (RevisionId(4), Some(RevisionId(1)), "bob")
    );

    let hist = h.svc.history(&m).await.unwrap();
    assert_eq!(
        hist.iter().map(|r| r.id.0).collect::<Vec<_>>(),
        [1, 2, 3, 4]
    );
    assert_eq!(hist[3].content, hist[0].content);
    assert_eq!(hist[2].restored_from, None);
    assert_eq!(h.svc.read(&m, None).await.unwrap().1, b"one");
    assert!(
        h.svc.locks().await.unwrap().is_empty(),
        "a restore releases the lock like a checkin"
    );

    h.svc
        .checkout(&m, &user("alice"), Some(RevisionId(4)))
        .await
        .unwrap();
    let c = blob(&h, "four").await;
    let next = h
        .svc
        .checkin(&who("alice"), &m, c, Some(RevisionId(4)), "edit".into())
        .await
        .unwrap();
    assert_eq!(
        (next.id, next.restored_from),
        (RevisionId(5), None),
        "edits build on the restore"
    );
}

pub async fn restore_needs_the_lock_and_the_right_confirmation(store: Store) {
    let h = harness(store);
    let m = path("Content/m.umap");
    exclusive_history(&h, &m, &["one", "two"]).await;
    let restore = |name: &str, confirm: &str| {
        let (svc, by, confirm) = (h.svc.clone(), who(name), confirm.to_string());
        let m = m.clone();
        async move {
            svc.restore(&by, &m, RevisionId(1), RevisionId(2), &confirm, None)
                .await
        }
    };

    let err = restore("bob", "Content/m.umap@r2").await.unwrap_err();
    assert!(matches!(err, PynError::LockRequired(_)), "{err}");

    h.svc
        .checkout(&m, &user("alice"), Some(RevisionId(2)))
        .await
        .unwrap();
    let err = restore("bob", "Content/m.umap@r2").await.unwrap_err();
    assert!(
        matches!(err, PynError::LockHeld { .. }),
        "someone else's lock: {err}"
    );

    for wrong in ["", "Content/m.umap", "Content/m.umap@r1", "Source/x@r2"] {
        let err = restore("alice", wrong).await.unwrap_err();
        assert!(
            matches!(err, PynError::ConfirmationRequired { ref expected } if expected == "Content/m.umap@r2"),
            "{wrong:?}: {err}"
        );
    }
    assert_eq!(
        h.svc.head(&m).await.unwrap().unwrap().id,
        RevisionId(2),
        "refused restores change nothing"
    );
}

pub async fn restore_is_for_exclusive_paths_and_real_older_revisions(store: Store) {
    let h = harness(store);
    let s = path("Source/a.cpp");
    for (i, text) in ["one", "two"].into_iter().enumerate() {
        let c = blob(&h, text).await;
        let base = (i > 0).then_some(RevisionId(i as u64));
        h.svc
            .checkin(&who("alice"), &s, c, base, text.into())
            .await
            .unwrap();
    }
    let err = h
        .svc
        .restore(
            &who("alice"),
            &s,
            RevisionId(1),
            RevisionId(2),
            "Source/a.cpp@r2",
            None,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::NotExclusive(_)), "{err}");

    let m = path("Content/m.umap");
    exclusive_history(&h, &m, &["one", "two"]).await;
    h.svc
        .checkout(&m, &user("alice"), Some(RevisionId(2)))
        .await
        .unwrap();
    let err = h
        .svc
        .restore(
            &who("alice"),
            &m,
            RevisionId(9),
            RevisionId(2),
            "Content/m.umap@r2",
            None,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::RevisionNotFound { .. }), "{err}");
    let err = h
        .svc
        .restore(
            &who("alice"),
            &m,
            RevisionId(2),
            RevisionId(2),
            "Content/m.umap@r2",
            None,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, PynError::InvalidRequest(_)),
        "the head itself: {err}"
    );
}

pub async fn force_unlock_removes_a_live_lock_and_says_why(store: Store) {
    let h = harness(store);
    let m = path("Content/m.umap");
    h.svc.checkout(&m, &user("alice"), None).await.unwrap();

    let err = h
        .svc
        .force_unlock(&m, &user("maya"), "  ")
        .await
        .unwrap_err();
    assert!(
        matches!(err, PynError::InvalidRequest(_)),
        "a reason is required: {err}"
    );
    assert_eq!(
        h.svc.locks().await.unwrap().len(),
        1,
        "a refused unlock changes nothing"
    );

    let removed = h
        .svc
        .force_unlock(&m, &user("maya"), "alice is on leave")
        .await
        .unwrap();
    assert_eq!(removed.owner, user("alice"));
    assert!(h.svc.locks().await.unwrap().is_empty());
    h.svc.checkout(&m, &user("bob"), None).await.unwrap();

    let err = h
        .svc
        .force_unlock(&path("Content/free.umap"), &user("maya"), "x")
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::NotLocked(_)), "{err}");
    h.clock.advance(Duration::hours(9));
    let err = h
        .svc
        .force_unlock(&m, &user("maya"), "x")
        .await
        .unwrap_err();
    assert!(
        matches!(err, PynError::NotLocked(_)),
        "an expired lock is already gone: {err}"
    );

    let log = h
        .svc
        .audit(&AuditQuery {
            action: Some(AuditAction::ForceUnlock),
            limit: 10,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(log.len(), 1);
    assert_eq!(
        (
            log[0].actor.as_str(),
            log[0].path.as_ref().map(|p| p.as_str())
        ),
        ("maya", Some("Content/m.umap"))
    );
    assert!(
        log[0].detail.contains("alice") && log[0].detail.contains("alice is on leave"),
        "{}",
        log[0].detail
    );
}

fn limited_service(store: &Store, h: &Harness, id: &str, max_locks: u32) -> Arc<RepoService> {
    Arc::new(RepoService::new(
        RepoId::new(id),
        Rules::from_toml(RULES).unwrap(),
        store.clone(),
        h.objects.clone(),
        h.audit.clone(),
        h.clock.clone(),
        ServiceConfig {
            max_locks,
            ..ServiceConfig::default()
        },
    ))
}

pub async fn checkout_past_the_lock_limit_is_refused_until_a_slot_frees(store: Store) {
    let h = harness(store.clone());
    let svc = limited_service(&store, &h, "game", 2);
    let (a, b, c) = (path("Content/a"), path("Content/b"), path("Content/c"));
    let alice = user("alice");
    svc.checkout(&a, &alice, None).await.unwrap();
    svc.checkout(&b, &alice, None).await.unwrap();

    let err = svc.checkout(&c, &alice, None).await.unwrap_err();
    assert!(
        matches!(err, PynError::LockLimitReached { limit: 2 }),
        "{err}"
    );
    assert_eq!(
        svc.locks().await.unwrap().len(),
        2,
        "a refusal locks nothing"
    );
    svc.checkout(&a, &alice, None).await.unwrap();
    svc.checkout(&c, &user("bob"), None).await.unwrap();
    let held = svc.checkout(&c, &alice, None).await.unwrap_err();
    assert!(
        matches!(held, PynError::LockHeld { .. }),
        "a held path reports the holder: {held}"
    );

    svc.release(&a, &alice).await.unwrap();
    svc.checkout(&a, &alice, None).await.unwrap();
    svc.force_unlock(&b, &user("maya"), "reassigned")
        .await
        .unwrap();
    svc.checkout(&b, &alice, None).await.unwrap();

    h.clock.advance(Duration::hours(9));
    svc.checkout(&c, &alice, None).await.unwrap();
    svc.checkout(&a, &alice, None).await.unwrap();
    let again = svc.checkout(&b, &alice, None).await.unwrap_err();
    assert!(
        matches!(again, PynError::LockLimitReached { .. }),
        "{again}"
    );
}

pub async fn the_lock_limit_counts_one_repository_at_a_time(store: Store) {
    let h = harness(store.clone());
    let game = limited_service(&store, &h, "game", 1);
    let other = limited_service(&store, &h, "other", 1);
    let alice = user("alice");
    game.checkout(&path("Content/a"), &alice, None)
        .await
        .unwrap();
    other
        .checkout(&path("Content/a"), &alice, None)
        .await
        .unwrap();
    let err = other
        .checkout(&path("Content/b"), &alice, None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, PynError::LockLimitReached { limit: 1 }),
        "{err}"
    );
}

pub async fn racing_checkouts_cannot_exceed_the_lock_limit(store: Store) {
    let h = harness(store.clone());
    let svc = limited_service(&store, &h, "game", 3);
    let tasks: Vec<_> = (0..10)
        .map(|i| {
            let svc = svc.clone();
            tokio::spawn(async move {
                svc.checkout(&path(&format!("Content/{i}")), &user("alice"), None)
                    .await
            })
        })
        .collect();
    let mut won = 0;
    for task in tasks {
        match task.await.unwrap() {
            Ok(_) => won += 1,
            Err(e) => assert!(matches!(e, PynError::LockLimitReached { .. }), "{e}"),
        }
    }
    assert_eq!(won, 3);
    assert_eq!(svc.locks().await.unwrap().len(), 3);
}

pub async fn operations_are_recorded_in_the_audit_log(store: Store) {
    let h = harness(store);
    let m = path("Content/m.umap");
    h.svc.checkout(&m, &user("alice"), None).await.unwrap();
    let c = blob(&h, "one").await;
    h.svc
        .checkin(&who("alice"), &m, c, None, "first".into())
        .await
        .unwrap();
    h.svc
        .checkout(&m, &user("alice"), Some(RevisionId(1)))
        .await
        .unwrap();
    h.svc.release(&m, &user("alice")).await.unwrap();

    let log = h
        .svc
        .audit(&AuditQuery {
            limit: 10,
            ..Default::default()
        })
        .await
        .unwrap();
    let actions: Vec<_> = log.iter().map(|e| (e.action, e.actor.as_str())).collect();
    assert_eq!(
        actions,
        [
            (AuditAction::Release, "alice"),
            (AuditAction::Checkout, "alice"),
            (AuditAction::Checkin, "alice"),
            (AuditAction::Checkout, "alice"),
        ]
    );
    assert!(
        log[2].detail.contains("r1") && log[2].detail.contains("first"),
        "{}",
        log[2].detail
    );
    assert!(
        h.audit
            .list(
                &AuditScope::Repo(RepoId::new("game")),
                &AuditQuery {
                    limit: 100,
                    ..Default::default()
                }
            )
            .await
            .unwrap()
            .len()
            == 4
    );

    let failed = h
        .svc
        .checkout(&path("Source/a.cpp"), &user("alice"), None)
        .await;
    assert!(failed.is_err());
    assert_eq!(
        h.svc
            .audit(&AuditQuery {
                limit: 100,
                ..Default::default()
            })
            .await
            .unwrap()
            .len(),
        4,
        "failures are not events"
    );
}

fn repo_record(id: &str, owner: &str, name: &str) -> RepoRecord {
    RepoRecord {
        id: RepoId::new(id),
        owner: user(owner),
        name: name.to_string(),
        visibility: Visibility::Private,
        settings: RepoSettings::default(),
        created_at: at(0),
    }
}

pub async fn repositories_are_registered_found_and_listed_in_order(store: Store) {
    let mut public = repo_record("r2", "alice", "zeta");
    public.visibility = Visibility::Public;
    public.settings = RepoSettings {
        lease_hours: 2,
        ..RepoSettings::default()
    };
    for r in [
        public.clone(),
        repo_record("r1", "alice", "alpha"),
        repo_record("r3", "bob", "alpha"),
    ] {
        assert_eq!(store.create_repo(r.clone()).await.unwrap(), r);
    }

    assert_eq!(
        store.find_repo(&user("alice"), "zeta").await.unwrap(),
        Some(public.clone())
    );
    assert_eq!(
        store
            .get_repo(&RepoId::new("r3"))
            .await
            .unwrap()
            .unwrap()
            .owner,
        user("bob")
    );
    assert_eq!(store.find_repo(&user("alice"), "nope").await.unwrap(), None);
    assert_eq!(store.get_repo(&RepoId::new("nope")).await.unwrap(), None);

    let all: Vec<_> = store
        .list_repos(None)
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.address())
        .collect();
    assert_eq!(all, ["alice/alpha", "alice/zeta", "bob/alpha"]);
    let alices = store.list_repos(Some(&user("alice"))).await.unwrap();
    assert_eq!(alices.len(), 2);
}

pub async fn a_repository_name_is_unique_per_owner(store: Store) {
    store
        .create_repo(repo_record("r1", "alice", "game"))
        .await
        .unwrap();
    let err = store
        .create_repo(repo_record("r2", "alice", "game"))
        .await
        .unwrap_err();
    assert!(
        matches!(err, PynError::RepoExists(ref a) if a == "alice/game"),
        "{err}"
    );
    store
        .create_repo(repo_record("r3", "bob", "game"))
        .await
        .unwrap();
    assert_eq!(store.list_repos(None).await.unwrap().len(), 2);
}

pub async fn updating_a_repository_keeps_its_id(store: Store) {
    store
        .create_repo(repo_record("r1", "alice", "game"))
        .await
        .unwrap();
    store
        .create_repo(repo_record("r2", "alice", "taken"))
        .await
        .unwrap();

    let renamed = store
        .update_repo(
            &RepoId::new("r1"),
            RepoUpdate {
                name: Some("engine".into()),
                visibility: Some(Visibility::Public),
                settings: Some(RepoSettings {
                    lease_hours: 4,
                    ..RepoSettings::default()
                }),
            },
        )
        .await
        .unwrap();
    assert_eq!(renamed.id, RepoId::new("r1"));
    assert_eq!(renamed.address(), "alice/engine");
    assert_eq!(renamed.visibility, Visibility::Public);
    assert_eq!(renamed.settings.lease_hours, 4);
    assert_eq!(store.find_repo(&user("alice"), "game").await.unwrap(), None);
    assert_eq!(
        store.find_repo(&user("alice"), "engine").await.unwrap(),
        Some(renamed.clone())
    );

    let untouched = store
        .update_repo(&RepoId::new("r1"), RepoUpdate::default())
        .await
        .unwrap();
    assert_eq!(untouched, renamed);

    let clash = store
        .update_repo(
            &RepoId::new("r1"),
            RepoUpdate {
                name: Some("taken".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(clash, PynError::RepoExists(_)), "{clash}");
    assert_eq!(
        store
            .get_repo(&RepoId::new("r1"))
            .await
            .unwrap()
            .unwrap()
            .name,
        "engine",
        "a rejected rename changes nothing"
    );

    let missing = store
        .update_repo(&RepoId::new("nope"), RepoUpdate::default())
        .await
        .unwrap_err();
    assert!(matches!(missing, PynError::RepoNotFound(_)), "{missing}");
}

pub async fn a_repository_stores_its_lock_limit_and_can_clear_it(store: Store) {
    let mut record = repo_record("r1", "alice", "game");
    record.settings.max_locks = Some(3);
    let created = store.create_repo(record).await.unwrap();
    let found = store.get_repo(&created.id).await.unwrap().unwrap();
    assert_eq!(found.settings.max_locks, Some(3));

    let renamed = store
        .update_repo(
            &created.id,
            RepoUpdate {
                name: Some("engine".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(renamed.settings.max_locks, Some(3), "other updates keep it");

    let cleared = store
        .update_repo(
            &created.id,
            RepoUpdate {
                settings: Some(RepoSettings::default()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(cleared.settings.max_locks, None);
    let listed = store.list_repos(None).await.unwrap();
    assert_eq!(listed[0].settings.max_locks, None);
}

fn service_in(store: &Store, h: &Harness, id: &str) -> RepoService {
    RepoService::new(
        RepoId::new(id),
        Rules::from_toml(RULES).unwrap(),
        store.clone(),
        h.objects.clone(),
        h.audit.clone(),
        h.clock.clone(),
        ServiceConfig::default(),
    )
}

pub async fn repositories_do_not_share_locks_or_revisions(store: Store) {
    let h = harness(store.clone());
    let other = service_in(&store, &h, "other");
    let map = path("Content/Dungeon.umap");
    h.svc.checkout(&map, &user("alice"), None).await.unwrap();
    other.checkout(&map, &user("bob"), None).await.unwrap();
    let c = blob(&h, "one").await;
    h.svc
        .checkin(&who("alice"), &map, c, None, "first".into())
        .await
        .unwrap();
    assert!(other.head(&map).await.unwrap().is_none());
    assert_eq!(other.locks().await.unwrap().len(), 1);
    assert_eq!(h.svc.locks().await.unwrap().len(), 0);
}

pub async fn a_users_live_locks_are_listed_across_repositories(store: Store) {
    let h = harness(store.clone());
    let other = service_in(&store, &h, "other");
    let (a, b) = (path("Content/a.umap"), path("Content/b.umap"));
    other.checkout(&b, &user("alice"), None).await.unwrap();
    h.svc.checkout(&b, &user("alice"), None).await.unwrap();
    h.svc.checkout(&a, &user("alice"), None).await.unwrap();
    h.svc
        .checkout(&path("Content/c.umap"), &user("bob"), None)
        .await
        .unwrap();

    let now = h.clock.now();
    let mine = store.list_locks_of(&user("alice"), now).await.unwrap();
    let listed: Vec<_> = mine
        .iter()
        .map(|(r, l)| (r.as_str(), l.path.as_str()))
        .collect();
    assert_eq!(
        listed,
        [
            ("game", "Content/a.umap"),
            ("game", "Content/b.umap"),
            ("other", "Content/b.umap")
        ]
    );
    assert!(mine.iter().all(|(_, l)| l.owner == user("alice")));
    assert!(
        store
            .list_locks_of(&user("carol"), now)
            .await
            .unwrap()
            .is_empty()
    );

    h.clock.advance(Duration::hours(9));
    let later = h.clock.now();
    assert!(
        store
            .list_locks_of(&user("alice"), later)
            .await
            .unwrap()
            .is_empty()
    );
}

pub async fn deleting_a_repository_removes_only_its_own_data(store: Store) {
    let h = harness(store.clone());
    let other = service_in(&store, &h, "other");
    store
        .create_repo(repo_record("game", "alice", "game"))
        .await
        .unwrap();
    store
        .create_repo(repo_record("other", "alice", "other"))
        .await
        .unwrap();
    let map = path("Content/Dungeon.umap");
    for svc in [&h.svc, &other] {
        svc.checkout(&map, &user("alice"), None).await.unwrap();
        let c = blob(&h, "one").await;
        svc.checkin(&who("alice"), &map, c, None, "first".into())
            .await
            .unwrap();
        svc.checkout(&map, &user("alice"), Some(RevisionId(1)))
            .await
            .unwrap();
    }

    assert!(store.delete_repo(&RepoId::new("game")).await.unwrap());
    assert!(!store.delete_repo(&RepoId::new("game")).await.unwrap());
    assert_eq!(store.find_repo(&user("alice"), "game").await.unwrap(), None);
    assert!(h.svc.head(&map).await.unwrap().is_none());
    assert!(h.svc.locks().await.unwrap().is_empty());
    assert!(other.head(&map).await.unwrap().is_some());
    assert_eq!(other.locks().await.unwrap().len(), 1);
    assert!(
        store
            .find_repo(&user("alice"), "other")
            .await
            .unwrap()
            .is_some()
    );

    store
        .create_repo(repo_record("again", "alice", "game"))
        .await
        .unwrap();
}

/// Generates one `#[tokio::test]` per contract case for the store built by `$factory`.
#[macro_export]
macro_rules! contract_tests {
    ($factory:expr $(, #[$attr:meta])*) => {
        $crate::contract_tests!(@one $factory; [$(#[$attr])*]; second_user_cannot_take_a_held_lock);
        $crate::contract_tests!(@one $factory; [$(#[$attr])*]; revisions_keep_the_mode_they_were_made_under);
        $crate::contract_tests!(@one $factory; [$(#[$attr])*]; racing_checkouts_have_exactly_one_winner);
        $crate::contract_tests!(@one $factory; [$(#[$attr])*]; holder_can_renew_and_the_lease_moves_forward);
        $crate::contract_tests!(@one $factory; [$(#[$attr])*]; expired_lease_frees_the_file);
        $crate::contract_tests!(@one $factory; [$(#[$attr])*]; expired_holder_cannot_check_in);
        $crate::contract_tests!(@one $factory; [$(#[$attr])*]; exclusive_checkin_requires_the_lock);
        $crate::contract_tests!(@one $factory; [$(#[$attr])*]; checkin_creates_a_revision_and_releases_the_lock);
        $crate::contract_tests!(@one $factory; [$(#[$attr])*]; checkout_from_a_stale_copy_is_refused);
        $crate::contract_tests!(@one $factory; [$(#[$attr])*]; shared_paths_cannot_be_checked_out_and_reject_stale_checkins);
        $crate::contract_tests!(@one $factory; [$(#[$attr])*]; release_is_holder_only);
        $crate::contract_tests!(@one $factory; [$(#[$attr])*]; checkin_of_unknown_content_is_refused);
        $crate::contract_tests!(@one $factory; [$(#[$attr])*]; files_lists_heads_and_locks);
        $crate::contract_tests!(@one $factory; [$(#[$attr])*]; tree_lists_a_directory_with_last_change_mode_and_lock);
        $crate::contract_tests!(@one $factory; [$(#[$attr])*]; tree_rejects_missing_directories_and_files);
        $crate::contract_tests!(@one $factory; [$(#[$attr])*]; summary_counts_files_locks_and_file_activity);
        $crate::contract_tests!(@one $factory; [$(#[$attr])*]; old_revisions_can_be_read_back);
        $crate::contract_tests!(@one $factory; [$(#[$attr])*]; repo_history_is_newest_first_across_paths);
        $crate::contract_tests!(@one $factory; [$(#[$attr])*]; repo_history_filters_by_glob);
        $crate::contract_tests!(@one $factory; [$(#[$attr])*]; repo_history_pages_without_gaps_or_repeats);
        $crate::contract_tests!(@one $factory; [$(#[$attr])*]; repo_history_is_per_repository);
        $crate::contract_tests!(@one $factory; [$(#[$attr])*]; restore_appends_a_checkpoint_with_the_old_content);
        $crate::contract_tests!(@one $factory; [$(#[$attr])*]; restore_needs_the_lock_and_the_right_confirmation);
        $crate::contract_tests!(@one $factory; [$(#[$attr])*]; restore_is_for_exclusive_paths_and_real_older_revisions);
        $crate::contract_tests!(@one $factory; [$(#[$attr])*]; force_unlock_removes_a_live_lock_and_says_why);
        $crate::contract_tests!(@one $factory; [$(#[$attr])*]; checkout_past_the_lock_limit_is_refused_until_a_slot_frees);
        $crate::contract_tests!(@one $factory; [$(#[$attr])*]; the_lock_limit_counts_one_repository_at_a_time);
        $crate::contract_tests!(@one $factory; [$(#[$attr])*]; racing_checkouts_cannot_exceed_the_lock_limit);
        $crate::contract_tests!(@one $factory; [$(#[$attr])*]; operations_are_recorded_in_the_audit_log);
        $crate::contract_tests!(@one $factory; [$(#[$attr])*]; repositories_are_registered_found_and_listed_in_order);
        $crate::contract_tests!(@one $factory; [$(#[$attr])*]; a_repository_name_is_unique_per_owner);
        $crate::contract_tests!(@one $factory; [$(#[$attr])*]; updating_a_repository_keeps_its_id);
        $crate::contract_tests!(@one $factory; [$(#[$attr])*]; a_repository_stores_its_lock_limit_and_can_clear_it);
        $crate::contract_tests!(@one $factory; [$(#[$attr])*]; repositories_do_not_share_locks_or_revisions);
        $crate::contract_tests!(@one $factory; [$(#[$attr])*]; a_users_live_locks_are_listed_across_repositories);
        $crate::contract_tests!(@one $factory; [$(#[$attr])*]; deleting_a_repository_removes_only_its_own_data);
    };
    (@one $factory:expr; [$(#[$attr:meta])*]; $name:ident) => {
        #[tokio::test(flavor = "multi_thread")]
        $(#[$attr])*
        async fn $name() {
            let store: $crate::contract::Store = ($factory).await;
            $crate::contract::$name(store).await;
        }
    };
}

fn at(minutes: i64) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 8, 9, 0, 0).unwrap() + Duration::minutes(minutes)
}

fn token(id: &str, owner: &str, created: i64) -> TokenRecord {
    TokenRecord {
        id: TokenId(id.to_string()),
        user: user(owner),
        name: format!("token {id}"),
        secret_hash: "00".repeat(32),
        permissions: [Permission::Read, Permission::Checkin].into(),
        repos: vec![RepoId::new("game")],
        created_at: at(created),
        expires_at: Some(at(created + 60)),
        revoked_at: None,
        last_used_at: None,
    }
}

pub async fn tokens_round_trip_and_list_newest_first(store: Access) {
    store.ensure_user(&user("alice"), at(0)).await.unwrap();
    store.ensure_user(&user("bob"), at(0)).await.unwrap();
    for (id, owner, created) in [
        ("aaaaaaaaaaaa", "alice", 1),
        ("bbbbbbbbbbbb", "alice", 5),
        ("cccccccccccc", "bob", 3),
    ] {
        store.create_token(token(id, owner, created)).await.unwrap();
    }
    let got = store
        .get_token(&TokenId("aaaaaaaaaaaa".into()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(got, token("aaaaaaaaaaaa", "alice", 1));
    assert!(
        store
            .get_token(&TokenId("ffffffffffff".into()))
            .await
            .unwrap()
            .is_none()
    );
    let ids: Vec<_> = store
        .list_tokens(&user("alice"))
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.id.0)
        .collect();
    assert_eq!(ids, ["bbbbbbbbbbbb", "aaaaaaaaaaaa"]);
}

pub async fn revoking_is_recorded_and_harmless_twice(store: Access) {
    store.ensure_user(&user("alice"), at(0)).await.unwrap();
    let id = TokenId("aaaaaaaaaaaa".into());
    store
        .create_token(token("aaaaaaaaaaaa", "alice", 1))
        .await
        .unwrap();
    assert!(store.revoke_token(&id, at(10)).await.unwrap());
    assert!(store.revoke_token(&id, at(20)).await.unwrap());
    assert_eq!(
        store.get_token(&id).await.unwrap().unwrap().revoked_at,
        Some(at(10))
    );
    assert!(
        !store
            .revoke_token(&TokenId("ffffffffffff".into()), at(10))
            .await
            .unwrap()
    );
}

pub async fn touching_a_token_records_last_use(store: Access) {
    store.ensure_user(&user("alice"), at(0)).await.unwrap();
    let id = TokenId("aaaaaaaaaaaa".into());
    store
        .create_token(token("aaaaaaaaaaaa", "alice", 1))
        .await
        .unwrap();
    store.touch_token(&id, at(30)).await.unwrap();
    assert_eq!(
        store.get_token(&id).await.unwrap().unwrap().last_used_at,
        Some(at(30))
    );
}

pub async fn roles_assign_and_overwrite(store: Access) {
    let repo = RepoId::new("game");
    for u in ["alice", "bob"] {
        store.ensure_user(&user(u), at(0)).await.unwrap();
    }
    assert_eq!(store.role_of(&repo, &user("alice")).await.unwrap(), None);
    store
        .set_role(&repo, &user("alice"), Role::Writer)
        .await
        .unwrap();
    store
        .set_role(&repo, &user("bob"), Role::Reader)
        .await
        .unwrap();
    store
        .set_role(&repo, &user("alice"), Role::Maintainer)
        .await
        .unwrap();
    assert_eq!(
        store.role_of(&repo, &user("alice")).await.unwrap(),
        Some(Role::Maintainer)
    );
    assert_eq!(
        store.members(&repo).await.unwrap(),
        [
            (user("alice"), Role::Maintainer),
            (user("bob"), Role::Reader)
        ]
    );
    assert!(
        store
            .members(&RepoId::new("other"))
            .await
            .unwrap()
            .is_empty()
    );
}

pub async fn role_definitions_apply_overrides_to_defaults(store: Access) {
    let repo = RepoId::new("game");
    assert_eq!(
        store.role_definitions(&repo).await.unwrap(),
        RoleDefinitions::defaults()
    );
    store
        .set_role_permissions(&repo, Role::Writer, [Permission::Read].into())
        .await
        .unwrap();
    let defs = store.role_definitions(&repo).await.unwrap();
    assert_eq!(defs.get(Role::Writer), &[Permission::Read].into());
    assert_eq!(
        defs.get(Role::Admin),
        RoleDefinitions::defaults().get(Role::Admin)
    );
    assert_eq!(
        store.role_definitions(&RepoId::new("other")).await.unwrap(),
        RoleDefinitions::defaults()
    );
}

pub async fn accounts_hold_a_unique_name_and_a_password(store: Access) {
    assert!(!store.user_exists(&user("alice")).await.unwrap());
    assert!(store.create_user(&user("alice"), at(0)).await.unwrap());
    assert!(
        !store.create_user(&user("alice"), at(1)).await.unwrap(),
        "a name can be taken once"
    );
    assert!(store.user_exists(&user("alice")).await.unwrap());

    assert_eq!(store.password_hash(&user("alice")).await.unwrap(), None);
    store
        .set_password_hash(&user("alice"), "hash-one")
        .await
        .unwrap();
    store
        .set_password_hash(&user("alice"), "hash-two")
        .await
        .unwrap();
    assert_eq!(
        store
            .password_hash(&user("alice"))
            .await
            .unwrap()
            .as_deref(),
        Some("hash-two")
    );
    assert_eq!(store.password_hash(&user("nobody")).await.unwrap(), None);
}

pub async fn organizations_share_the_account_namespace_and_start_with_their_creator_as_owner(
    store: Access,
) {
    let acme = user("acme");
    assert!(store.create_user(&user("alice"), at(0)).await.unwrap());
    assert!(
        store
            .create_org(&acme, &user("alice"), at(1))
            .await
            .unwrap()
    );
    assert!(
        !store.create_org(&acme, &user("bob"), at(2)).await.unwrap(),
        "a name can be taken once"
    );
    assert!(
        !store
            .create_org(&user("alice"), &user("bob"), at(2))
            .await
            .unwrap(),
        "an organization cannot take a user's name"
    );
    assert!(!store.create_user(&acme, at(3)).await.unwrap());
    assert!(
        !store
            .create_account(new_account("acme", "acme@example.com", 3))
            .await
            .unwrap()
    );
    assert_eq!(store.password_hash(&acme).await.unwrap(), None);

    let record = store.account(&acme).await.unwrap().unwrap();
    assert_eq!(record.kind, AccountKind::Org);
    assert_eq!(record.created_at, at(1));
    assert_eq!(
        store.account(&user("alice")).await.unwrap().unwrap().kind,
        AccountKind::User
    );
    store.ensure_user(&acme, at(9)).await.unwrap();
    assert_eq!(
        store.account(&acme).await.unwrap().unwrap().kind,
        AccountKind::Org,
        "ensuring a name leaves an organization alone"
    );

    assert_eq!(
        store.org_role(&acme, &user("alice")).await.unwrap(),
        Some(OrgRole::Owner)
    );
    assert_eq!(store.org_role(&acme, &user("bob")).await.unwrap(), None);
    assert_eq!(
        store
            .org_role(&user("alice"), &user("alice"))
            .await
            .unwrap(),
        None,
        "a user has no members"
    );

    assert!(
        store
            .create_org(&user("beta"), &user("carol"), at(4))
            .await
            .unwrap()
    );
    assert!(
        store.user_exists(&user("carol")).await.unwrap(),
        "the owner's account is created with the organization"
    );
    assert!(
        store
            .create_org(&user("aaa"), &user("alice"), at(5))
            .await
            .unwrap()
    );
    assert_eq!(
        store.orgs_of(&user("alice")).await.unwrap(),
        [
            (user("aaa"), OrgRole::Owner),
            (user("acme"), OrgRole::Owner)
        ]
    );
    assert!(store.orgs_of(&user("bob")).await.unwrap().is_empty());

    let listed: Vec<_> = store
        .list_accounts(None, 10)
        .await
        .unwrap()
        .into_iter()
        .map(|a| a.user)
        .collect();
    assert_eq!(
        listed,
        [user("alice"), user("carol")],
        "listings hold users only"
    );
}

pub async fn deleting_an_organization_removes_its_members_and_frees_the_name(store: Access) {
    let acme = user("acme");
    store.create_user(&user("alice"), at(0)).await.unwrap();
    store
        .create_org(&acme, &user("alice"), at(1))
        .await
        .unwrap();

    assert!(
        !store.delete_org(&user("alice")).await.unwrap(),
        "a user is not an organization"
    );
    assert!(store.user_exists(&user("alice")).await.unwrap());
    assert!(!store.delete_org(&user("nobody")).await.unwrap());

    assert!(store.delete_org(&acme).await.unwrap());
    assert!(store.account(&acme).await.unwrap().is_none());
    assert_eq!(store.org_role(&acme, &user("alice")).await.unwrap(), None);
    assert!(store.orgs_of(&user("alice")).await.unwrap().is_empty());
    assert!(store.user_exists(&user("alice")).await.unwrap());
    assert!(!store.delete_org(&acme).await.unwrap());

    assert!(
        store.create_user(&acme, at(2)).await.unwrap(),
        "the name is free again"
    );
}

pub async fn organization_members_are_added_changed_and_never_left_without_an_owner(store: Access) {
    let acme = user("acme");
    for name in ["alice", "bob", "carol"] {
        store.create_user(&user(name), at(0)).await.unwrap();
    }
    store
        .create_org(&acme, &user("alice"), at(1))
        .await
        .unwrap();
    assert_eq!(
        store.org_members(&acme).await.unwrap(),
        [(user("alice"), OrgRole::Owner)]
    );

    assert!(
        store
            .add_org_member(&acme, &user("carol"), OrgRole::Member)
            .await
            .unwrap()
    );
    assert!(
        !store
            .add_org_member(&acme, &user("carol"), OrgRole::Owner)
            .await
            .unwrap(),
        "adding twice changes nothing"
    );
    assert_eq!(
        store.org_role(&acme, &user("carol")).await.unwrap(),
        Some(OrgRole::Member)
    );
    store
        .add_org_member(&acme, &user("bob"), OrgRole::Member)
        .await
        .unwrap();
    assert_eq!(
        store.org_members(&acme).await.unwrap(),
        [
            (user("alice"), OrgRole::Owner),
            (user("bob"), OrgRole::Member),
            (user("carol"), OrgRole::Member)
        ]
    );
    assert_eq!(
        store.orgs_of(&user("bob")).await.unwrap(),
        [(acme.clone(), OrgRole::Member)]
    );
    assert!(store.org_members(&user("other")).await.unwrap().is_empty());

    assert_eq!(
        store
            .set_org_role(&acme, &user("alice"), OrgRole::Member)
            .await
            .unwrap(),
        OrgMemberChange::LastOwner
    );
    assert_eq!(
        store
            .remove_org_member(&acme, &user("alice"), &[])
            .await
            .unwrap(),
        OrgMemberChange::LastOwner
    );
    assert_eq!(
        store.org_role(&acme, &user("alice")).await.unwrap(),
        Some(OrgRole::Owner),
        "a refused change leaves the owner in place"
    );
    assert_eq!(
        store
            .set_org_role(&acme, &user("nobody"), OrgRole::Owner)
            .await
            .unwrap(),
        OrgMemberChange::NotMember
    );
    assert_eq!(
        store
            .remove_org_member(&acme, &user("nobody"), &[])
            .await
            .unwrap(),
        OrgMemberChange::NotMember
    );

    assert_eq!(
        store
            .set_org_role(&acme, &user("bob"), OrgRole::Owner)
            .await
            .unwrap(),
        OrgMemberChange::Done(OrgRole::Member)
    );
    assert_eq!(
        store
            .set_org_role(&acme, &user("alice"), OrgRole::Member)
            .await
            .unwrap(),
        OrgMemberChange::Done(OrgRole::Owner),
        "with a second owner the first may step down"
    );
    assert_eq!(
        store
            .remove_org_member(&acme, &user("bob"), &[])
            .await
            .unwrap(),
        OrgMemberChange::LastOwner
    );
    assert_eq!(
        store
            .remove_org_member(&acme, &user("alice"), &[])
            .await
            .unwrap(),
        OrgMemberChange::Done(OrgRole::Member)
    );
    assert_eq!(store.org_role(&acme, &user("alice")).await.unwrap(), None);
    assert!(store.user_exists(&user("alice")).await.unwrap());
}

pub async fn removing_an_organization_member_drops_their_roles_in_the_given_repositories(
    store: Access,
) {
    let acme = user("acme");
    store.create_user(&user("alice"), at(0)).await.unwrap();
    store.create_user(&user("bob"), at(0)).await.unwrap();
    store
        .create_org(&acme, &user("alice"), at(1))
        .await
        .unwrap();
    store
        .add_org_member(&acme, &user("bob"), OrgRole::Member)
        .await
        .unwrap();
    let (game, tools, other) = (
        RepoId::new("acme/game"),
        RepoId::new("acme/tools"),
        RepoId::new("alice/own"),
    );
    for repo in [&game, &tools, &other] {
        store
            .set_role(repo, &user("bob"), Role::Writer)
            .await
            .unwrap();
    }
    store
        .set_role(&game, &user("alice"), Role::Admin)
        .await
        .unwrap();

    assert_eq!(
        store
            .remove_org_member(&acme, &user("bob"), &[game.clone(), tools.clone()])
            .await
            .unwrap(),
        OrgMemberChange::Done(OrgRole::Member)
    );
    assert_eq!(store.role_of(&game, &user("bob")).await.unwrap(), None);
    assert_eq!(store.role_of(&tools, &user("bob")).await.unwrap(), None);
    assert_eq!(
        store.role_of(&other, &user("bob")).await.unwrap(),
        Some(Role::Writer),
        "grants outside the organization stay"
    );
    assert_eq!(
        store.role_of(&game, &user("alice")).await.unwrap(),
        Some(Role::Admin),
        "other members keep theirs"
    );

    assert_eq!(
        store
            .remove_org_member(&acme, &user("alice"), std::slice::from_ref(&game))
            .await
            .unwrap(),
        OrgMemberChange::LastOwner
    );
    assert_eq!(
        store.role_of(&game, &user("alice")).await.unwrap(),
        Some(Role::Admin),
        "a refused removal drops no grants"
    );
}

fn team(org: &str, slug: &str) -> TeamRecord {
    TeamRecord {
        org: user(org),
        slug: slug.to_string(),
        name: format!("Team {slug}"),
        description: String::new(),
        created_at: at(0),
    }
}

async fn org_with_members(store: &Access, org: &str, owner: &str, members: &[&str]) {
    store.create_user(&user(owner), at(0)).await.unwrap();
    store
        .create_org(&user(org), &user(owner), at(1))
        .await
        .unwrap();
    for m in members {
        store.create_user(&user(m), at(0)).await.unwrap();
        store
            .add_org_member(&user(org), &user(m), OrgRole::Member)
            .await
            .unwrap();
    }
}

pub async fn teams_hold_members_and_repository_roles_until_removed(store: Access) {
    let acme = user("acme");
    org_with_members(&store, "acme", "alice", &["bob", "carol"]).await;
    let (game, tools) = (RepoId::new("acme/game"), RepoId::new("acme/tools"));

    assert!(store.create_team(team("acme", "art")).await.unwrap());
    assert!(
        !store.create_team(team("acme", "art")).await.unwrap(),
        "a slug is unique in its organization"
    );
    assert!(store.create_team(team("acme", "eng")).await.unwrap());
    store
        .create_org(&user("other"), &user("alice"), at(1))
        .await
        .unwrap();
    assert!(
        store.create_team(team("other", "art")).await.unwrap(),
        "other organizations may reuse a slug"
    );
    let listed = store.teams(&acme).await.unwrap();
    assert_eq!(
        listed.iter().map(|t| t.slug.as_str()).collect::<Vec<_>>(),
        ["art", "eng"]
    );
    let mut renamed = team("acme", "art");
    renamed.name = "Artists".into();
    renamed.description = "pixels".into();
    assert!(store.update_team(&renamed).await.unwrap());
    assert_eq!(store.team(&acme, "art").await.unwrap(), Some(renamed));
    assert!(!store.update_team(&team("acme", "none")).await.unwrap());
    assert_eq!(store.team(&acme, "none").await.unwrap(), None);

    assert!(
        store
            .add_team_member(&acme, "art", &user("bob"))
            .await
            .unwrap()
    );
    assert!(
        !store
            .add_team_member(&acme, "art", &user("bob"))
            .await
            .unwrap(),
        "adding twice reports no change"
    );
    store
        .add_team_member(&acme, "art", &user("carol"))
        .await
        .unwrap();
    store
        .add_team_member(&acme, "eng", &user("bob"))
        .await
        .unwrap();
    assert!(
        store
            .add_team_member(&acme, "art", &user("stranger"))
            .await
            .is_err(),
        "only organization members join teams"
    );
    assert_eq!(
        store.team_members(&acme, "art").await.unwrap(),
        [user("bob"), user("carol")]
    );
    assert_eq!(
        store.teams_of(&acme, &user("bob")).await.unwrap(),
        ["art", "eng"]
    );

    assert_eq!(
        store
            .set_team_role(&game, &acme, "art", Role::Reader)
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        store
            .set_team_role(&game, &acme, "art", Role::Writer)
            .await
            .unwrap(),
        Some(Role::Reader)
    );
    store
        .set_team_role(&game, &acme, "eng", Role::Maintainer)
        .await
        .unwrap();
    store
        .set_team_role(&tools, &acme, "art", Role::Reader)
        .await
        .unwrap();
    assert_eq!(
        store.team_grants(&game).await.unwrap(),
        [
            ("art".to_string(), Role::Writer),
            ("eng".to_string(), Role::Maintainer)
        ]
    );
    assert_eq!(
        store.team_repos(&acme, "art").await.unwrap(),
        [(game.clone(), Role::Writer), (tools.clone(), Role::Reader)]
    );
    assert_eq!(
        store.team_role_of(&game, &user("bob")).await.unwrap(),
        Some(Role::Maintainer),
        "the highest of several teams"
    );
    assert_eq!(
        store.team_role_of(&game, &user("carol")).await.unwrap(),
        Some(Role::Writer)
    );
    assert_eq!(
        store.team_role_of(&game, &user("alice")).await.unwrap(),
        None
    );
    assert_eq!(
        store.team_repos_of(&user("bob")).await.unwrap(),
        [game.clone(), tools.clone()]
    );
    assert!(
        store
            .team_repos_of(&user("alice"))
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        store.repos_of(&user("carol")).await.unwrap().is_empty(),
        "direct roles are listed apart from team roles"
    );

    assert!(
        store
            .remove_team_member(&acme, "eng", &user("bob"))
            .await
            .unwrap()
    );
    assert!(
        !store
            .remove_team_member(&acme, "eng", &user("bob"))
            .await
            .unwrap()
    );
    assert_eq!(
        store.team_role_of(&game, &user("bob")).await.unwrap(),
        Some(Role::Writer)
    );
    assert_eq!(
        store.remove_team_role(&game, &acme, "eng").await.unwrap(),
        Some(Role::Maintainer)
    );
    assert_eq!(
        store.remove_team_role(&game, &acme, "eng").await.unwrap(),
        None
    );

    store
        .remove_org_member(&acme, &user("carol"), std::slice::from_ref(&game))
        .await
        .unwrap();
    assert_eq!(
        store.team_members(&acme, "art").await.unwrap(),
        [user("bob")]
    );
    assert_eq!(
        store.team_role_of(&game, &user("carol")).await.unwrap(),
        None
    );
    assert!(
        store
            .team_repos_of(&user("carol"))
            .await
            .unwrap()
            .is_empty()
    );

    assert!(store.delete_team(&acme, "art").await.unwrap());
    assert!(!store.delete_team(&acme, "art").await.unwrap());
    assert!(store.team_grants(&game).await.unwrap().is_empty());
    assert!(store.team_members(&acme, "art").await.unwrap().is_empty());
    assert_eq!(store.team_role_of(&game, &user("bob")).await.unwrap(), None);
    assert!(
        store.team(&user("other"), "art").await.unwrap().is_some(),
        "another organization's team stays"
    );

    store.create_team(team("acme", "ops")).await.unwrap();
    store
        .add_team_member(&acme, "ops", &user("bob"))
        .await
        .unwrap();
    store
        .set_team_role(&game, &acme, "ops", Role::Admin)
        .await
        .unwrap();
    store.delete_repo_access(&game).await.unwrap();
    assert!(store.team_grants(&game).await.unwrap().is_empty());
    assert!(store.team_repos_of(&user("bob")).await.unwrap().is_empty());
    assert_eq!(
        store.team_members(&acme, "ops").await.unwrap(),
        [user("bob")],
        "forgetting a repository keeps its teams"
    );

    store
        .set_team_role(&game, &acme, "ops", Role::Reader)
        .await
        .unwrap();
    assert!(store.delete_org(&acme).await.unwrap());
    assert!(store.teams(&acme).await.unwrap().is_empty());
    assert!(store.team_grants(&game).await.unwrap().is_empty());
}

fn session(name: &str) -> Identity {
    Identity {
        user: user(name),
        credential: Credential::Session,
    }
}

fn token_identity(name: &str, permissions: &[Permission]) -> Identity {
    Identity {
        user: user(name),
        credential: Credential::Token(TokenRecord {
            permissions: permissions.iter().copied().collect(),
            repos: Vec::new(),
            ..token("tk", name, 0)
        }),
    }
}

pub async fn effective_roles_resolve_across_direct_team_and_owner_grants(store: Access) {
    let meta = Arc::new(MemoryMetadataStore::new());
    let clock = Arc::new(ManualClock::new(at(0)));
    let access = AccessService::new(store.clone(), clock).with_registry(meta.clone());
    let (acme, alice) = (user("acme"), session("alice"));
    access.create_org(&alice, "acme").await.unwrap();
    for name in ["bob", "carol", "dave", "erin"] {
        store.create_user(&user(name), at(0)).await.unwrap();
    }
    for name in ["bob", "carol", "dave"] {
        access
            .add_org_member(&alice, &acme, &user(name), OrgRole::Member)
            .await
            .unwrap();
    }
    let (game, tools) = (RepoId::new("acme/game"), RepoId::new("acme/tools"));
    for (id, name) in [(&game, "game"), (&tools, "tools")] {
        meta.create_repo(repo_record(id.as_str(), "acme", name))
            .await
            .unwrap();
    }
    let admin = access.principal(&game, &user("alice")).await.unwrap();
    for slug in ["art", "eng"] {
        access
            .create_team(&alice, &acme, slug, None, None)
            .await
            .unwrap();
    }
    let role = |who: &'static str, repo: &RepoId| {
        let (access, repo) = (&access, repo.clone());
        async move { access.effective_role(&repo, &user(who)).await.unwrap() }
    };

    assert_eq!(
        role("bob", &game).await,
        None,
        "membership alone grants nothing"
    );
    assert_eq!(
        role("alice", &game).await,
        Some(Role::Admin),
        "owners are admins"
    );

    access
        .set_user_role(&admin, &game, &user("bob"), Role::Reader)
        .await
        .unwrap();
    assert_eq!(role("bob", &game).await, Some(Role::Reader), "direct only");

    access
        .add_team_member(&alice, &acme, "art", &user("carol"))
        .await
        .unwrap();
    access
        .set_team_access(&admin, &game, "art", Role::Writer)
        .await
        .unwrap();
    assert_eq!(role("carol", &game).await, Some(Role::Writer), "team only");
    assert_eq!(
        role("carol", &tools).await,
        None,
        "a team grant is per repository"
    );

    access
        .add_team_member(&alice, &acme, "art", &user("bob"))
        .await
        .unwrap();
    assert_eq!(
        role("bob", &game).await,
        Some(Role::Writer),
        "the team's writer beats the direct reader"
    );
    access
        .set_user_role(&admin, &game, &user("bob"), Role::Maintainer)
        .await
        .unwrap();
    assert_eq!(
        role("bob", &game).await,
        Some(Role::Maintainer),
        "the direct maintainer beats the team's writer"
    );
    access
        .set_user_role(&admin, &game, &user("bob"), Role::Reader)
        .await
        .unwrap();

    access
        .add_team_member(&alice, &acme, "art", &user("dave"))
        .await
        .unwrap();
    access
        .add_team_member(&alice, &acme, "eng", &user("dave"))
        .await
        .unwrap();
    access
        .set_team_access(&admin, &game, "eng", Role::Maintainer)
        .await
        .unwrap();
    assert_eq!(
        role("dave", &game).await,
        Some(Role::Maintainer),
        "several teams give the highest"
    );

    let listed = access.members(&admin, &game).await.unwrap();
    let member = |name: &str, role, source| RepoMember {
        user: user(name),
        role,
        source,
    };
    assert_eq!(
        listed,
        [
            member("alice", Role::Admin, RoleSource::OrgOwner),
            member("bob", Role::Writer, RoleSource::Team),
            member("carol", Role::Writer, RoleSource::Team),
            member("dave", Role::Maintainer, RoleSource::Team),
        ]
    );
    access
        .set_user_role(&admin, &game, &user("carol"), Role::Writer)
        .await
        .unwrap();
    assert_eq!(
        access.members(&admin, &game).await.unwrap()[2],
        member("carol", Role::Writer, RoleSource::Direct),
        "a tie goes to the direct grant"
    );

    assert_eq!(
        access.repos_of(&user("carol")).await.unwrap(),
        std::slice::from_ref(&game)
    );
    assert_eq!(access.repos_of(&user("erin")).await.unwrap(), []);
    assert_eq!(
        access.repos_of(&user("alice")).await.unwrap(),
        [game.clone(), tools.clone()]
    );

    let token = token_identity(
        "dave",
        &[
            Permission::Read,
            Permission::Lock,
            Permission::ForceUnlock,
            Permission::ManageUsers,
        ],
    );
    let capped = access.principal_in(&game, &token).await.unwrap();
    assert_eq!(
        capped.permissions,
        [Permission::Read, Permission::Lock, Permission::ForceUnlock].into(),
        "a token gets only what the team role and the token share"
    );
    assert!(
        access
            .principal_in(&tools, &token)
            .await
            .unwrap()
            .permissions
            .is_empty()
    );

    let writer_actor = access.principal(&game, &user("carol")).await.unwrap();
    assert!(
        access
            .set_team_access(&writer_actor, &game, "art", Role::Maintainer)
            .await
            .is_err(),
        "nobody grants a role above their own"
    );
    let reach = Principal {
        user: user("carol"),
        permissions: [
            Permission::ManageUsers,
            Permission::Read,
            Permission::Lock,
            Permission::Checkin,
        ]
        .into(),
    };
    assert!(matches!(
        access
            .set_team_access(&reach, &game, "eng", Role::Maintainer)
            .await,
        Err(PynError::Forbidden(_))
    ));
    access
        .set_team_access(&reach, &game, "art", Role::Reader)
        .await
        .unwrap();
    assert_eq!(role("bob", &game).await, Some(Role::Reader));
    access
        .set_team_access(&admin, &game, "art", Role::Writer)
        .await
        .unwrap();

    access
        .remove_team_member(&alice, &acme, "eng", &user("dave"))
        .await
        .unwrap();
    assert_eq!(
        role("dave", &game).await,
        Some(Role::Writer),
        "leaving a team drops its grant"
    );
    access
        .remove_team_access(&admin, &game, "art")
        .await
        .unwrap();
    assert_eq!(
        role("carol", &game).await,
        Some(Role::Writer),
        "carol's direct grant remains"
    );
    assert_eq!(
        role("bob", &game).await,
        Some(Role::Reader),
        "bob falls back to the direct grant"
    );
    assert_eq!(role("dave", &game).await, None);
    access.delete_team(&alice, &acme, "eng").await.unwrap();
    assert!(access.repo_teams(&admin, &game).await.unwrap().is_empty());

    access
        .set_team_access(&admin, &game, "art", Role::Writer)
        .await
        .unwrap();
    access
        .add_team_member(&alice, &acme, "art", &user("dave"))
        .await
        .unwrap();
    access
        .remove_org_member(&alice, &acme, &user("dave"))
        .await
        .unwrap();
    assert_eq!(
        role("dave", &game).await,
        None,
        "leaving the organization drops team access"
    );
    assert_eq!(access.repos_of(&user("dave")).await.unwrap(), []);

    access
        .add_org_member(&alice, &acme, &user("erin"), OrgRole::Owner)
        .await
        .unwrap();
    assert_eq!(role("erin", &tools).await, Some(Role::Admin));
    access
        .add_team_member(&alice, &acme, "art", &user("erin"))
        .await
        .unwrap();
    access
        .set_org_member_role(&alice, &acme, &user("erin"), OrgRole::Member)
        .await
        .unwrap();
    assert_eq!(
        role("erin", &tools).await,
        None,
        "a demoted owner loses the implicit admin"
    );
    assert_eq!(
        role("erin", &game).await,
        Some(Role::Writer),
        "and keeps what a team gives"
    );
    assert_eq!(
        access.repos_of(&user("erin")).await.unwrap(),
        std::slice::from_ref(&game)
    );
}

fn invite(id: &str, created: i64, expires: i64) -> InviteRecord {
    InviteRecord {
        id: InviteId(id.to_string()),
        secret_hash: "11".repeat(32),
        repo: RepoId::new("game"),
        role: Role::Writer,
        created_by: user("alice"),
        created_at: at(created),
        expires_at: at(expires),
        used_at: None,
        used_by: None,
        revoked_at: None,
    }
}

pub async fn invitations_are_single_use_expire_and_can_be_revoked(store: Access) {
    store.ensure_user(&user("alice"), at(0)).await.unwrap();
    for (id, created, expires) in [
        ("aaaaaaaaaaaa", 1, 60),
        ("bbbbbbbbbbbb", 5, 60),
        ("cccccccccccc", 3, 10),
    ] {
        store
            .create_invite(invite(id, created, expires))
            .await
            .unwrap();
    }
    let ids: Vec<_> = store
        .list_invites(&RepoId::new("game"))
        .await
        .unwrap()
        .into_iter()
        .map(|i| i.id.0)
        .collect();
    assert_eq!(
        ids,
        ["bbbbbbbbbbbb", "cccccccccccc", "aaaaaaaaaaaa"],
        "newest first"
    );
    assert!(
        store
            .list_invites(&RepoId::new("other"))
            .await
            .unwrap()
            .is_empty()
    );

    let a = InviteId("aaaaaaaaaaaa".into());
    assert!(store.use_invite(&a, &user("bob"), at(20)).await.unwrap());
    assert!(
        !store.use_invite(&a, &user("carol"), at(21)).await.unwrap(),
        "single use"
    );
    let used = store.get_invite(&a).await.unwrap().unwrap();
    assert_eq!(
        (used.used_at, used.used_by),
        (Some(at(20)), Some(user("bob")))
    );

    let expired = InviteId("cccccccccccc".into());
    assert!(
        !store
            .use_invite(&expired, &user("bob"), at(11))
            .await
            .unwrap(),
        "expired"
    );

    let b = InviteId("bbbbbbbbbbbb".into());
    assert!(store.revoke_invite(&b, at(30)).await.unwrap());
    assert!(store.revoke_invite(&b, at(40)).await.unwrap());
    assert_eq!(
        store.get_invite(&b).await.unwrap().unwrap().revoked_at,
        Some(at(30))
    );
    assert!(
        !store.use_invite(&b, &user("bob"), at(31)).await.unwrap(),
        "revoked"
    );
    assert!(
        !store
            .revoke_invite(&InviteId("ffffffffffff".into()), at(1))
            .await
            .unwrap()
    );
    assert!(
        store
            .get_invite(&InviteId("ffffffffffff".into()))
            .await
            .unwrap()
            .is_none()
    );
}

fn ssh_key(id: &str, owner: &str, fingerprint: &str, created: i64) -> SshKeyRecord {
    SshKeyRecord {
        id: id.to_string(),
        user: user(owner),
        title: format!("key {id}"),
        algorithm: "ssh-ed25519".into(),
        public_key: format!("ssh-ed25519 AAAA{id}"),
        fingerprint: fingerprint.to_string(),
        created_at: at(created),
        last_used_at: None,
    }
}

pub async fn sessions_are_found_deleted_and_swept_when_expired(store: Access) {
    store.ensure_user(&user("alice"), at(0)).await.unwrap();
    let session = |hash: &str, expires: i64| SessionRecord {
        id_hash: hash.to_string(),
        user: user("alice"),
        csrf_token: format!("csrf-{hash}"),
        created_at: at(0),
        expires_at: at(expires),
    };
    store.create_session(session("one", 10)).await.unwrap();
    store.create_session(session("two", 20)).await.unwrap();

    assert_eq!(
        store.get_session("one").await.unwrap(),
        Some(session("one", 10))
    );
    assert!(store.get_session("nope").await.unwrap().is_none());

    store.delete_expired_sessions(at(10)).await.unwrap();
    assert!(store.get_session("one").await.unwrap().is_none(), "expired");
    assert!(store.get_session("two").await.unwrap().is_some());

    assert!(store.delete_session("two").await.unwrap());
    assert!(!store.delete_session("two").await.unwrap());
}

pub async fn ssh_keys_are_unique_across_accounts_and_can_be_found_and_deleted(store: Access) {
    for u in ["alice", "bob"] {
        store.ensure_user(&user(u), at(0)).await.unwrap();
    }
    assert!(
        store
            .add_ssh_key(ssh_key("aaaaaaaaaaaa", "alice", "SHA256:one", 1))
            .await
            .unwrap()
    );
    assert!(
        store
            .add_ssh_key(ssh_key("bbbbbbbbbbbb", "alice", "SHA256:two", 5))
            .await
            .unwrap()
    );
    assert!(
        !store
            .add_ssh_key(ssh_key("cccccccccccc", "bob", "SHA256:one", 6))
            .await
            .unwrap(),
        "another account's key"
    );
    assert!(
        !store
            .add_ssh_key(ssh_key("dddddddddddd", "alice", "SHA256:one", 7))
            .await
            .unwrap(),
        "the same key twice"
    );

    let ids: Vec<_> = store
        .list_ssh_keys(&user("alice"))
        .await
        .unwrap()
        .into_iter()
        .map(|k| k.id)
        .collect();
    assert_eq!(ids, ["bbbbbbbbbbbb", "aaaaaaaaaaaa"], "newest first");
    assert!(store.list_ssh_keys(&user("bob")).await.unwrap().is_empty());

    let found = store.find_ssh_key("SHA256:one").await.unwrap().unwrap();
    assert_eq!((found.user, found.last_used_at), (user("alice"), None));
    store.touch_ssh_key("SHA256:one", at(30)).await.unwrap();
    assert_eq!(
        store
            .find_ssh_key("SHA256:one")
            .await
            .unwrap()
            .unwrap()
            .last_used_at,
        Some(at(30))
    );
    assert!(store.find_ssh_key("SHA256:nope").await.unwrap().is_none());

    assert!(
        !store
            .delete_ssh_key(&user("bob"), "aaaaaaaaaaaa")
            .await
            .unwrap(),
        "not bob's key"
    );
    assert!(
        store
            .delete_ssh_key(&user("alice"), "aaaaaaaaaaaa")
            .await
            .unwrap()
    );
    assert!(
        !store
            .delete_ssh_key(&user("alice"), "aaaaaaaaaaaa")
            .await
            .unwrap()
    );
    assert!(store.find_ssh_key("SHA256:one").await.unwrap().is_none());
    assert!(
        store
            .add_ssh_key(ssh_key("eeeeeeeeeeee", "bob", "SHA256:one", 40))
            .await
            .unwrap(),
        "a removed key can be linked again"
    );
}

pub async fn repos_of_lists_a_users_roles_and_forgetting_a_repo_clears_its_access(store: Access) {
    let (game, other) = (RepoId::new("game"), RepoId::new("other"));
    for u in ["alice", "bob"] {
        store.ensure_user(&user(u), at(0)).await.unwrap();
    }
    store
        .set_role(&game, &user("alice"), Role::Admin)
        .await
        .unwrap();
    store
        .set_role(&other, &user("alice"), Role::Reader)
        .await
        .unwrap();
    store
        .set_role(&game, &user("bob"), Role::Writer)
        .await
        .unwrap();
    store
        .set_role_permissions(&game, Role::Writer, [Permission::Read].into())
        .await
        .unwrap();
    store
        .create_invite(invite("aaaaaaaaaaaa", 1, 60))
        .await
        .unwrap();

    assert_eq!(
        store.repos_of(&user("alice")).await.unwrap(),
        [game.clone(), other.clone()]
    );
    assert_eq!(
        store.repos_of(&user("bob")).await.unwrap(),
        std::slice::from_ref(&game)
    );

    store.delete_repo_access(&game).await.unwrap();
    assert!(store.members(&game).await.unwrap().is_empty());
    assert_eq!(
        store.repos_of(&user("alice")).await.unwrap(),
        std::slice::from_ref(&other)
    );
    assert!(store.repos_of(&user("bob")).await.unwrap().is_empty());
    assert_eq!(
        store.role_definitions(&game).await.unwrap(),
        RoleDefinitions::defaults()
    );
    assert!(store.list_invites(&game).await.unwrap().is_empty());
    assert_eq!(store.members(&other).await.unwrap().len(), 1);
}

fn new_account(name: &str, email: &str, minute: i64) -> NewAccount {
    NewAccount {
        user: user(name),
        email: Some(email.to_string()),
        password_hash: format!("hash-{name}"),
        signup: SignupStage::PendingVerification,
        created_at: at(minute),
    }
}

pub async fn self_registered_accounts_start_pending_and_keep_their_password(store: Access) {
    assert!(
        store
            .create_account(new_account("alice", "a@example.org", 0))
            .await
            .unwrap()
    );
    assert!(
        !store
            .create_account(new_account("alice", "other@example.org", 1))
            .await
            .unwrap(),
        "a name can be taken once"
    );
    let account = store.account(&user("alice")).await.unwrap().unwrap();
    assert_eq!(account.email.as_deref(), Some("a@example.org"));
    assert_eq!(account.status(), AccountStatus::PendingVerification);
    assert!(account.email_verified_at.is_none() && !account.is_admin);
    assert_eq!(
        store
            .password_hash(&user("alice"))
            .await
            .unwrap()
            .as_deref(),
        Some("hash-alice")
    );
    assert!(store.account(&user("nobody")).await.unwrap().is_none());

    store.ensure_user(&user("bob"), at(2)).await.unwrap();
    let bob = store.account(&user("bob")).await.unwrap().unwrap();
    assert_eq!(bob.status(), AccountStatus::Active);
    assert!(bob.email.is_none());
}

pub async fn a_verified_address_belongs_to_one_account(store: Access) {
    for (name, minute) in [("alice", 0), ("bob", 1)] {
        store
            .create_account(new_account(name, "same@example.org", minute))
            .await
            .unwrap();
    }
    assert_eq!(
        store
            .pending_by_email("same@example.org")
            .await
            .unwrap()
            .len(),
        2,
        "unverified addresses can repeat"
    );
    assert!(
        store
            .verified_email_owner("same@example.org")
            .await
            .unwrap()
            .is_none()
    );

    assert!(
        store
            .complete_verification(&user("alice"), SignupStage::Complete, at(5))
            .await
            .unwrap()
    );
    assert_eq!(
        store
            .verified_email_owner("same@example.org")
            .await
            .unwrap(),
        Some(user("alice"))
    );
    let alice = store.account(&user("alice")).await.unwrap().unwrap();
    assert_eq!(alice.email_verified_at, Some(at(5)));
    assert_eq!(alice.status(), AccountStatus::Active);
    assert!(
        !store
            .complete_verification(&user("bob"), SignupStage::Complete, at(6))
            .await
            .unwrap(),
        "the address is already verified elsewhere"
    );
    let pending = store.pending_by_email("same@example.org").await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].user, user("bob"));
    assert!(
        !store
            .complete_verification(&user("nobody"), SignupStage::Complete, at(6))
            .await
            .unwrap()
    );
}

pub async fn accounts_can_be_approved_disabled_enabled_and_listed(store: Access) {
    for (name, minute) in [("alice", 0), ("bob", 1), ("carol", 2)] {
        store.ensure_user(&user(name), at(minute)).await.unwrap();
    }
    assert!(
        store
            .set_signup(&user("bob"), SignupStage::PendingApproval)
            .await
            .unwrap()
    );
    assert!(
        store
            .set_disabled(&user("carol"), Some((at(9), Some("spam".into()))))
            .await
            .unwrap()
    );
    assert!(
        !store
            .set_disabled(&user("nobody"), Some((at(9), None)))
            .await
            .unwrap()
    );
    store.set_admin(&user("alice"), true).await.unwrap();

    let carol = store.account(&user("carol")).await.unwrap().unwrap();
    assert_eq!(carol.status(), AccountStatus::Disabled);
    assert_eq!(carol.disabled_at, Some(at(9)));
    assert_eq!(carol.disabled_reason.as_deref(), Some("spam"));
    assert!(
        store
            .account(&user("alice"))
            .await
            .unwrap()
            .unwrap()
            .is_admin
    );

    let names = |list: Vec<crate::AccountRecord>| -> Vec<String> {
        list.into_iter().map(|a| a.user.to_string()).collect()
    };
    assert_eq!(
        names(store.list_accounts(None, 10).await.unwrap()),
        ["alice", "bob", "carol"]
    );
    assert_eq!(
        names(
            store
                .list_accounts(Some(AccountStatus::PendingApproval), 10)
                .await
                .unwrap()
        ),
        ["bob"]
    );
    assert_eq!(
        names(
            store
                .list_accounts(Some(AccountStatus::Disabled), 10)
                .await
                .unwrap()
        ),
        ["carol"]
    );
    assert_eq!(
        names(
            store
                .list_accounts(Some(AccountStatus::Active), 10)
                .await
                .unwrap()
        ),
        ["alice"]
    );
    assert_eq!(store.list_accounts(None, 2).await.unwrap().len(), 2);

    assert!(store.set_disabled(&user("carol"), None).await.unwrap());
    let carol = store.account(&user("carol")).await.unwrap().unwrap();
    assert_eq!(carol.status(), AccountStatus::Active);
    assert!(carol.disabled_at.is_none() && carol.disabled_reason.is_none());
}

fn verification(hash: &str, name: &str, expires: i64) -> VerificationRecord {
    VerificationRecord {
        token_hash: hash.to_string(),
        user: user(name),
        email: format!("{name}@example.org"),
        expires_at: at(expires),
    }
}

pub async fn verifications_are_single_use_and_replace_earlier_ones(store: Access) {
    store
        .create_account(new_account("alice", "alice@example.org", 0))
        .await
        .unwrap();
    store
        .put_verification(verification("one", "alice", 60))
        .await
        .unwrap();
    store
        .put_verification(verification("two", "alice", 60))
        .await
        .unwrap();
    assert!(
        store.take_verification("one").await.unwrap().is_none(),
        "a new verification replaces the old one"
    );
    assert_eq!(
        store.take_verification("two").await.unwrap(),
        Some(verification("two", "alice", 60))
    );
    assert!(store.take_verification("two").await.unwrap().is_none());
}

pub async fn unverified_accounts_with_no_live_verification_are_swept(store: Access) {
    for (name, minute) in [("alice", 0), ("bob", 0), ("carol", 0), ("dave", 0)] {
        store
            .create_account(new_account(name, &format!("{name}@example.org"), minute))
            .await
            .unwrap();
    }
    store
        .put_verification(verification("a", "alice", 10))
        .await
        .unwrap();
    store
        .put_verification(verification("b", "bob", 100))
        .await
        .unwrap();
    store
        .complete_verification(&user("carol"), SignupStage::Complete, at(1))
        .await
        .unwrap();
    store
        .set_disabled(&user("dave"), Some((at(1), None)))
        .await
        .unwrap();
    store.ensure_user(&user("erin"), at(0)).await.unwrap();
    store
        .create_account(new_account("frank", "frank@example.org", 40))
        .await
        .unwrap();
    store
        .create_account(new_account("gina", "gina@example.org", 0))
        .await
        .unwrap();

    store.delete_stale_pending(at(50), at(30)).await.unwrap();
    assert!(store.user_exists(&user("frank")).await.unwrap(), "recent");
    assert!(
        !store.user_exists(&user("gina")).await.unwrap(),
        "old and no verification"
    );
    assert!(!store.user_exists(&user("alice")).await.unwrap(), "expired");
    assert!(store.user_exists(&user("bob")).await.unwrap(), "still live");
    assert!(store.user_exists(&user("carol")).await.unwrap(), "verified");
    assert!(store.user_exists(&user("dave")).await.unwrap(), "disabled");
    assert!(
        store.user_exists(&user("erin")).await.unwrap(),
        "not pending"
    );
    assert!(store.take_verification("a").await.unwrap().is_none());
    assert!(store.take_verification("b").await.unwrap().is_some());
    assert!(
        store.password_hash(&user("alice")).await.unwrap().is_none(),
        "the swept account's password goes with it"
    );
}

pub async fn deleting_an_accounts_sessions_leaves_others(store: Access) {
    for name in ["alice", "bob"] {
        store.ensure_user(&user(name), at(0)).await.unwrap();
        store
            .create_session(SessionRecord {
                id_hash: format!("s-{name}"),
                user: user(name),
                csrf_token: "csrf".into(),
                created_at: at(0),
                expires_at: at(60),
            })
            .await
            .unwrap();
    }
    store.delete_sessions_of(&user("alice")).await.unwrap();
    assert!(store.get_session("s-alice").await.unwrap().is_none());
    assert!(store.get_session("s-bob").await.unwrap().is_some());
}

pub async fn rate_windows_count_expire_and_reset(store: RateLimits) {
    let window = Duration::minutes(10);
    assert!(store.state("k", at(0)).await.unwrap().is_none());
    let first = store.hit("k", window, at(0)).await.unwrap();
    assert_eq!((first.count, first.resets_at), (1, at(10)));
    let second = store.hit("k", window, at(4)).await.unwrap();
    assert_eq!(
        (second.count, second.resets_at),
        (2, at(10)),
        "the window keeps its start"
    );
    assert_eq!(store.state("k", at(5)).await.unwrap(), Some(second));
    assert_eq!(store.hit("other", window, at(5)).await.unwrap().count, 1);

    assert!(store.state("k", at(10)).await.unwrap().is_none(), "ended");
    let again = store.hit("k", window, at(10)).await.unwrap();
    assert_eq!((again.count, again.resets_at), (1, at(20)));

    store.reset("k").await.unwrap();
    assert!(store.state("k", at(11)).await.unwrap().is_none());
    store.reset("k").await.unwrap();

    store.sweep(at(15)).await.unwrap();
    assert!(store.state("other", at(11)).await.unwrap().is_none());
}

pub async fn racing_hits_are_all_counted(store: RateLimits) {
    let tasks: Vec<_> = (0..20)
        .map(|_| {
            let store = store.clone();
            tokio::spawn(async move {
                store
                    .hit("race", Duration::minutes(10), at(0))
                    .await
                    .unwrap()
            })
        })
        .collect();
    let mut counts = Vec::new();
    for t in tasks {
        counts.push(t.await.unwrap().count);
    }
    counts.sort();
    assert_eq!(counts, (1..=20).collect::<Vec<_>>());
}

/// Generates one `#[tokio::test]` per access-store case for the store built by `$factory`.
#[macro_export]
macro_rules! access_contract_tests {
    ($factory:expr $(, #[$attr:meta])*) => {
        $crate::access_contract_tests!(@one $factory; [$(#[$attr])*]; tokens_round_trip_and_list_newest_first);
        $crate::access_contract_tests!(@one $factory; [$(#[$attr])*]; revoking_is_recorded_and_harmless_twice);
        $crate::access_contract_tests!(@one $factory; [$(#[$attr])*]; touching_a_token_records_last_use);
        $crate::access_contract_tests!(@one $factory; [$(#[$attr])*]; roles_assign_and_overwrite);
        $crate::access_contract_tests!(@one $factory; [$(#[$attr])*]; role_definitions_apply_overrides_to_defaults);
        $crate::access_contract_tests!(@one $factory; [$(#[$attr])*]; accounts_hold_a_unique_name_and_a_password);
        $crate::access_contract_tests!(@one $factory; [$(#[$attr])*]; invitations_are_single_use_expire_and_can_be_revoked);
        $crate::access_contract_tests!(@one $factory; [$(#[$attr])*]; ssh_keys_are_unique_across_accounts_and_can_be_found_and_deleted);
        $crate::access_contract_tests!(@one $factory; [$(#[$attr])*]; sessions_are_found_deleted_and_swept_when_expired);
        $crate::access_contract_tests!(@one $factory; [$(#[$attr])*]; repos_of_lists_a_users_roles_and_forgetting_a_repo_clears_its_access);
        $crate::access_contract_tests!(@one $factory; [$(#[$attr])*]; self_registered_accounts_start_pending_and_keep_their_password);
        $crate::access_contract_tests!(@one $factory; [$(#[$attr])*]; a_verified_address_belongs_to_one_account);
        $crate::access_contract_tests!(@one $factory; [$(#[$attr])*]; accounts_can_be_approved_disabled_enabled_and_listed);
        $crate::access_contract_tests!(@one $factory; [$(#[$attr])*]; verifications_are_single_use_and_replace_earlier_ones);
        $crate::access_contract_tests!(@one $factory; [$(#[$attr])*]; unverified_accounts_with_no_live_verification_are_swept);
        $crate::access_contract_tests!(@one $factory; [$(#[$attr])*]; deleting_an_accounts_sessions_leaves_others);
        $crate::access_contract_tests!(@one $factory; [$(#[$attr])*]; organizations_share_the_account_namespace_and_start_with_their_creator_as_owner);
        $crate::access_contract_tests!(@one $factory; [$(#[$attr])*]; deleting_an_organization_removes_its_members_and_frees_the_name);
        $crate::access_contract_tests!(@one $factory; [$(#[$attr])*]; organization_members_are_added_changed_and_never_left_without_an_owner);
        $crate::access_contract_tests!(@one $factory; [$(#[$attr])*]; removing_an_organization_member_drops_their_roles_in_the_given_repositories);
        $crate::access_contract_tests!(@one $factory; [$(#[$attr])*]; teams_hold_members_and_repository_roles_until_removed);
        $crate::access_contract_tests!(@one $factory; [$(#[$attr])*]; effective_roles_resolve_across_direct_team_and_owner_grants);
    };
    (@one $factory:expr; [$(#[$attr:meta])*]; $name:ident) => {
        #[tokio::test(flavor = "multi_thread")]
        $(#[$attr])*
        async fn $name() {
            let store: $crate::contract::Access = ($factory).await;
            $crate::contract::$name(store).await;
        }
    };
}

fn event(
    actor: &str,
    action: AuditAction,
    path: Option<&str>,
    minute: i64,
) -> crate::NewAuditEvent {
    crate::NewAuditEvent {
        at: at(minute),
        actor: user(actor),
        action,
        path: path.map(|p| RepoPath::new(p).unwrap()),
        detail: format!("{action} by {actor}"),
    }
}

pub async fn audit_events_list_newest_first_with_filters(store: Audit) {
    let repo = AuditScope::Repo(RepoId::new("game"));
    for (i, (who, action, p)) in [
        ("alice", AuditAction::Checkout, Some("Content/a.umap")),
        ("alice", AuditAction::Checkin, Some("Content/a.umap")),
        ("bob", AuditAction::Checkout, Some("Content/b.umap")),
        ("maya", AuditAction::ForceUnlock, Some("Content/b.umap")),
    ]
    .into_iter()
    .enumerate()
    {
        store
            .record(&repo, event(who, action, p, i as i64))
            .await
            .unwrap();
    }
    store
        .record(
            &AuditScope::Repo(RepoId::new("other")),
            event("zed", AuditAction::Checkout, None, 9),
        )
        .await
        .unwrap();

    let all = store
        .list(
            &repo,
            &AuditQuery {
                limit: 10,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        all.iter().map(|e| e.actor.as_str()).collect::<Vec<_>>(),
        ["maya", "bob", "alice", "alice"]
    );
    assert!(all.windows(2).all(|w| w[0].id > w[1].id), "ids descend");
    assert_eq!(all[0].detail, "force_unlock by maya");

    let by = |q: AuditQuery| {
        let store = store.clone();
        let repo = repo.clone();
        async move {
            store
                .list(&repo, &AuditQuery { limit: 10, ..q })
                .await
                .unwrap()
                .len()
        }
    };
    assert_eq!(
        by(AuditQuery {
            actor: Some(user("alice")),
            ..Default::default()
        })
        .await,
        2
    );
    assert_eq!(
        by(AuditQuery {
            action: Some(AuditAction::Checkout),
            ..Default::default()
        })
        .await,
        2
    );
    assert_eq!(
        by(AuditQuery {
            path: Some(path("Content/b.umap")),
            ..Default::default()
        })
        .await,
        2
    );
    assert_eq!(
        by(AuditQuery {
            actor: Some(user("alice")),
            action: Some(AuditAction::Checkout),
            ..Default::default()
        })
        .await,
        1
    );
}

pub async fn audit_events_page_backwards(store: Audit) {
    let repo = AuditScope::Repo(RepoId::new("game"));
    for i in 0..5 {
        store
            .record(
                &repo,
                event("alice", AuditAction::Checkout, Some("Content/a.umap"), i),
            )
            .await
            .unwrap();
    }
    let first = store
        .list(
            &repo,
            &AuditQuery {
                limit: 2,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let second = store
        .list(
            &repo,
            &AuditQuery {
                limit: 2,
                before: Some(first[1].id),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let rest = store
        .list(
            &repo,
            &AuditQuery {
                limit: 10,
                before: Some(second[1].id),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let ids: Vec<i64> = first
        .iter()
        .chain(&second)
        .chain(&rest)
        .map(|e| e.id)
        .collect();
    assert_eq!(ids.len(), 5);
    assert!(
        ids.windows(2).all(|w| w[0] > w[1]),
        "no event repeats or is skipped: {ids:?}"
    );
}

pub async fn audit_scopes_keep_repository_organization_and_server_logs_apart(store: Audit) {
    let scopes = [
        AuditScope::Repo(RepoId::new("game")),
        AuditScope::Org(user("game")),
        AuditScope::Server,
    ];
    let actions = [
        AuditAction::RepoCreated,
        AuditAction::OrgCreated,
        AuditAction::AccountApproved,
    ];
    for (scope, action) in scopes.iter().zip(actions) {
        store
            .record(scope, event("alice", action, None, 0))
            .await
            .unwrap();
    }
    store
        .record(&scopes[1], event("alice", AuditAction::OrgDeleted, None, 1))
        .await
        .unwrap();

    for (scope, expected) in scopes.iter().zip([
        vec![AuditAction::RepoCreated],
        vec![AuditAction::OrgDeleted, AuditAction::OrgCreated],
        vec![AuditAction::AccountApproved],
    ]) {
        let found: Vec<_> = store
            .list(
                scope,
                &AuditQuery {
                    limit: 10,
                    ..Default::default()
                },
            )
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.action)
            .collect();
        assert_eq!(found, expected, "{scope:?}");
    }
    let none = store
        .list(
            &AuditScope::Org(user("other")),
            &AuditQuery {
                limit: 10,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(none.is_empty());
}

/// Generates one `#[tokio::test]` per audit-store case for the store built by `$factory`.
#[macro_export]
macro_rules! audit_contract_tests {
    ($factory:expr $(, #[$attr:meta])*) => {
        $crate::audit_contract_tests!(@one $factory; [$(#[$attr])*]; audit_events_list_newest_first_with_filters);
        $crate::audit_contract_tests!(@one $factory; [$(#[$attr])*]; audit_events_page_backwards);
        $crate::audit_contract_tests!(@one $factory; [$(#[$attr])*]; audit_scopes_keep_repository_organization_and_server_logs_apart);
    };
    (@one $factory:expr; [$(#[$attr:meta])*]; $name:ident) => {
        #[tokio::test(flavor = "multi_thread")]
        $(#[$attr])*
        async fn $name() {
            let store: $crate::contract::Audit = ($factory).await;
            $crate::contract::$name(store).await;
        }
    };
}

/// Generates one `#[tokio::test]` per rate-limit-store case for the store built by `$factory`.
#[macro_export]
macro_rules! rate_limit_contract_tests {
    ($factory:expr $(, #[$attr:meta])*) => {
        $crate::rate_limit_contract_tests!(@one $factory; [$(#[$attr])*]; rate_windows_count_expire_and_reset);
        $crate::rate_limit_contract_tests!(@one $factory; [$(#[$attr])*]; racing_hits_are_all_counted);
    };
    (@one $factory:expr; [$(#[$attr:meta])*]; $name:ident) => {
        #[tokio::test(flavor = "multi_thread")]
        $(#[$attr])*
        async fn $name() {
            let store: $crate::contract::RateLimits = ($factory).await;
            $crate::contract::$name(store).await;
        }
    };
}
