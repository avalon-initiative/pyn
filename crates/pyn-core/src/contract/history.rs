//! The bounded scan behind repository history.

use super::push::{Built, Files, build, objects, set};
use super::{CountingStore, RULES, Store, blob, harness_on};
use crate::memory::MemoryObjectStore;
use crate::{PathFilter, RepoId};
use std::sync::Arc;

pub async fn a_scan_that_spends_its_budget_resumes_from_a_cursor(store: Store) {
    let counting = CountingStore::new(store.clone());
    let h = harness_on(
        counting.clone(),
        Arc::new(MemoryObjectStore::new()),
        RepoId::new("scan"),
        RULES,
    );
    let mut files = Files::new();
    let mut parent = None;
    let mut built: Vec<Built> = Vec::new();
    for i in 1..=6 {
        let b = blob(&h, &format!("v{i}")).await;
        set(&mut files, "Source/a.cpp", b);
        let c = build(&files, parent, &format!("c{i}"), i);
        parent = Some(c.id);
        built.push(c);
    }
    let parts: Vec<&Built> = built.iter().collect();
    h.svc
        .push_commits(&super::who("alice"), "main", objects(&parts))
        .await
        .unwrap();
    let tip = built.last().unwrap().id;
    let nothing = PathFilter::new("nothing.txt").unwrap();

    let scan = store
        .repo_history(h.svc.repo(), Some(&tip), Some(&nothing), None, 5, 4)
        .await
        .unwrap();
    assert!(scan.revisions.is_empty());
    let resume = scan.resume.expect("the budget ended the scan");

    let rest = store
        .repo_history(
            h.svc.repo(),
            Some(&tip),
            Some(&nothing),
            Some(&resume),
            5,
            4,
        )
        .await
        .unwrap();
    assert!(rest.revisions.is_empty());
    assert_eq!(
        rest.resume, None,
        "two rows remained, fewer than the budget"
    );

    let any = PathFilter::new("a.cpp").unwrap();
    let first = store
        .repo_history(h.svc.repo(), Some(&tip), Some(&any), None, 2, 4)
        .await
        .unwrap();
    assert_eq!(first.revisions.len(), 2);
    assert_eq!(first.resume, None);

    let folder = PathFilter::new("Source/").unwrap();
    let by_folder = store
        .repo_history(h.svc.repo(), Some(&tip), Some(&folder), None, 3, 4)
        .await
        .unwrap();
    assert_eq!(by_folder.revisions.len(), 3);

    counting.take();
    let page = h
        .svc
        .repo_history(Some(&nothing), None, None)
        .await
        .unwrap();
    assert!(page.revisions.is_empty() && page.next.is_none());
    assert!(counting.take() <= 3, "one scan, not one per batch");
}

pub async fn listings_page_in_the_store_and_agree_with_the_whole_listing(store: Store) {
    use super::{path, put_on, user};
    use crate::PynError;

    let h = super::harness(store);
    for name in [
        "Source/a.cpp",
        "Source/b.cpp",
        "Source/sub/c.cpp",
        "Docs/d.md",
    ] {
        put_on(&h, "main", "alice", &path(name), name, None).await;
    }
    h.svc
        .checkout(&path("Content/lock-only.umap"), &user("alice"), None)
        .await
        .unwrap();

    let whole = h.svc.snapshot(None).await.unwrap();
    let names: Vec<_> = whole.entries.iter().map(|e| e.path.to_string()).collect();
    assert_eq!(names.len(), 4);
    let head = whole.commit;
    let page = h
        .svc
        .snapshot_page(head, Some(&path(&names[0])), 2)
        .await
        .unwrap();
    let got: Vec<_> = page.entries.iter().map(|e| e.path.to_string()).collect();
    assert_eq!(got, names[1..3]);

    let files = h.svc.files(None, 100).await.unwrap();
    let all: Vec<_> = files.iter().map(|f| f.path.to_string()).collect();
    assert_eq!(all.len(), 5);
    assert!(all.windows(2).all(|w| w[0] < w[1]));
    assert!(files.iter().any(|f| f.lock.is_some()));
    for take in 1..=5 {
        let page = h.svc.files(None, take).await.unwrap();
        let got: Vec<_> = page.iter().map(|f| f.path.to_string()).collect();
        assert_eq!(got, all[..take], "limit {take}");
        let after = path(&all[take - 1]);
        let rest = h.svc.files(Some(&after), 100).await.unwrap();
        let got: Vec<_> = rest.iter().map(|f| f.path.to_string()).collect();
        assert_eq!(got, all[take..], "after {take}");
    }

    let source = h.svc.tree(Some(&path("Source"))).await.unwrap();
    let kids: Vec<_> = source.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(kids, ["sub", "a.cpp", "b.cpp"]);
    let err = h.svc.tree(Some(&path("Source/a.cpp"))).await.unwrap_err();
    assert!(matches!(err, PynError::InvalidRequest(_)), "{err}");
    let err = h.svc.tree(Some(&path("Nope"))).await.unwrap_err();
    assert!(matches!(err, PynError::PathNotFound(_)), "{err}");
}

