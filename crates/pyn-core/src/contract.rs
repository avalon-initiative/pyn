//! Behaviour every `MetadataStore` must satisfy, run against the store passed in.

use std::sync::Arc;

use chrono::{Duration, TimeZone, Utc};

use crate::AccessStore;
use crate::access::{
    InviteId, InviteRecord, Permission, Role, RoleDefinitions, TokenId, TokenRecord,
};
use crate::memory::{MemoryAuditStore, MemoryObjectStore};
use crate::{
    AuditAction, AuditQuery, AuditStore, ContentHash, ManualClock, MetadataStore, ObjectStore,
    PynError, RepoId, RepoPath, RepoService, RevisionId, Rules, ServiceConfig, UserId,
};

pub type Store = Arc<dyn MetadataStore>;
pub type Access = Arc<dyn AccessStore>;
pub type Audit = Arc<dyn AuditStore>;

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
        .checkin(&map, &user("alice"), c, None, "late".into())
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
        .checkin(&map, &user("alice"), c.clone(), None, "sneaky".into())
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::LockRequired(_)), "{err}");

    h.svc.checkout(&map, &user("alice"), None).await.unwrap();
    let err = h
        .svc
        .checkin(&map, &user("bob"), c, None, "bypass".into())
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
        .checkin(&map, &user("alice"), c1, None, "first".into())
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
        .checkin(&map, &user("bob"), c2, Some(RevisionId(1)), "second".into())
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
        .checkin(&map, &user("alice"), c, None, "first".into())
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
        .checkin(&src, &user("alice"), c1, None, "a".into())
        .await
        .unwrap();
    let c2 = blob(&h, "b").await;
    let err = h
        .svc
        .checkin(&src, &user("bob"), c2, None, "b".into())
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
        .checkin(&src, &user("alice"), ghost, None, "x".into())
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
            .checkin(&a, &user("alice"), c, base, text.into())
            .await
            .unwrap();
    }
    h.svc.checkout(&n, &user("bob"), None).await.unwrap();
    let c = blob(&h, "n").await;
    h.svc
        .checkin(&n, &user("bob"), c, None, "n".into())
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

pub async fn old_revisions_can_be_read_back(store: Store) {
    let h = harness(store);
    let a = path("Source/a.cpp");
    for (i, text) in ["one", "two", "three"].into_iter().enumerate() {
        let c = blob(&h, text).await;
        let base = (i > 0).then_some(RevisionId(i as u64));
        h.svc
            .checkin(&a, &user("alice"), c, base, text.into())
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
            .checkin(path, &user("alice"), c, base, (*text).into())
            .await
            .unwrap();
    }
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
            &m,
            &user("bob"),
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
        .checkin(&m, &user("alice"), c, Some(RevisionId(4)), "edit".into())
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
    let restore = |who: &str, confirm: &str| {
        let (svc, who, confirm) = (h.svc.clone(), user(who), confirm.to_string());
        let m = m.clone();
        async move {
            svc.restore(&m, &who, RevisionId(1), RevisionId(2), &confirm, None)
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
            .checkin(&s, &user("alice"), c, base, text.into())
            .await
            .unwrap();
    }
    let err = h
        .svc
        .restore(
            &s,
            &user("alice"),
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
            &m,
            &user("alice"),
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
            &m,
            &user("alice"),
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

pub async fn operations_are_recorded_in_the_audit_log(store: Store) {
    let h = harness(store);
    let m = path("Content/m.umap");
    h.svc.checkout(&m, &user("alice"), None).await.unwrap();
    let c = blob(&h, "one").await;
    h.svc
        .checkin(&m, &user("alice"), c, None, "first".into())
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
                &RepoId::new("game"),
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

/// Generates one `#[tokio::test]` per contract case for the store built by `$factory`.
#[macro_export]
macro_rules! contract_tests {
    ($factory:expr $(, #[$attr:meta])*) => {
        $crate::contract_tests!(@one $factory; [$(#[$attr])*]; second_user_cannot_take_a_held_lock);
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
        $crate::contract_tests!(@one $factory; [$(#[$attr])*]; old_revisions_can_be_read_back);
        $crate::contract_tests!(@one $factory; [$(#[$attr])*]; restore_appends_a_checkpoint_with_the_old_content);
        $crate::contract_tests!(@one $factory; [$(#[$attr])*]; restore_needs_the_lock_and_the_right_confirmation);
        $crate::contract_tests!(@one $factory; [$(#[$attr])*]; restore_is_for_exclusive_paths_and_real_older_revisions);
        $crate::contract_tests!(@one $factory; [$(#[$attr])*]; force_unlock_removes_a_live_lock_and_says_why);
        $crate::contract_tests!(@one $factory; [$(#[$attr])*]; operations_are_recorded_in_the_audit_log);
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
    let repo = RepoId::new("game");
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
            &RepoId::new("other"),
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
    let repo = RepoId::new("game");
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

/// Generates one `#[tokio::test]` per audit-store case for the store built by `$factory`.
#[macro_export]
macro_rules! audit_contract_tests {
    ($factory:expr $(, #[$attr:meta])*) => {
        $crate::audit_contract_tests!(@one $factory; [$(#[$attr])*]; audit_events_list_newest_first_with_filters);
        $crate::audit_contract_tests!(@one $factory; [$(#[$attr])*]; audit_events_page_backwards);
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
