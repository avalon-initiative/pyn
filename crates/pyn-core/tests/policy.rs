use std::collections::BTreeSet;
use std::sync::Arc;

use chrono::{TimeZone, Utc};
use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore,
};
use pyn_core::{
    AccessService, AuditAction, AuditQuery, AuditStore, ContentHash, Credential, Identity,
    ManualClock, MetadataStore, Mode, NewRevision, ObjectStore, Permission, Principal, PynError,
    RepoPath, RepoService, Repositories, RevisionId, Rules, UserId,
};

const POLICY: &str = ".pyn/pyn.toml";
const SHARED_POLICY: &str = "[meta]\ndefault = \"shared\"\n[exclusive]\npaths = [\"Content/\"]\n[shared]\npaths = [\".pyn/pyn.toml\"]\n";
const EXCLUSIVE_POLICY: &str =
    "[meta]\ndefault = \"shared\"\n[exclusive]\npaths = [\"Content/\"]\n";

struct World {
    meta: Arc<MemoryMetadataStore>,
    objects: Arc<MemoryObjectStore>,
    audit: Arc<MemoryAuditStore>,
    access: Arc<AccessService>,
    clock: Arc<ManualClock>,
    repos: Repositories,
}

fn build(w: &World, fallback: Rules) -> Repositories {
    Repositories::new(
        w.meta.clone(),
        w.objects.clone(),
        w.audit.clone(),
        w.access.clone(),
        w.clock.clone(),
        fallback,
    )
}

fn world() -> World {
    let clock = Arc::new(ManualClock::new(
        Utc.with_ymd_and_hms(2026, 10, 8, 9, 0, 0).unwrap(),
    ));
    let audit = Arc::new(MemoryAuditStore::new());
    let access = Arc::new(
        AccessService::new(Arc::new(MemoryAccessStore::new()), clock.clone())
            .with_audit(audit.clone()),
    );
    let meta = Arc::new(MemoryMetadataStore::new());
    let objects = Arc::new(MemoryObjectStore::new());
    let repos = Repositories::new(
        meta.clone(),
        objects.clone(),
        audit.clone(),
        access.clone(),
        clock.clone(),
        Rules::empty(),
    );
    World {
        meta,
        objects,
        audit,
        access,
        clock,
        repos,
    }
}

fn path(s: &str) -> RepoPath {
    RepoPath::new(s).unwrap()
}

fn admin(name: &str) -> Principal {
    Principal::unrestricted(UserId::new(name))
}

fn writer(name: &str) -> Principal {
    Principal {
        user: UserId::new(name),
        permissions: BTreeSet::from([Permission::Read, Permission::Lock, Permission::Checkin]),
    }
}

async fn open(w: &World, repo: &str) -> Arc<RepoService> {
    w.repos.open("alice", repo).await.unwrap().service
}

async fn make_repo(w: &World, repo: &str) -> Arc<RepoService> {
    let alice = Identity {
        user: UserId::new("alice"),
        credential: Credential::Session,
    };
    w.repos
        .create(&alice, &UserId::new("alice"), repo, None, None)
        .await
        .unwrap();
    open(w, repo).await
}

async fn blob(w: &World, text: &str) -> ContentHash {
    w.objects.put(text.as_bytes().to_vec()).await.unwrap()
}

async fn put_policy(
    w: &World,
    svc: &RepoService,
    who: &Principal,
    text: &str,
    base: Option<u64>,
) -> Result<pyn_core::Revision, PynError> {
    let c = blob(w, text).await;
    svc.checkin(who, &path(POLICY), c, base.map(RevisionId), "policy".into())
        .await
}

#[tokio::test]
async fn the_first_policy_file_is_accepted_without_a_lock_and_applies_at_once() {
    let w = world();
    let svc = make_repo(&w, "game").await;
    assert_eq!(svc.mode_for(&path("Source/a.cpp")), Mode::Exclusive);

    let rev = put_policy(&w, &svc, &admin("alice"), SHARED_POLICY, None)
        .await
        .unwrap();
    assert_eq!(rev.id, RevisionId(1));
    assert_eq!(
        rev.mode,
        Some(Mode::Shared),
        "the mode it declares for itself"
    );
    assert_eq!(svc.mode_for(&path("Source/a.cpp")), Mode::Shared);
    assert_eq!(svc.mode_for(&path("Content/a.umap")), Mode::Exclusive);
    assert_eq!(svc.mode_for(&path(POLICY)), Mode::Shared);
}

