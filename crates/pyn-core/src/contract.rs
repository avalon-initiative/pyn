//! Behaviour every `MetadataStore` must satisfy, run against the store passed in.

use std::sync::Arc;

use chrono::{Duration, TimeZone, Utc};

use crate::memory::MemoryObjectStore;
use crate::{
    ContentHash, ManualClock, MetadataStore, ObjectStore, PynError, RepoId, RepoPath, RepoService,
    RevisionId, Rules, ServiceConfig, UserId,
};

pub type Store = Arc<dyn MetadataStore>;

const RULES: &str = "[meta]\ndefault = \"shared\"\n[exclusive]\npaths = [\"Content/\"]\n";

struct Harness {
    svc: Arc<RepoService>,
    objects: Arc<MemoryObjectStore>,
    clock: Arc<ManualClock>,
}

fn harness(store: Store) -> Harness {
    let objects = Arc::new(MemoryObjectStore::new());
    let clock = Arc::new(ManualClock::new(
        Utc.with_ymd_and_hms(2026, 10, 8, 9, 0, 0).unwrap(),
    ));
    let svc = Arc::new(RepoService::new(
        RepoId::new("game"),
        Rules::from_toml(RULES).unwrap(),
        store,
        objects.clone(),
        clock.clone(),
        ServiceConfig::default(),
    ));
    Harness {
        svc,
        objects,
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
