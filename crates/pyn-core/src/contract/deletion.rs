//! Deleting paths in a changeset (issue #284), through `RepoService`: a deletion is a revision, and every store must
//! keep it out of the branch's files and trees while history keeps it.

use super::{Harness, Store, blob, entry, exclusive_history, harness, path, put_on, user, who};
use crate::audit::{AuditAction, AuditQuery};
use crate::{Deletion, PynError, RepoPath, Revision, RevisionId};

fn gone(p: &RepoPath, base: u64) -> Deletion {
    Deletion {
        path: p.clone(),
        base: Some(RevisionId(base)),
    }
}

async fn delete_on(
    h: &Harness,
    branch: &str,
    user_: &str,
    p: &RepoPath,
    base: u64,
) -> Result<Revision, PynError> {
    let mut revs = h
        .svc
        .checkin_changes_on(
            branch,
            &who(user_),
            Vec::new(),
            vec![gone(p, base)],
            "remove".into(),
        )
        .await?;
    Ok(revs.remove(0))
}

async fn live_paths(h: &Harness, branch: &str) -> Vec<String> {
    h.svc
        .snapshot_on(branch)
        .await
        .unwrap()
        .entries
        .into_iter()
        .map(|e| e.path.to_string())
        .collect()
}

pub async fn a_deletion_is_a_revision_that_leaves_the_branch_without_the_path(store: Store) {
    let h = harness(store);
    let (a, b) = (path("Source/a.cpp"), path("Source/sub/only.cpp"));
    put_on(&h, "main", "alice", &a, "one", None).await;
    put_on(&h, "main", "alice", &b, "two", None).await;
    let before = h.svc.head_commit().await.unwrap();

    let rev = delete_on(&h, "main", "alice", &b, 1).await.unwrap();
    assert!(rev.deleted && rev.id == RevisionId(2) && rev.message == "remove");
    assert!(rev.changeset.is_some());
    rev.verify().unwrap();
    assert_eq!(rev.mode, Some(crate::Mode::Shared));

    assert_eq!(live_paths(&h, "main").await, ["Source/a.cpp"]);
    let files = h.svc.files(None, 10).await.unwrap();
    assert_eq!(files.len(), 1);
    assert!(h.svc.tree(Some(&path("Source/sub"))).await.is_err());
    assert!(matches!(
        h.svc.read(&b, None).await.unwrap_err(),
        PynError::RevisionNotFound { .. }
    ));
    assert!(matches!(
        h.svc.read(&b, Some(RevisionId(2))).await.unwrap_err(),
        PynError::RevisionNotFound { .. }
    ));
    assert_eq!(h.svc.read(&b, Some(RevisionId(1))).await.unwrap().1, b"two");

    let history = h.svc.history(&b).await.unwrap();
    assert_eq!(
        history.iter().map(|r| r.deleted).collect::<Vec<_>>(),
        [false, true]
    );
    assert_eq!(history[1].previous.as_ref(), Some(&history[0].hash));
    assert!(h.svc.head(&b).await.unwrap().unwrap().deleted);

    let commit = h.svc.head_commit().await.unwrap().unwrap();
    assert_ne!(Some(commit), before);
    let page = h.svc.commits(None, None).await.unwrap();
    assert_eq!(
        page.commits[0].parents,
        before.into_iter().collect::<Vec<_>>()
    );
    assert_eq!(page.commits[0].revisions.len(), 1);
    assert!(page.commits[0].revisions[0].deleted);
    let report = h.svc.verify(None, true).await.unwrap();
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    let summary = h.svc.summary(10).await.unwrap();
    assert_eq!(summary.files, 1);
    let log = h
        .svc
        .audit(&AuditQuery {
            limit: 1,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(log[0].action, AuditAction::Delete);
    assert_eq!(log[0].path.as_ref(), Some(&b));
    assert!(log[0].detail.contains("r2"), "{}", log[0].detail);
}

pub async fn deleting_an_exclusive_path_needs_the_callers_live_lock_and_consumes_it(store: Store) {
    let h = harness(store);
    let m = path("Content/m.umap");
    exclusive_history(&h, &m, &["one", "two"]).await;

    let err = delete_on(&h, "main", "alice", &m, 2).await.unwrap_err();
    assert!(matches!(err, PynError::LockRequired(_)), "{err}");
    h.svc
        .checkout(&m, &user("bob"), Some(RevisionId(2)))
        .await
        .unwrap();
    let err = delete_on(&h, "main", "alice", &m, 2).await.unwrap_err();
    assert!(
        matches!(err, PynError::LockHeld { ref owner, .. } if owner == &user("bob")),
        "{err}"
    );
    assert_eq!(h.svc.history(&m).await.unwrap().len(), 2, "nothing written");
    assert_eq!(live_paths(&h, "main").await, ["Content/m.umap"]);

    let rev = delete_on(&h, "main", "bob", &m, 2).await.unwrap();
    assert_eq!((rev.id, rev.deleted), (RevisionId(3), true));
    assert_eq!(rev.mode, Some(crate::Mode::Exclusive));
    assert!(
        h.svc.locks().await.unwrap().is_empty(),
        "the lock is consumed"
    );
    assert!(live_paths(&h, "main").await.is_empty());
}

pub async fn a_deleted_exclusive_path_comes_back_by_restoring_an_earlier_revision(store: Store) {
    let h = harness(store);
    let m = path("Content/m.umap");
    exclusive_history(&h, &m, &["one", "two"]).await;
    h.svc
        .checkout(&m, &user("alice"), Some(RevisionId(2)))
        .await
        .unwrap();
    delete_on(&h, "main", "alice", &m, 2).await.unwrap();

    h.svc.checkout(&m, &user("alice"), None).await.unwrap();
    let err = h
        .svc
        .restore(
            &who("alice"),
            &m,
            RevisionId(3),
            RevisionId(3),
            "Content/m.umap@r3",
            None,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::InvalidRequest(_)), "{err}");
    let restored = h
        .svc
        .restore(
            &who("alice"),
            &m,
            RevisionId(1),
            RevisionId(3),
            "Content/m.umap@r3",
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        (restored.id, restored.restored_from, restored.deleted),
        (RevisionId(4), Some(RevisionId(1)), false)
    );
    assert_eq!(h.svc.read(&m, None).await.unwrap().1, b"one");
    assert_eq!(live_paths(&h, "main").await, ["Content/m.umap"]);
    let report = h.svc.verify(None, true).await.unwrap();
    assert!(report.problems.is_empty(), "{:?}", report.problems);
}

pub async fn a_deleted_shared_path_can_be_added_again(store: Store) {
    let h = harness(store);
    let a = path("Source/a.cpp");
    put_on(&h, "main", "alice", &a, "one", None).await;
    delete_on(&h, "main", "alice", &a, 1).await.unwrap();
    let again = put_on(&h, "main", "bob", &a, "back", None).await;
    assert_eq!((again.id, again.deleted), (RevisionId(3), false));
    assert_eq!(h.svc.read(&a, None).await.unwrap().1, b"back");
    assert_eq!(live_paths(&h, "main").await, ["Source/a.cpp"]);
}

pub async fn a_path_that_is_not_there_cannot_be_deleted(store: Store) {
    let h = harness(store);
    let (a, b) = (path("Source/a.cpp"), path("Source/b.cpp"));
    put_on(&h, "main", "alice", &a, "one", None).await;
    let commits = h.svc.commits(None, None).await.unwrap().commits.len();

    let err = delete_on(&h, "main", "alice", &b, 1).await.unwrap_err();
    assert!(
        matches!(err, PynError::PathNotFound(ref p) if p == "Source/b.cpp"),
        "{err}"
    );
    delete_on(&h, "main", "alice", &a, 1).await.unwrap();
    let err = delete_on(&h, "main", "alice", &a, 2).await.unwrap_err();
    assert!(matches!(err, PynError::PathNotFound(_)), "{err}");
    assert_eq!(
        h.svc.commits(None, None).await.unwrap().commits.len(),
        commits + 1
    );
    assert!(h.svc.head(&b).await.unwrap().is_none());
}

pub async fn a_deletion_is_judged_against_the_branchs_revision(store: Store) {
    let h = harness(store);
    let a = path("Source/a.cpp");
    put_on(&h, "main", "alice", &a, "one", None).await;
    put_on(&h, "main", "alice", &a, "two", Some(1)).await;
    let err = delete_on(&h, "main", "alice", &a, 1).await.unwrap_err();
    assert!(
        matches!(err, PynError::StaleBase { ref path, .. } if path == &a),
        "{err}"
    );
    let err = h
        .svc
        .checkin_changes_on(
            "main",
            &who("alice"),
            Vec::new(),
            vec![Deletion {
                path: a.clone(),
                base: None,
            }],
            "x".into(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::StaleBase { .. }), "{err}");
    assert_eq!(h.svc.history(&a).await.unwrap().len(), 2);
}

pub async fn a_changeset_mixes_writes_and_deletions_atomically(store: Store) {
    let h = harness(store);
    let (a, b, c) = (
        path("Source/a.cpp"),
        path("Source/b.cpp"),
        path("Source/c.cpp"),
    );
    put_on(&h, "main", "alice", &a, "a", None).await;
    put_on(&h, "main", "alice", &b, "b", None).await;
    let new = blob(&h, "c").await;

    let err = h
        .svc
        .checkin_changes_on(
            "main",
            &who("alice"),
            vec![entry(&c, &new, None)],
            vec![gone(&a, 1), gone(&b, 5)],
            "mixed".into(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::StaleBase { .. }), "{err}");
    assert_eq!(
        live_paths(&h, "main").await,
        ["Source/a.cpp", "Source/b.cpp"]
    );
    assert!(h.svc.head(&c).await.unwrap().is_none());

    let revs = h
        .svc
        .checkin_changes_on(
            "main",
            &who("alice"),
            vec![entry(&c, &new, None)],
            vec![gone(&a, 1)],
            "mixed".into(),
        )
        .await
        .unwrap();
    assert_eq!(
        revs.iter().map(|r| r.deleted).collect::<Vec<_>>(),
        [false, true]
    );
    assert_eq!(revs[0].changeset, revs[1].changeset);
    assert_eq!(
        live_paths(&h, "main").await,
        ["Source/b.cpp", "Source/c.cpp"]
    );
    let commit = h.svc.commits(None, None).await.unwrap().commits.remove(0);
    assert_eq!(commit.revisions.len(), 2);
    let report = h.svc.verify(None, false).await.unwrap();
    assert!(report.problems.is_empty(), "{:?}", report.problems);
}

pub async fn a_deletion_on_a_branch_leaves_the_other_branches_alone(store: Store) {
    let h = harness(store);
    let a = path("Source/a.cpp");
    put_on(&h, "main", "alice", &a, "one", None).await;
    h.svc
        .create_branch(&who("alice"), "feature", None)
        .await
        .unwrap();
    delete_on(&h, "feature", "alice", &a, 1).await.unwrap();
    assert!(live_paths(&h, "feature").await.is_empty());
    assert_eq!(live_paths(&h, "main").await, ["Source/a.cpp"]);
    put_on(&h, "main", "alice", &a, "two", Some(1)).await;
    let again = put_on(&h, "feature", "alice", &a, "new", None).await;
    assert!(!again.deleted);
    let report = h.svc.verify(None, false).await.unwrap();
    assert!(report.problems.is_empty(), "{:?}", report.problems);
}

pub async fn a_changeset_refuses_a_path_written_and_deleted_or_a_policy_deletion(store: Store) {
    let h = harness(store);
    let a = path("Source/a.cpp");
    put_on(&h, "main", "alice", &a, "one", None).await;
    let c = blob(&h, "x").await;
    let err = h
        .svc
        .checkin_changes_on(
            "main",
            &who("alice"),
            vec![entry(&a, &c, Some(1))],
            vec![gone(&a, 1)],
            "twice".into(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::InvalidRequest(_)), "{err}");
    let err = h
        .svc
        .checkin_changes_on(
            "main",
            &who("alice"),
            Vec::new(),
            vec![gone(&path(".pyn/pyn.toml"), 1)],
            "policy".into(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::InvalidRequest(_)), "{err}");
    let err = h
        .svc
        .checkin_changes_on("main", &who("alice"), Vec::new(), Vec::new(), "none".into())
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::InvalidRequest(_)), "{err}");
}