#[tokio::test]
async fn the_first_policy_file_that_leaves_itself_unlisted_is_recorded_exclusive() {
    let w = world();
    let svc = make_repo(&w, "game").await;
    let rev = put_policy(&w, &svc, &admin("alice"), EXCLUSIVE_POLICY, None)
        .await
        .unwrap();
    assert_eq!(rev.mode, Some(Mode::Exclusive));
    assert_eq!(svc.mode_for(&path(POLICY)), Mode::Exclusive);
}

#[tokio::test]
async fn changing_policy_needs_the_policy_permission() {
    let w = world();
    let svc = make_repo(&w, "game").await;
    let err = put_policy(&w, &svc, &writer("bob"), SHARED_POLICY, None)
        .await
        .unwrap_err();
    assert!(
        matches!(err, PynError::Forbidden(Permission::EditPolicy)),
        "{err}"
    );
    assert!(svc.head(&path(POLICY)).await.unwrap().is_none());

    put_policy(&w, &svc, &admin("alice"), EXCLUSIVE_POLICY, None)
        .await
        .unwrap();
    svc.checkout(&path(POLICY), &UserId::new("bob"), Some(RevisionId(1)))
        .await
        .unwrap();
    let err = put_policy(&w, &svc, &writer("bob"), SHARED_POLICY, Some(1))
        .await
        .unwrap_err();
    assert!(
        matches!(err, PynError::Forbidden(Permission::EditPolicy)),
        "{err}"
    );
    assert_eq!(svc.mode_for(&path("Source/a.cpp")), Mode::Shared);
    assert_eq!(svc.mode_for(&path("Content/a.umap")), Mode::Exclusive);
}

#[tokio::test]
async fn a_policy_that_does_not_parse_is_rejected_and_leaves_nothing_behind() {
    let w = world();
    let svc = make_repo(&w, "game").await;
    for bad in [
        "[exlusive]\npaths = []\n",
        "[exclusive]\npaths = [\"a\"]\n[shared]\npaths = [\"a\"]\n",
        "not = [valid",
    ] {
        let err = put_policy(&w, &svc, &admin("alice"), bad, None)
            .await
            .unwrap_err();
        assert!(matches!(err, PynError::InvalidRules(_)), "{bad}: {err}");
    }
    let c = w.objects.put(vec![0xff, 0xfe]).await.unwrap();
    let err = svc
        .checkin(&admin("alice"), &path(POLICY), c, None, "bytes".into())
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::InvalidRules(_)), "{err}");
    assert!(svc.head(&path(POLICY)).await.unwrap().is_none());
    assert_eq!(svc.mode_for(&path("Source/a.cpp")), Mode::Exclusive);

    put_policy(&w, &svc, &admin("alice"), EXCLUSIVE_POLICY, None)
        .await
        .unwrap();
    svc.checkout(&path(POLICY), &UserId::new("alice"), Some(RevisionId(1)))
        .await
        .unwrap();
    let err = put_policy(&w, &svc, &admin("alice"), "[meta]\ndefault = 3\n", Some(1))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::InvalidRules(_)), "{err}");
    assert_eq!(
        svc.head(&path(POLICY)).await.unwrap().unwrap().id,
        RevisionId(1)
    );
    assert_eq!(
        svc.locks().await.unwrap().len(),
        1,
        "the lock is kept for a retry"
    );
    assert_eq!(svc.mode_for(&path("Source/a.cpp")), Mode::Shared);
}

