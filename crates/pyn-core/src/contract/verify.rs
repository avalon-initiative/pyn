//! Verification in bounded steps against the store's own paging.

use super::{Store, harness, path, put_on, user, who};
use crate::{PynError, RevisionId, VerifyReport};

async fn stepped(h: &super::Harness, audit: bool, budget: usize) -> (VerifyReport, usize) {
    let mut total = VerifyReport::default();
    let (mut cursor, mut steps) = (None, 0);
    loop {
        let step = h
            .svc
            .verify_step(None, audit, cursor.as_deref(), budget)
            .await
            .unwrap();
        steps += 1;
        assert!(steps < 1000, "no progress at budget {budget}");
        total.paths += step.paths;
        total.revisions += step.revisions;
        total.commits += step.commits;
        total.audit_events = match (total.audit_events, step.audit_events) {
            (None, None) => None,
            (a, b) => Some(a.unwrap_or(0) + b.unwrap_or(0)),
        };
        total.problems.extend(step.problems);
        match step.next {
            Some(next) => cursor = Some(next),
            None => return (total, steps),
        }
    }
}

pub async fn verification_in_steps_covers_the_same_ground_at_every_budget(store: Store) {
    let h = harness(store);
    let (a, b, map) = (
        path("Source/a.cpp"),
        path("Source/b.cpp"),
        path("Content/a.umap"),
    );
    for i in 1..=4u64 {
        put_on(
            &h,
            "main",
            "alice",
            &a,
            &format!("a{i}"),
            (i > 1).then_some(i - 1),
        )
        .await;
    }
    put_on(&h, "main", "alice", &b, "b1", None).await;
    h.svc.checkout(&map, &user("alice"), None).await.unwrap();
    put_on(&h, "main", "alice", &map, "m1", None).await;
    h.svc
        .create_branch(&who("alice"), "feature", None)
        .await
        .unwrap();
    h.svc
        .checkout_on("feature", &map, &user("bob"), Some(RevisionId(1)))
        .await
        .unwrap();
    put_on(&h, "feature", "bob", &map, "m2", Some(1)).await;
    put_on(&h, "feature", "bob", &b, "b2", Some(1)).await;

    let whole = h.svc.verify(None, true).await.unwrap();
    assert!(whole.problems.is_empty(), "{whole:?}");
    assert_eq!((whole.paths, whole.revisions), (3, 8));
    assert_eq!(whole.commits, 8);
    assert!(whole.audit_events.is_some_and(|n| n >= 8), "{whole:?}");

    let key = |r: &VerifyReport| (r.paths, r.revisions, r.commits, r.audit_events);
    let mut most = 0;
    for budget in 1..=30 {
        let (got, steps) = stepped(&h, true, budget).await;
        assert!(got.problems.is_empty(), "{got:?}");
        assert_eq!(key(&got), key(&whole), "budget {budget}");
        most = most.max(steps);
    }
    assert!(most > 5, "a tiny budget takes many steps");
    let (_, one) = stepped(&h, true, 100_000).await;
    assert_eq!(one, 1, "a large budget is one step");
    let (without, _) = stepped(&h, false, 2).await;
    assert_eq!(without.audit_events, None);

    for bad in ["", "x", "r:0:Source/a.cpp", "c:zz", "t:x", "a:-1"] {
        let err = h
            .svc
            .verify_step(None, true, Some(bad), 5)
            .await
            .unwrap_err();
        assert!(matches!(err, PynError::InvalidRequest(_)), "{bad:?}: {err}");
    }
}

pub async fn head_revisions_page_by_path(store: Store) {
    use crate::RepoId;

    let h = harness(store.clone());
    let names = ["Source/a.cpp", "Source/b.cpp", "Source/c.cpp", "Docs/d.md"];
    for name in names {
        put_on(&h, "main", "alice", &path(name), name, None).await;
    }
    put_on(&h, "main", "alice", &path("Source/a.cpp"), "again", Some(1)).await;
    let repo: &RepoId = h.svc.repo();
    let all = store.list_head_revisions(repo, None, 100).await.unwrap();
    let got: Vec<_> = all.iter().map(|r| r.path.to_string()).collect();
    assert_eq!(
        got,
        ["Docs/d.md", "Source/a.cpp", "Source/b.cpp", "Source/c.cpp"]
    );
    assert_eq!(all[1].id, RevisionId(2), "the newest revision of the path");
    let exact = store.list_head_revisions(repo, None, 4).await.unwrap();
    assert_eq!(exact.len(), 4);
    let two = store.list_head_revisions(repo, None, 2).await.unwrap();
    assert_eq!(two.len(), 2);
    let rest = store
        .list_head_revisions(repo, Some(&two[1].path), 100)
        .await
        .unwrap();
    assert_eq!(rest.len(), 2);
    let none = store
        .list_head_revisions(repo, Some(&all[3].path), 100)
        .await
        .unwrap();
    assert!(none.is_empty());
}
