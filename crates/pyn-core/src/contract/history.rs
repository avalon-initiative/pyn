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
