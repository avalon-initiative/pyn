//! Commit listing filters and revision ranges every `MetadataStore` must satisfy, through `RepoService`.

use super::{Harness, Store, harness, path, put_on, who};
use crate::{CommitFilter, ObjectId, PynError};
use chrono::{DateTime, Duration, TimeZone, Utc};

fn t0() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 8, 9, 0, 0).unwrap()
}

async fn ids(h: &Harness, from: Option<&ObjectId>, f: &CommitFilter) -> Vec<ObjectId> {
    let page = h
        .svc
        .commits_filtered("main", from, f, None, None)
        .await
        .unwrap();
    page.commits.iter().map(|c| c.id).collect()
}

async fn by(h: &Harness, f: CommitFilter) -> Vec<ObjectId> {
    ids(h, None, &f).await
}

/// Four commits on main an hour apart (alice, bob, alice, bob) and a feature commit by carol on the second.
struct Log {
    main: [ObjectId; 4],
    feature: ObjectId,
}

async fn log(h: &Harness) -> Log {
    let (a, b, c, f) = (
        path("Source/a.cpp"),
        path("Source/sub/b.cpp"),
        path("Source/c.cpp"),
        path("Source/f.cpp"),
    );
    let c1 = put_on(h, "main", "alice", &a, "alpha", None).await.commit;
    h.clock.advance(Duration::hours(1));
    let c2 = put_on(h, "main", "bob", &b, "Beta", None).await.commit;
    h.svc
        .create_branch(&who("alice"), "feature", Some(&c2.to_hex()))
        .await
        .unwrap();
    let feature = put_on(h, "feature", "carol", &f, "feat", None).await.commit;
    h.clock.advance(Duration::hours(1));
    let c3 = put_on(h, "main", "alice", &a, "gamma", Some(1))
        .await
        .commit;
    h.clock.advance(Duration::hours(1));
    let c4 = put_on(h, "main", "bob", &c, "delta", None).await.commit;
    Log {
        main: [c1, c2, c3, c4],
        feature,
    }
}

pub async fn commit_filters_match_author_time_message_and_path(store: Store) {
    let h = harness(store);
    let Log {
        main: [c1, c2, c3, c4],
        ..
    } = log(&h).await;
    let none = CommitFilter::default;

    assert_eq!(by(&h, none()).await, vec![c4, c3, c2, c1]);
    let author = |a: &str| CommitFilter {
        author: Some(a.into()),
        ..none()
    };
    assert_eq!(by(&h, author("ALICE")).await, vec![c3, c1]);
    assert_eq!(by(&h, author("bob@PYN")).await, vec![c4, c2]);
    assert!(by(&h, author("nobody")).await.is_empty());

    let at = |h: i64| t0() + Duration::hours(h);
    let since = CommitFilter {
        since: Some(at(1)),
        ..none()
    };
    assert_eq!(by(&h, since).await, vec![c4, c3, c2]);
    let until = CommitFilter {
        until: Some(at(1)),
        ..none()
    };
    assert_eq!(by(&h, until).await, vec![c2, c1]);
    let window = CommitFilter {
        since: Some(at(1)),
        until: Some(at(1)),
        ..none()
    };
    assert_eq!(by(&h, window).await, vec![c2]);

    let grep = |g: &str| CommitFilter {
        grep: Some(g.into()),
        ..none()
    };
    assert_eq!(by(&h, grep("BETA")).await, vec![c2]);
    assert_eq!(by(&h, grep("on main")).await, vec![c4, c3, c2, c1]);
    assert!(by(&h, grep("feat")).await.is_empty());

    let on = |p: &str| CommitFilter {
        path: Some(path(p)),
        ..none()
    };
    assert_eq!(by(&h, on("Source/a.cpp")).await, vec![c3, c1]);
    assert_eq!(by(&h, on("Source/sub")).await, vec![c2]);
    assert_eq!(by(&h, on("Source")).await, vec![c4, c3, c2, c1]);
    assert!(by(&h, on("Source/su")).await.is_empty());
    assert!(by(&h, on("Source/a.cp")).await.is_empty());

    let combined = CommitFilter {
        author: Some("alice".into()),
        path: Some(path("Source/a.cpp")),
        since: Some(at(1)),
        ..none()
    };
    assert_eq!(by(&h, combined).await, vec![c3]);
}

pub async fn filtered_commit_pages_are_full_while_more_matches_exist(store: Store) {
    let h = harness(store);
    let Log {
        main: [c1, c2, c3, c4],
        ..
    } = log(&h).await;
    let f = CommitFilter {
        path: Some(path("Source/a.cpp")),
        ..CommitFilter::default()
    };
    let first = h
        .svc
        .commits_filtered("main", None, &f, None, Some(1))
        .await
        .unwrap();
    assert_eq!(
        first.commits.iter().map(|c| c.id).collect::<Vec<_>>(),
        vec![c3]
    );
    assert_eq!(first.next, Some(c3));
    let rest = h
        .svc
        .commits_filtered("main", None, &f, first.next.as_ref(), Some(1))
        .await
        .unwrap();
    assert_eq!(
        rest.commits.iter().map(|c| c.id).collect::<Vec<_>>(),
        vec![c1]
    );
    assert_eq!(rest.next, None);

    let bob = CommitFilter {
        author: Some("bob".into()),
        ..CommitFilter::default()
    };
    let page = h
        .svc
        .commits_filtered("main", None, &bob, None, Some(2))
        .await
        .unwrap();
    assert_eq!(
        page.commits.iter().map(|c| c.id).collect::<Vec<_>>(),
        vec![c4, c2]
    );
    assert_eq!(page.next, None);
}

pub async fn a_revision_range_lists_what_one_commit_has_and_the_other_lacks(store: Store) {
    let h = harness(store);
    let Log {
        main: [c1, c2, c3, c4],
        feature,
    } = log(&h).await;
    let to = |x: ObjectId| CommitFilter {
        exclude: Some(x),
        ..CommitFilter::default()
    };

    assert_eq!(ids(&h, Some(&c3), &to(c1)).await, vec![c3, c2]);
    assert_eq!(ids(&h, None, &to(c2)).await, vec![c4, c3]);
    assert_eq!(ids(&h, Some(&feature), &to(c4)).await, vec![feature]);
    assert_eq!(ids(&h, Some(&c4), &to(feature)).await, vec![c4, c3]);
    assert!(ids(&h, Some(&c2), &to(c4)).await.is_empty());

    let both = CommitFilter {
        author: Some("bob".into()),
        exclude: Some(c1),
        ..CommitFilter::default()
    };
    assert_eq!(ids(&h, None, &both).await, vec![c4, c2]);

    let unknown = ObjectId::from_hex(crate::REPO_ALGORITHM, &"0".repeat(64)).unwrap();
    for (from, f) in [
        (Some(&unknown), CommitFilter::default()),
        (None, to(unknown)),
    ] {
        let err = h
            .svc
            .commits_filtered("main", from, &f, None, None)
            .await
            .unwrap_err();
        assert!(matches!(err, PynError::InvalidRequest(_)), "{err}");
    }
}
