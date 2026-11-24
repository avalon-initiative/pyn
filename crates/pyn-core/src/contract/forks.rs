//! Forks at the store: what is copied, what is not, and the limits checked in the same step.

use super::*;
use crate::{ForkLimits, PynError};

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
                .fork_repo_with_events(fork_record("refused", owner, "game"), limits, &[])
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
        .fork_repo_with_events(fork_record("fork1", "bob", "game"), fits, &[])
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