#[tokio::test]
async fn later_revisions_of_an_exclusive_policy_file_need_the_lock() {
    let w = world();
    let svc = make_repo(&w, "game").await;
    let alice = admin("alice");
    put_policy(&w, &svc, &alice, EXCLUSIVE_POLICY, None)
        .await
        .unwrap();

    let err = put_policy(&w, &svc, &alice, SHARED_POLICY, Some(1))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::LockRequired(_)), "{err}");
    assert_eq!(svc.mode_for(&path("Source/a.cpp")), Mode::Shared);

    svc.checkout(&path(POLICY), &alice.user, Some(RevisionId(1)))
        .await
        .unwrap();
    let rev = put_policy(
        &w,
        &svc,
        &alice,
        "[exclusive]\npaths = [\"Source/\"]\n[shared]\npaths = [\".pyn/pyn.toml\"]\n",
        Some(1),
    )
    .await
    .unwrap();
    assert_eq!(
        rev.mode,
        Some(Mode::Exclusive),
        "made under the mode in force"
    );
    assert_eq!(svc.mode_for(&path("Source/a.cpp")), Mode::Exclusive);
    assert_eq!(svc.mode_for(&path(POLICY)), Mode::Shared);
}

#[tokio::test]
async fn a_shared_policy_file_needs_no_lock_after_the_bootstrap() {
    let w = world();
    let svc = make_repo(&w, "game").await;
    let alice = admin("alice");
    put_policy(&w, &svc, &alice, SHARED_POLICY, None)
        .await
        .unwrap();
    let rev = put_policy(&w, &svc, &alice, &format!("{SHARED_POLICY}\n"), Some(1))
        .await
        .unwrap();
    assert_eq!((rev.id, rev.mode), (RevisionId(2), Some(Mode::Shared)));
}

#[tokio::test]
async fn the_bootstrap_only_applies_while_no_revision_exists() {
    let w = world();
    let svc = make_repo(&w, "game").await;
    put_policy(&w, &svc, &admin("alice"), EXCLUSIVE_POLICY, None)
        .await
        .unwrap();
    let err = put_policy(&w, &svc, &admin("alice"), SHARED_POLICY, None)
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::StaleBase { .. }), "{err}");
}

