//! The lock/checkin contract. These are the behaviours the whole product exists for.

use std::sync::Arc;

use chrono::{Duration, TimeZone, Utc};
use pyn_core::memory::{MemoryMetadataStore, MemoryObjectStore};
use pyn_core::{
    ContentHash, ManualClock, ObjectStore, PynError, RepoId, RepoPath, RepoService, RevisionId,
    Rules, ServiceConfig, UserId,
};

const RULES: &str = "[meta]\ndefault = \"shared\"\n[exclusive]\npaths = [\"Content/\"]\n";

struct Harness {
    svc: Arc<RepoService>,
    objects: Arc<MemoryObjectStore>,
    clock: Arc<ManualClock>,
}

fn harness() -> Harness {
    let objects = Arc::new(MemoryObjectStore::new());
    let clock = Arc::new(ManualClock::new(
        Utc.with_ymd_and_hms(2026, 10, 8, 9, 0, 0).unwrap(),
    ));
    let svc = Arc::new(RepoService::new(
        RepoId::new("game"),
        Rules::from_toml(RULES).unwrap(),
        Arc::new(MemoryMetadataStore::new()),
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

#[tokio::test]
async fn second_user_cannot_take_a_held_lock() {
    let h = harness();
    let map = path("Content/Dungeon.umap");
    h.svc.checkout(&map, &user("alice"), None).await.unwrap();
    let err = h.svc.checkout(&map, &user("bob"), None).await.unwrap_err();
    assert!(
        matches!(err, PynError::LockHeld { ref owner, .. } if owner == &user("alice")),
        "{err}"
    );
}

#[tokio::test]
async fn racing_checkouts_have_exactly_one_winner() {
    let h = harness();
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

#[tokio::test]
async fn holder_can_renew_and_the_lease_moves_forward() {
    let h = harness();
    let map = path("Content/Dungeon.umap");
    let first = h.svc.checkout(&map, &user("alice"), None).await.unwrap();
    h.clock.advance(Duration::hours(3));
    let renewed = h.svc.checkout(&map, &user("alice"), None).await.unwrap();
    assert_eq!(renewed.acquired_at, first.acquired_at);
    assert_eq!(renewed.expires_at, first.expires_at + Duration::hours(3));
}

#[tokio::test]
async fn expired_lease_frees_the_file() {
    let h = harness();
    let map = path("Content/Dungeon.umap");
    h.svc.checkout(&map, &user("alice"), None).await.unwrap();
    h.clock.advance(Duration::hours(8));
    assert!(h.svc.locks().await.unwrap().is_empty());
    h.svc.checkout(&map, &user("bob"), None).await.unwrap();
}

#[tokio::test]
async fn expired_holder_cannot_check_in() {
    let h = harness();
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

#[tokio::test]
async fn exclusive_checkin_requires_the_lock() {
    let h = harness();
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

#[tokio::test]
async fn checkin_creates_a_revision_and_releases_the_lock() {
    let h = harness();
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

#[tokio::test]
async fn checkout_from_a_stale_copy_is_refused() {
    let h = harness();
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

#[tokio::test]
async fn shared_paths_cannot_be_checked_out_and_reject_stale_checkins() {
    let h = harness();
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

#[tokio::test]
async fn release_is_holder_only() {
    let h = harness();
    let map = path("Content/Dungeon.umap");
    h.svc.checkout(&map, &user("alice"), None).await.unwrap();
    assert!(matches!(
        h.svc.release(&map, &user("bob")).await,
        Err(PynError::NotLockHolder(_))
    ));
    h.svc.release(&map, &user("alice")).await.unwrap();
    h.svc.checkout(&map, &user("bob"), None).await.unwrap();
}

#[tokio::test]
async fn checkin_of_unknown_content_is_refused() {
    let h = harness();
    let src = path("Source/a.cpp");
    let ghost = ContentHash::new("00".repeat(32));
    let err = h
        .svc
        .checkin(&src, &user("alice"), ghost, None, "x".into())
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::ObjectMissing(_)), "{err}");
}

#[test]
fn paths_cannot_escape_or_alias() {
    for bad in ["", "/etc/passwd", "a/../b", "a//b", "a/", "./a", "a\\b"] {
        assert!(RepoPath::new(bad).is_err(), "{bad:?} should be rejected");
    }
    assert!(RepoPath::new("Content/World/Main.umap").is_ok());
}