pub async fn a_path_history_pages_by_revision_number_and_ends_exactly(store: Store) {
    use super::{harness, path, put_on, user};
    use crate::{PynError, RevisionId, decode_path_cursor, encode_path_cursor};

    let h = harness(store);
    let a = path("Source/a.cpp");
    let empty = h.svc.history_page_on("main", &a, None, None).await.unwrap();
    assert!(
        empty.revisions.is_empty() && empty.next.is_none(),
        "no revisions"
    );

    for i in 1..=5u64 {
        put_on(
            &h,
            "main",
            "alice",
            &a,
            &format!("v{i}"),
            i.checked_sub(1).filter(|b| *b > 0),
        )
        .await;
    }
    let ids = |p: &crate::PathHistoryPage| p.revisions.iter().map(|r| r.id.0).collect::<Vec<_>>();

    let exact = h
        .svc
        .history_page_on("main", &a, None, Some(5))
        .await
        .unwrap();
    assert_eq!(
        (ids(&exact), exact.next),
        (vec![1, 2, 3, 4, 5], None),
        "exactly at the limit"
    );
    let over = h
        .svc
        .history_page_on("main", &a, None, Some(6))
        .await
        .unwrap();
    assert_eq!((ids(&over), over.next), (vec![1, 2, 3, 4, 5], None));
    let one_short = h
        .svc
        .history_page_on("main", &a, None, Some(4))
        .await
        .unwrap();
    assert_eq!(
        (ids(&one_short), one_short.next),
        (vec![1, 2, 3, 4], Some(RevisionId(4)))
    );

    let mut seen = Vec::new();
    let mut after = None;
    let mut pages = 0;
    loop {
        let page = h
            .svc
            .history_page_on("main", &a, after, Some(2))
            .await
            .unwrap();
        seen.extend(ids(&page));
        pages += 1;
        after = page.next;
        if after.is_none() {
            break;
        }
    }
    assert_eq!((seen, pages), (vec![1, 2, 3, 4, 5], 3));

    let past = h
        .svc
        .history_page_on("main", &a, Some(RevisionId(5)), None)
        .await
        .unwrap();
    assert!(
        past.revisions.is_empty() && past.next.is_none(),
        "after the last revision"
    );
    let zero = h
        .svc
        .history_page_on("main", &a, None, Some(0))
        .await
        .unwrap();
    assert_eq!(ids(&zero), vec![1], "a zero limit is one");
    let huge = h
        .svc
        .history_page_on("main", &a, None, Some(usize::MAX))
        .await
        .unwrap();
    assert_eq!(
        ids(&huge).len(),
        5,
        "an oversized limit is capped, not an error"
    );
    let default = h.svc.history_page_on("main", &a, None, None).await.unwrap();
    assert_eq!(ids(&default).len(), 5);
    assert_eq!(h.svc.history_on("main", &a).await.unwrap().len(), 5);

    for bad in ["", "0", "-1", "x", "1.5"] {
        assert!(
            matches!(decode_path_cursor(bad), Err(PynError::InvalidRequest(_))),
            "{bad:?}"
        );
    }
    assert_eq!(
        decode_path_cursor(&encode_path_cursor(RevisionId(4))).unwrap(),
        RevisionId(4)
    );

    let map = path("Content/a.umap");
    h.svc.checkout(&map, &user("alice"), None).await.unwrap();
    put_on(&h, "main", "alice", &map, "v1", None).await;
    h.svc
        .create_branch(&crate::contract::who("alice"), "feature", None)
        .await
        .unwrap();
    h.svc
        .checkout_on("feature", &map, &user("bob"), Some(RevisionId(1)))
        .await
        .unwrap();
    put_on(&h, "feature", "bob", &map, "v2", Some(1)).await;
    h.svc
        .checkout(&map, &user("alice"), Some(RevisionId(1)))
        .await
        .unwrap();
    put_on(&h, "main", "alice", &map, "v3", Some(1)).await;
    h.svc
        .checkout_on("feature", &map, &user("bob"), Some(RevisionId(2)))
        .await
        .unwrap();
    put_on(&h, "feature", "bob", &map, "v4", Some(2)).await;

    let mut seen = Vec::new();
    let mut after = None;
    loop {
        let page = h
            .svc
            .history_page_on("feature", &map, after, Some(1))
            .await
            .unwrap();
        seen.extend(ids(&page));
        after = page.next;
        if after.is_none() {
            break;
        }
    }
    assert_eq!(
        seen,
        vec![1, 2, 4],
        "a branch's page skips what it does not contain and still checks the links"
    );
    let main = h
        .svc
        .history_page_on("main", &map, None, Some(1))
        .await
        .unwrap();
    assert_eq!((ids(&main), main.next), (vec![1], Some(RevisionId(1))));
    let err = h
        .svc
        .history_page_on("nope", &map, None, None)
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::BranchNotFound(_)), "{err}");
}