#[tokio::test]
async fn making_a_path_shared_releases_its_live_locks_and_audits_it() {
    let w = world();
    let svc = make_repo(&w, "game").await;
    let alice = admin("alice");
    put_policy(&w, &svc, &alice, EXCLUSIVE_POLICY, None)
        .await
        .unwrap();
    let (map, art) = (path("Content/a.umap"), path("Art/b.psd"));
    let bob = UserId::new("bob");
    svc.checkout(&map, &bob, None).await.unwrap();
    svc.checkout(&path(POLICY), &alice.user, Some(RevisionId(1)))
        .await
        .unwrap();
    let both = "[meta]\ndefault = \"shared\"\n[exclusive]\npaths = [\"Content/\", \"Art/\"]\n";
    put_policy(&w, &svc, &alice, both, Some(1)).await.unwrap();
    svc.checkout(&art, &bob, None).await.unwrap();
    assert_eq!(svc.locks().await.unwrap().len(), 2);

    svc.checkout(&path(POLICY), &alice.user, Some(RevisionId(2)))
        .await
        .unwrap();
    let only_art = "[meta]\ndefault = \"shared\"\n[exclusive]\npaths = [\"Art/\"]\n";
    put_policy(&w, &svc, &alice, only_art, Some(2))
        .await
        .unwrap();

    let locks = svc.locks().await.unwrap();
    assert_eq!(locks.len(), 1, "{locks:?}");
    assert_eq!(
        locks[0].path, art,
        "an unaffected exclusive path keeps its lock"
    );
    assert_eq!(svc.mode_for(&map), Mode::Shared);

    let log = w
        .audit
        .list(
            &pyn_core::AuditScope::Repo(svc.repo().clone()),
            &AuditQuery {
                limit: 100,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let released: Vec<_> = log
        .iter()
        .filter(|e| e.action == AuditAction::ForceUnlock)
        .collect();
    assert_eq!(released.len(), 1);
    assert_eq!(released[0].path.as_ref(), Some(&map));
    assert!(released[0].detail.contains("bob"), "{}", released[0].detail);
    let changes = log
        .iter()
        .filter(|e| e.action == AuditAction::PolicyChanged)
        .count();
    assert_eq!(changes, 3, "the bootstrap and each later revision");
}

#[tokio::test]
async fn every_revision_records_the_mode_it_was_made_under() {
    let w = world();
    let svc = make_repo(&w, "game").await;
    put_policy(&w, &svc, &admin("alice"), EXCLUSIVE_POLICY, None)
        .await
        .unwrap();
    let (src, map) = (path("Source/a.cpp"), path("Content/a.umap"));
    let alice = admin("alice");
    let c = blob(&w, "x").await;
    let shared = svc
        .checkin(&alice, &src, c.clone(), None, "s".into())
        .await
        .unwrap();
    svc.checkout(&map, &alice.user, None).await.unwrap();
    let exclusive = svc
        .checkin(&alice, &map, c, None, "e".into())
        .await
        .unwrap();
    assert_eq!(shared.mode, Some(Mode::Shared));
    assert_eq!(exclusive.mode, Some(Mode::Exclusive));
    assert_eq!(
        svc.head(&map).await.unwrap().unwrap().mode,
        Some(Mode::Exclusive)
    );
}

#[tokio::test]
async fn restoring_an_older_policy_applies_it() {
    let w = world();
    let svc = make_repo(&w, "game").await;
    let alice = admin("alice");
    put_policy(&w, &svc, &alice, EXCLUSIVE_POLICY, None)
        .await
        .unwrap();
    svc.checkout(&path(POLICY), &alice.user, Some(RevisionId(1)))
        .await
        .unwrap();
    put_policy(
        &w,
        &svc,
        &alice,
        "[meta]\ndefault = \"exclusive\"\n",
        Some(1),
    )
    .await
    .unwrap();
    assert_eq!(svc.mode_for(&path("Source/a.cpp")), Mode::Exclusive);

    svc.checkout(&path(POLICY), &alice.user, Some(RevisionId(2)))
        .await
        .unwrap();
    let writer = writer("bob");
    let err = svc
        .restore(
            &writer,
            &path(POLICY),
            RevisionId(1),
            RevisionId(2),
            ".pyn/pyn.toml@r2",
            None,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, PynError::Forbidden(Permission::EditPolicy)),
        "{err}"
    );
    svc.restore(
        &alice,
        &path(POLICY),
        RevisionId(1),
        RevisionId(2),
        ".pyn/pyn.toml@r2",
        None,
    )
    .await
    .unwrap();
    assert_eq!(svc.mode_for(&path("Source/a.cpp")), Mode::Shared);
}

#[tokio::test]
async fn a_restarted_server_reads_the_policy_from_each_repositorys_head() {
    let w = world();
    let game = make_repo(&w, "game").await;
    let site = make_repo(&w, "site").await;
    put_policy(&w, &game, &admin("alice"), EXCLUSIVE_POLICY, None)
        .await
        .unwrap();

    assert_eq!(game.mode_for(&path("Source/a.cpp")), Mode::Shared);
    assert_eq!(site.mode_for(&path("Source/a.cpp")), Mode::Exclusive);

    let restarted = build(&w, Rules::empty());
    let game = restarted.open("alice", "game").await.unwrap().service;
    let site = restarted.open("alice", "site").await.unwrap().service;
    assert_eq!(game.mode_for(&path("Source/a.cpp")), Mode::Shared);
    assert_eq!(game.mode_for(&path("Content/a.umap")), Mode::Exclusive);
    assert_eq!(site.mode_for(&path("Source/a.cpp")), Mode::Exclusive);
}

#[tokio::test]
async fn the_fallback_applies_only_until_a_policy_file_exists() {
    let w = world();
    make_repo(&w, "game").await;
    let fallback = Rules::from_toml("[meta]\ndefault = \"shared\"\n").unwrap();
    let configured = build(&w, fallback.clone());
    let svc = configured.open("alice", "game").await.unwrap().service;
    assert_eq!(svc.mode_for(&path("Source/a.cpp")), Mode::Shared);
    assert_eq!(svc.mode_for(&path(POLICY)), Mode::Exclusive);

    put_policy(
        &w,
        &svc,
        &admin("alice"),
        "[meta]\ndefault = \"exclusive\"\n",
        None,
    )
    .await
    .unwrap();
    assert_eq!(svc.mode_for(&path("Source/a.cpp")), Mode::Exclusive);
    let again = build(&w, fallback);
    let svc = again.open("alice", "game").await.unwrap().service;
    assert_eq!(svc.mode_for(&path("Source/a.cpp")), Mode::Exclusive);
}

#[tokio::test]
async fn a_stored_policy_that_no_longer_parses_fails_the_repository_closed() {
    let w = world();
    make_repo(&w, "game").await;
    let rec = w
        .meta
        .find_repo(&UserId::new("alice"), "game")
        .await
        .unwrap()
        .unwrap();
    let bad = blob(&w, "[exlusive]\n").await;
    w.meta
        .commit_revision(
            &rec.id,
            NewRevision {
                path: path(POLICY),
                content: bad,
                author: UserId::new("alice"),
                message: "old".into(),
                created_at: Utc::now(),
                restored_from: None,
                mode: Mode::Exclusive,
                size: 0,
            },
            None,
            None,
            Utc::now(),
        )
        .await
        .unwrap();
    let restarted = build(&w, Rules::empty());
    let err = restarted.open("alice", "game").await.err().unwrap();
    assert!(matches!(err, PynError::InvalidRules(_)), "{err}");
}

const LIMIT_POLICY: &str =
    "[meta]\ndefault = \"shared\"\nmax_locks_per_user = 2\n[exclusive]\npaths = [\"Content/\"]\n";

fn alice_session() -> Identity {
    Identity {
        user: UserId::new("alice"),
        credential: Credential::Session,
    }
}

async fn take(svc: &RepoService, name: &str) -> Result<pyn_core::Lock, PynError> {
    svc.checkout(&path(&format!("Content/{name}")), &UserId::new("bob"), None)
        .await
}

#[tokio::test]
async fn the_server_default_limit_applies_until_the_repository_or_policy_sets_one() {
    let w = world();
    let repos = build(&w, Rules::empty()).with_default_max_locks(1).unwrap();
    repos
        .create(&alice_session(), &UserId::new("alice"), "game", None, None)
        .await
        .unwrap();
    let open = repos.open("alice", "game").await.unwrap();
    let limit = open.service.lock_limit();
    assert_eq!((limit.max, limit.from_policy), (1, false));

    take(&open.service, "a").await.unwrap();
    let err = take(&open.service, "b").await.unwrap_err();
    assert!(
        matches!(err, PynError::LockLimitReached { limit: 1 }),
        "{err}"
    );

    let settings = pyn_core::RepoSettings {
        max_locks: Some(3),
        ..Default::default()
    };
    let update = pyn_core::RepoUpdate {
        settings: Some(settings),
        ..Default::default()
    };
    repos
        .update(&alice_session(), &open.record, update)
        .await
        .unwrap();
    let open = repos.open("alice", "game").await.unwrap();
    assert_eq!(
        open.service.lock_limit().max,
        3,
        "the setting beats the server default"
    );
    take(&open.service, "b").await.unwrap();
}

#[tokio::test]
async fn the_policy_limit_wins_over_the_setting_and_cannot_be_changed_as_a_setting() {
    let w = world();
    let svc = make_repo(&w, "game").await;
    let record = w.repos.open("alice", "game").await.unwrap().record;
    let set = |max_locks| pyn_core::RepoUpdate {
        settings: Some(pyn_core::RepoSettings {
            max_locks,
            ..Default::default()
        }),
        ..Default::default()
    };
    let record = w
        .repos
        .update(&alice_session(), &record, set(Some(4)))
        .await
        .unwrap();
    put_policy(&w, &svc, &admin("alice"), LIMIT_POLICY, None)
        .await
        .unwrap();

    let open = w.repos.open("alice", "game").await.unwrap();
    let limit = w.repos.lock_limit(&open.record).await.unwrap();
    assert_eq!((limit.max, limit.from_policy), (2, true));
    assert_eq!(
        open.record.settings.max_locks,
        Some(4),
        "the setting is kept underneath"
    );

    for attempt in [Some(5), None] {
        let err = w
            .repos
            .update(&alice_session(), &record, set(attempt))
            .await
            .unwrap_err();
        assert!(matches!(err, PynError::InvalidRequest(_)), "{err}");
    }
    w.repos
        .update(&alice_session(), &record, set(Some(4)))
        .await
        .expect("restating the stored value is not a change");

    take(&open.service, "a").await.unwrap();
    take(&open.service, "b").await.unwrap();
    let err = take(&open.service, "c").await.unwrap_err();
    assert!(
        matches!(err, PynError::LockLimitReached { limit: 2 }),
        "{err}"
    );
}

#[tokio::test]
async fn a_policy_change_moves_the_limit_at_once_and_keeps_existing_locks() {
    let w = world();
    let svc = make_repo(&w, "game").await;
    put_policy(&w, &svc, &admin("alice"), LIMIT_POLICY, None)
        .await
        .unwrap();
    take(&svc, "a").await.unwrap();
    take(&svc, "b").await.unwrap();

    svc.checkout(&path(POLICY), &UserId::new("alice"), Some(RevisionId(1)))
        .await
        .unwrap();
    let lower = LIMIT_POLICY.replace("= 2", "= 1");
    put_policy(&w, &svc, &admin("alice"), &lower, Some(1))
        .await
        .unwrap();
    assert_eq!(svc.lock_limit().max, 1);
    assert_eq!(svc.locks().await.unwrap().len(), 2, "no lock is released");
    let err = take(&svc, "c").await.unwrap_err();
    assert!(
        matches!(err, PynError::LockLimitReached { limit: 1 }),
        "{err}"
    );
    take(&svc, "a").await.expect("renewing is always allowed");

    svc.checkout(&path(POLICY), &UserId::new("alice"), Some(RevisionId(2)))
        .await
        .unwrap();
    let dropped = "[meta]\ndefault = \"shared\"\n[exclusive]\npaths = [\"Content/\"]\n";
    put_policy(&w, &svc, &admin("alice"), dropped, Some(2))
        .await
        .unwrap();
    let limit = svc.lock_limit();
    assert_eq!(
        (limit.max, limit.from_policy),
        (5, false),
        "back to the server default"
    );

    let restarted = build(&w, Rules::empty());
    let svc = restarted.open("alice", "game").await.unwrap().service;
    assert!(!svc.lock_limit().from_policy);
}

#[tokio::test]
async fn an_invalid_policy_limit_is_rejected() {
    let w = world();
    let svc = make_repo(&w, "game").await;
    for bad in [
        "[meta]\nmax_locks_per_user = 0\n",
        "[meta]\nmax_locks_per_user = -1\n",
        "[meta]\nmax_locks_per_user = \"five\"\n",
        "max_locks_per_user = 3\n",
    ] {
        let err = put_policy(&w, &svc, &admin("alice"), bad, None)
            .await
            .unwrap_err();
        assert!(matches!(err, PynError::InvalidRules(_)), "{bad}: {err}");
    }
    assert!(svc.head(&path(POLICY)).await.unwrap().is_none());
}

#[tokio::test]
async fn repository_creation_takes_the_limit_and_validates_it() {
    let w = world();
    let made = w
        .repos
        .create(
            &alice_session(),
            &UserId::new("alice"),
            "game",
            None,
            Some(pyn_core::RepoSettings {
                max_locks: Some(7),
                ..Default::default()
            }),
        )
        .await
        .unwrap();
    assert_eq!(made.settings.max_locks, Some(7));
    assert_eq!(w.repos.lock_limit(&made).await.unwrap().max, 7);

    for bad in [0, 10_001] {
        let err = w
            .repos
            .create(
                &alice_session(),
                &UserId::new("alice"),
                "other",
                None,
                Some(pyn_core::RepoSettings {
                    max_locks: Some(bad),
                    ..Default::default()
                }),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, PynError::InvalidRequest(_)), "{bad}: {err}");
    }
    assert!(build(&w, Rules::empty()).with_default_max_locks(0).is_err());
}
