//! Forks at the store: what is copied, what is not, and the limits checked in the same step.

use super::*;
use std::collections::HashSet;

use sha2::{Digest, Sha256};

use crate::{ForkCopy, ForkLimits, PynError};

fn fork_record(id: &str, owner: &str, name: &str) -> RepoRecord {
    let mut r = repo_record(id, owner, name);
    r.fork_of = Some(RepoId::new("game"));
    r
}

pub async fn a_fork_copies_history_but_not_locks_and_checks_its_limits(store: Store) {
    store
        .create_repo(repo_record("game", "alice", "game"))
        .await
        .unwrap();
    let h = harness(store.clone());
    let (a, m) = (path("Source/a.cpp"), path("Content/m.umap"));
    let c = blob(&h, "int main() {}").await;
    h.svc
        .checkin(&who("alice"), &a, c, None, "a".into())
        .await
        .unwrap();
    h.svc.checkout(&m, &user("bob"), None).await.unwrap();

    let refused = |limits: ForkLimits, owner: &'static str| {
        let store = store.clone();
        async move {
            let err = store
                .fork_repo_with_events(
                    fork_record("refused", owner, "game"),
                    &ForkCopy::All,
                    limits,
                    &[],
                )
                .await
                .unwrap_err();
            assert!(
                store
                    .get_repo(&RepoId::new("refused"))
                    .await
                    .unwrap()
                    .is_none()
            );
            err
        }
    };
    let err = refused(
        ForkLimits {
            repositories: Some(0),
            ..ForkLimits::default()
        },
        "bob",
    )
    .await;
    assert!(
        matches!(err, PynError::RepoLimitReached { limit: 0, .. }),
        "{err}"
    );
    let err = refused(
        ForkLimits {
            per_owner: Some(0),
            ..ForkLimits::default()
        },
        "bob",
    )
    .await;
    assert!(
        matches!(err, PynError::ForkLimitReached { limit: 0, .. }),
        "{err}"
    );
    let err = refused(
        ForkLimits {
            storage_bytes: Some(12),
            ..ForkLimits::default()
        },
        "bob",
    )
    .await;
    match err {
        PynError::ForkStorageLimitReached {
            used,
            needed,
            limit,
            ..
        } => {
            assert_eq!((used, needed, limit), (0, 13, 12));
        }
        other => panic!("{other}"),
    }
    let err = refused(ForkLimits::default(), "alice").await;
    assert!(
        matches!(err, PynError::RepoExists(_)),
        "the name is taken: {err}"
    );

    let fits = ForkLimits {
        repositories: Some(1),
        storage_bytes: Some(13),
        per_repository: Some(1),
        per_owner: Some(1),
    };
    let fork = store
        .fork_repo_with_events(
            fork_record("fork1", "bob", "game"),
            &ForkCopy::All,
            fits,
            &[],
        )
        .await
        .unwrap();
    assert_eq!(fork.fork_of, Some(RepoId::new("game")));
    assert_eq!(store.get_repo(&fork.id).await.unwrap(), Some(fork.clone()));
    let err = refused(fits, "carol").await;
    assert!(
        matches!(err, PynError::ForkLimitReached { limit: 1, ref what } if what == "alice/game"),
        "{err}"
    );

    let f = harness_on(store.clone(), h.objects.clone(), fork.id.clone(), RULES);
    let files: Vec<_> = f.svc.files(None, 100).await.unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, a);
    let (_, bytes) = f.svc.read(&a, None).await.unwrap();
    assert_eq!(
        bytes, b"int main() {}",
        "the content is reachable and verifies"
    );
    assert!(
        store
            .list_locks(&fork.id, h.clock.now())
            .await
            .unwrap()
            .is_empty(),
        "no locks"
    );
    assert_eq!(
        store
            .list_locks(&RepoId::new("game"), h.clock.now())
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(store.count_forks(&RepoId::new("game")).await.unwrap(), 1);
    assert_eq!(
        store.list_forks(&RepoId::new("game")).await.unwrap(),
        std::slice::from_ref(&fork)
    );
    let usage = store.owner_usage(&user("bob")).await.unwrap();
    assert_eq!(usage.stored_bytes, 13, "counted in full for the forker");

    let c = blob(&h, "int main() { return 1; }").await;
    h.svc
        .checkin(&who("alice"), &a, c, Some(RevisionId(1)), "later".into())
        .await
        .unwrap();
    assert_eq!(f.svc.files(None, 100).await.unwrap().len(), 1);
    assert_eq!(
        store.history(&fork.id, &a, None).await.unwrap().len(),
        1,
        "later source commits stay in the source"
    );

    assert!(store.delete_repo(&RepoId::new("game")).await.unwrap());
    let orphan = store.get_repo(&fork.id).await.unwrap().unwrap();
    assert_eq!(orphan.fork_of, None, "the link goes with the source");
    assert_eq!(
        f.svc.files(None, 100).await.unwrap().len(),
        1,
        "the copy stays"
    );
}

async fn lfs_put(h: &Harness, bytes: &[u8]) -> String {
    let oid = hex::encode(Sha256::digest(bytes));
    let body: crate::object::ObjectReader = Box::pin(std::io::Cursor::new(bytes.to_vec()));
    h.svc
        .lfs_upload(&user("alice"), &oid, body, bytes.len() as u64, None)
        .await
        .unwrap();
    oid
}

async fn names<T>(items: Vec<T>, name: impl Fn(&T) -> &str) -> Vec<String> {
    let mut out: Vec<String> = items.iter().map(|i| name(i).to_string()).collect();
    out.sort();
    out
}

