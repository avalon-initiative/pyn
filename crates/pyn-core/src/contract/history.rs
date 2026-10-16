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
    let nothing = PathFilter::new("Missing/").unwrap();

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