pub async fn a_default_branch_fork_copies_only_what_the_default_branch_reaches(store: Store) {
    store
        .create_repo(repo_record("game", "alice", "game"))
        .await
        .unwrap();
    let h = harness(store.clone());
    let (alice, source) = (who("alice"), RepoId::new("game"));
    let (a, b) = (path("Source/a.txt"), path("Source/b.txt"));
    let main_one = blob(&h, "main one").await;
    h.svc
        .checkin(&alice, &a, main_one, None, "a1".into())
        .await
        .unwrap();
    h.svc.create_branch(&alice, "feature", None).await.unwrap();
    let side = blob(&h, "feature edit").await;
    let regular = crate::FileMode::Regular;
    h.svc
        .checkin_on(
            "feature",
            &alice,
            &a,
            side,
            regular,
            Some(RevisionId(1)),
            "a2".into(),
        )
        .await
        .unwrap();
    let only = blob(&h, "feature only file").await;
    h.svc
        .checkin_on("feature", &alice, &b, only, regular, None, "b1".into())
        .await
        .unwrap();
    let main_two = blob(&h, "main two").await;
    h.svc
        .checkin(&alice, &a, main_two, Some(RevisionId(1)), "a3".into())
        .await
        .unwrap();
    h.svc.create_tag(&alice, "v1", None, None).await.unwrap();
    h.svc
        .create_tag(&alice, "f1", Some("feature"), None)
        .await
        .unwrap();
    let (main_asset, side_asset) = (
        lfs_put(&h, b"main asset bytes").await,
        lfs_put(&h, b"side asset").await,
    );
    let head = store
        .get_branch(&source, "main")
        .await
        .unwrap()
        .unwrap()
        .head;
    let later = blob(&h, "main three").await;
    h.svc
        .checkin(&alice, &a, later, Some(RevisionId(3)), "a4".into())
        .await
        .unwrap();

    let contents: HashSet<_> = store
        .fork_contents(&source, head.as_ref().unwrap())
        .await
        .unwrap()
        .into_iter()
        .collect();
    assert_eq!(
        contents,
        HashSet::from([(main_one, 8), (side, 12), (main_two, 8)]),
        "a's revisions 1 to 3, the side one included"
    );

    let copy = ForkCopy::DefaultBranch {
        head,
        lfs: vec![main_asset.clone()],
    };
    let limits = |bytes| ForkLimits {
        storage_bytes: Some(bytes),
        ..ForkLimits::default()
    };
    let err = store
        .fork_repo_with_events(
            fork_record("refused", "bob", "game"),
            &copy,
            limits(43),
            &[],
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, PynError::ForkStorageLimitReached { needed: 44, .. }),
        "{err}"
    );
    assert!(
        store
            .get_repo(&RepoId::new("refused"))
            .await
            .unwrap()
            .is_none()
    );
    let fork = store
        .fork_repo_with_events(fork_record("fork1", "bob", "game"), &copy, limits(44), &[])
        .await
        .unwrap();

    let branches = store.list_branches(&fork.id).await.unwrap();
    assert_eq!(names(branches.clone(), |b| &b.name).await, ["main"]);
    assert_eq!(branches[0].head, head, "the head read before the copy");
    let tags = store.list_tags(&fork.id).await.unwrap();
    assert_eq!(names(tags, |t| &t.name).await, ["v1"]);
    let f = harness_on(store.clone(), h.objects.clone(), fork.id.clone(), RULES);
    let files = f.svc.files(None, 100).await.unwrap();
    assert_eq!(files.iter().map(|r| &r.path).collect::<Vec<_>>(), [&a]);
    let (_, bytes) = f.svc.read(&a, None).await.unwrap();
    assert_eq!(bytes, b"main two");
    let history = f.svc.history(&a).await.unwrap();
    assert_eq!(history.len(), 3, "the revision chain stays whole");
    assert!(store.history(&fork.id, &b, None).await.unwrap().is_empty());
    let on_head = |repo: RepoId| {
        let store = store.clone();
        async move {
            store
                .commits(&repo, head.as_ref(), None, 100)
                .await
                .unwrap()
                .len()
        }
    };
    assert_eq!(
        store
            .commits(&fork.id, None, None, 100)
            .await
            .unwrap()
            .len(),
        on_head(source.clone()).await
    );
    assert!(
        store
            .lfs_object(&fork.id, &main_asset)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        store
            .lfs_object(&fork.id, &side_asset)
            .await
            .unwrap()
            .is_none()
    );
    let usage = store.owner_usage(&user("bob")).await.unwrap();
    assert_eq!((usage.stored_bytes, usage.lfs_bytes), (28, 16));
    assert_eq!(store.unreferenced_bytes(&fork.id).await.unwrap(), 0);

    let all = store
        .fork_repo_with_events(
            fork_record("fork2", "carol", "game"),
            &ForkCopy::All,
            ForkLimits::default(),
            &[],
        )
        .await
        .unwrap();
    let branches = store.list_branches(&all.id).await.unwrap();
    assert_eq!(names(branches, |b| &b.name).await, ["feature", "main"]);
    let tags = store.list_tags(&all.id).await.unwrap();
    assert_eq!(names(tags, |t| &t.name).await, ["f1", "v1"]);
    let usage = store.owner_usage(&user("carol")).await.unwrap();
    assert_eq!((usage.stored_bytes, usage.lfs_bytes), (55, 26));
}
