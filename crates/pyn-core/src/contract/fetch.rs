//! Enumerating the commits and trees a client lacks, through `RepoService::fetch`.

use std::collections::HashSet;
use std::sync::Arc;

use pyn_object::{Algorithm, parse_tree};

use super::push::{Built, Files, build, objects, set};
use super::{
    CountingStore, Harness, RULES, Store, blob, harness, harness_on, path, put_on, user, who,
};
use crate::memory::MemoryObjectStore;
use crate::merge::{FastForward, MergeRequest};
use crate::service::{FetchPage, FetchQuery};
use crate::{FileMode, Mode, ObjectId, PynError, RepoId, RevisionId};

async fn pushed(h: &Harness, parts: &[&Built]) {
    h.svc
        .push_commits(&who("alice"), "main", objects(parts))
        .await
        .unwrap();
}

async fn fetch(h: &Harness, query: FetchQuery) -> FetchPage {
    h.svc.fetch(&query).await.unwrap()
}

fn ids(page: &FetchPage) -> Vec<ObjectId> {
    page.commits.iter().map(|c| c.id).collect()
}

fn sent_trees(page: &FetchPage) -> HashSet<ObjectId> {
    page.commits
        .iter()
        .flat_map(|c| c.trees.iter().map(|(id, _)| *id))
        .collect()
}

async fn closure(h: &Harness, tree: ObjectId) -> HashSet<ObjectId> {
    let mut out = HashSet::new();
    let mut stack = vec![tree];
    while let Some(id) = stack.pop() {
        if !out.insert(id) {
            continue;
        }
        let body = h.store.get_tree(h.svc.repo(), &id).await.unwrap().unwrap();
        for e in parse_tree(Algorithm::Sha256, &body).unwrap() {
            if e.mode.is_tree() {
                stack.push(e.id);
            }
        }
    }
    out
}

/// Three commits over nested folders: the first adds `Source` and `Docs`, the second edits `Source` only, the third
/// adds `Docs/guide`.
async fn history(h: &Harness) -> ([Built; 3], Files) {
    let (a1, a2, r, g) = (
        blob(h, "a1").await,
        blob(h, "a2").await,
        blob(h, "readme").await,
        blob(h, "guide").await,
    );
    let mut files = Files::new();
    set(&mut files, "Source/a.cpp", a1);
    set(&mut files, "Docs/readme.md", r);
    let first = build(&files, None, "first", 1);
    set(&mut files, "Source/a.cpp", a2);
    let second = build(&files, Some(first.id), "second", 2);
    set(&mut files, "Docs/guide/intro.md", g);
    let third = build(&files, Some(second.id), "third", 3);
    pushed(h, &[&first, &second, &third]).await;
    ([first, second, third], files)
}

pub async fn a_full_fetch_lists_every_commit_oldest_first_with_the_trees_each_adds(store: Store) {
    let h = harness(store);
    let ([first, second, third], _) = history(&h).await;
    let page = fetch(&h, FetchQuery::default()).await;
    assert_eq!(ids(&page), [first.id, second.id, third.id]);
    assert_eq!(page.next, None);
    assert_eq!(page.refs, vec![("main".to_string(), Some(third.id))]);
    for (got, sent) in page.commits.iter().zip([&first, &second, &third]) {
        assert_eq!(got.raw, sent.raw);
    }
    let counts: Vec<_> = page.commits.iter().map(|c| c.trees.len()).collect();
    assert_eq!(counts, [3, 2, 3]);
    for c in &page.commits {
        for (id, body) in &c.trees {
            let hashed = pyn_object::hash_object(Algorithm::Sha256, pyn_object::Kind::Tree, body);
            assert_eq!(&hashed.unwrap(), id);
        }
    }
    let mut every = HashSet::new();
    for c in [&first, &second, &third] {
        let tree = h.svc.get_commit(&c.id).await.unwrap().tree;
        every.extend(closure(&h, tree).await);
    }
    assert_eq!(sent_trees(&page), every);

    let pointers: Vec<_> = page.commits[1]
        .pointers
        .iter()
        .map(|r| (r.path.to_string(), r.id.0, r.mode))
        .collect();
    assert_eq!(
        pointers,
        [("Source/a.cpp".to_string(), 2, Some(Mode::Shared))]
    );
}

pub async fn haves_leave_out_what_the_client_has_and_unknown_haves_are_ignored(store: Store) {
    let h = harness(store);
    let ([first, second, third], _) = history(&h).await;
    let stray = blob(&h, "not a commit").await;

    let page = fetch(
        &h,
        FetchQuery {
            haves: vec![first.id, stray],
            ..FetchQuery::default()
        },
    )
    .await;
    assert_eq!(ids(&page), [second.id, third.id]);
    let had = closure(&h, h.svc.get_commit(&first.id).await.unwrap().tree).await;
    let sent = sent_trees(&page);
    assert!(sent.is_disjoint(&had));
    let head_tree = h.svc.get_commit(&third.id).await.unwrap().tree;
    let missing: HashSet<_> = closure(&h, head_tree)
        .await
        .difference(&had)
        .copied()
        .collect();
    assert!(missing.is_subset(&sent));

    let page = fetch(
        &h,
        FetchQuery {
            haves: vec![third.id],
            ..FetchQuery::default()
        },
    )
    .await;
    assert!(page.commits.is_empty());
    assert_eq!(page.refs, vec![("main".to_string(), Some(third.id))]);

    let page = fetch(
        &h,
        FetchQuery {
            haves: vec![stray],
            ..FetchQuery::default()
        },
    )
    .await;
    assert_eq!(page.commits.len(), 3);
}

pub async fn a_fetch_pages_by_cursor_over_the_heads_it_started_with(store: Store) {
    let h = harness(store);
    let ([first, second, third], files) = history(&h).await;
    let page = fetch(
        &h,
        FetchQuery {
            limit: Some(2),
            ..FetchQuery::default()
        },
    )
    .await;
    assert_eq!(ids(&page), [first.id, second.id]);
    let cursor = page.next.clone().expect("a second page");

    let g = blob(&h, "later").await;
    let mut files = files;
    set(&mut files, "Source/later.cpp", g);
    let fourth = build(&files, Some(third.id), "fourth", 4);
    pushed(&h, &[&fourth]).await;

    let rest = fetch(
        &h,
        FetchQuery {
            cursor: Some(cursor),
            limit: Some(2),
            ..FetchQuery::default()
        },
    )
    .await;
    assert_eq!(ids(&rest), [third.id]);
    assert_eq!(rest.next, None);
    assert_eq!(rest.refs, vec![("main".to_string(), Some(third.id))]);

    let all = fetch(&h, FetchQuery::default()).await;
    assert_eq!(ids(&all), [first.id, second.id, third.id, fourth.id]);

    let mut seen = Vec::new();
    let mut cursor = None;
    loop {
        let page = fetch(
            &h,
            FetchQuery {
                cursor,
                limit: Some(1),
                ..FetchQuery::default()
            },
        )
        .await;
        assert!(page.commits.len() <= 1);
        seen.extend(ids(&page));
        cursor = page.next;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(seen, ids(&all));

    let capped = fetch(
        &h,
        FetchQuery {
            limit: Some(usize::MAX),
            ..FetchQuery::default()
        },
    )
    .await;
    assert_eq!(capped.commits.len(), 4);
}

pub async fn a_merge_commit_arrives_after_both_parents_with_the_pointers_it_adopted(store: Store) {
    let h = harness(store);
    let (map, other) = (path("Content/a.umap"), path("Source/a.cpp"));
    h.svc.checkout(&map, &user("alice"), None).await.unwrap();
    let base = put_on(&h, "main", "alice", &map, "v1", None).await.commit;
    put_on(&h, "main", "alice", &other, "one", None).await;
    h.svc
        .create_branch(&who("alice"), "feature", Some("main"))
        .await
        .unwrap();
    h.svc
        .checkout(&map, &user("bob"), Some(RevisionId(1)))
        .await
        .unwrap();
    let r2 = put_on(&h, "feature", "bob", &map, "v2", Some(1)).await;
    put_on(&h, "main", "alice", &other, "two", Some(1)).await;
    let merged = h
        .svc
        .merge(
            &who("carol"),
            MergeRequest {
                source: "feature".into(),
                target: Some("main".into()),
                fast_forward: FastForward::Allow,
                message: None,
                expect_target: None,
                resolutions: Vec::new(),
            },
        )
        .await
        .unwrap()
        .commit
        .unwrap();

    let page = fetch(&h, FetchQuery::default()).await;
    let order = ids(&page);
    assert_eq!(order.len(), 5);
    assert_eq!(order[0], base);
    assert_eq!(*order.last().unwrap(), merged);
    let at = |id: ObjectId| order.iter().position(|c| *c == id).unwrap();
    let merge = h.svc.get_commit(&merged).await.unwrap();
    assert_eq!(merge.parents.len(), 2);
    assert!(merge.revisions.is_empty());
    for p in &merge.parents {
        assert!(at(*p) < at(merged));
    }
    let got = page.commits.last().unwrap();
    assert_eq!(got.raw, merge.raw);
    let adopted: Vec<_> = got
        .pointers
        .iter()
        .map(|r| (r.path.to_string(), r.id))
        .collect();
    assert_eq!(adopted, [("Content/a.umap".to_string(), r2.id)]);
    assert_eq!(got.pointers[0].mode, Some(Mode::Exclusive));

    let have_feature = h.svc.head_of("feature").await.unwrap().unwrap();
    let page = fetch(
        &h,
        FetchQuery {
            haves: vec![have_feature],
            ..FetchQuery::default()
        },
    )
    .await;
    assert_eq!(page.commits.len(), 2, "main's later commit and the merge");
    assert!(ids(&page).contains(&merged));
    assert!(!ids(&page).contains(&have_feature));
}

pub async fn a_fetch_names_branches_or_takes_all_and_refuses_an_unknown_one(store: Store) {
    let h = harness(store);
    let empty = fetch(&h, FetchQuery::default()).await;
    assert_eq!(empty.refs, vec![("main".to_string(), None)]);
    assert!(empty.commits.is_empty());

    let ([first, second, third], files) = history(&h).await;
    h.svc
        .create_branch(&who("alice"), "old", Some(&first.id.to_hex()))
        .await
        .unwrap();
    let c = blob(&h, "side").await;
    let mut files = files;
    set(&mut files, "Source/a.cpp", c);
    files.insert("Source/side.rs".into(), (FileMode::Regular, c));
    let side = build(&files, Some(first.id), "side", 9);
    h.svc
        .push_commits(&who("alice"), "old", objects(&[&side]))
        .await
        .unwrap();

    let only_old = fetch(
        &h,
        FetchQuery {
            branches: vec!["old".into()],
            ..FetchQuery::default()
        },
    )
    .await;
    assert_eq!(ids(&only_old), [first.id, side.id]);
    assert_eq!(only_old.refs, vec![("old".to_string(), Some(side.id))]);

    let all = fetch(&h, FetchQuery::default()).await;
    assert_eq!(all.refs.len(), 2);
    assert_eq!(all.refs[0].0, "main");
    let got: HashSet<_> = ids(&all).into_iter().collect();
    assert_eq!(got, HashSet::from([first.id, second.id, third.id, side.id]));
    assert_eq!(ids(&all).iter().filter(|i| **i == first.id).count(), 1);

    let both = fetch(
        &h,
        FetchQuery {
            branches: vec!["main".into(), "old".into(), "main".into()],
            haves: vec![third.id],
            ..FetchQuery::default()
        },
    )
    .await;
    assert_eq!(ids(&both), [side.id]);
    assert_eq!(both.refs.len(), 2);

    let err = h
        .svc
        .fetch(&FetchQuery {
            branches: vec!["nope".into()],
            ..FetchQuery::default()
        })
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::BranchNotFound(_)), "{err}");
}

pub async fn a_fetch_refuses_a_bad_cursor_and_too_many_haves(store: Store) {
    let h = harness(store);
    let ([first, ..], _) = history(&h).await;
    for cursor in ["", "zz", "00", "not-a-cursor"] {
        let err = h
            .svc
            .fetch(&FetchQuery {
                cursor: Some(cursor.into()),
                ..FetchQuery::default()
            })
            .await
            .unwrap_err();
        assert!(
            matches!(err, PynError::InvalidRequest(_)),
            "{cursor}: {err}"
        );
    }
    let page = fetch(
        &h,
        FetchQuery {
            limit: Some(1),
            ..FetchQuery::default()
        },
    )
    .await;
    let cursor = page.next.unwrap();
    let err = h
        .svc
        .fetch(&FetchQuery {
            cursor: Some(cursor),
            haves: vec![first.id],
            ..FetchQuery::default()
        })
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::InvalidRequest(_)), "{err}");

    let many = vec![first.id; crate::service::FETCH_MAX_HAVES + 1];
    let err = h
        .svc
        .fetch(&FetchQuery {
            haves: many,
            ..FetchQuery::default()
        })
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::InvalidRequest(_)), "{err}");
}

/// A chain of `n` commits pushed to main, each editing one file.
async fn chain(h: &Harness, n: i64) -> Vec<ObjectId> {
    let mut files = Files::new();
    let mut parent = None;
    let mut built = Vec::new();
    for i in 1..=n {
        let b = blob(h, &format!("v{i}")).await;
        set(&mut files, "Source/a.cpp", b);
        let c = build(&files, parent, &format!("c{i}"), i);
        parent = Some(c.id);
        built.push(c);
    }
    let parts: Vec<&Built> = built.iter().collect();
    pushed(h, &parts).await;
    built.iter().map(|c| c.id).collect()
}

/// The store calls of the second page (two commits) of a fetch over a chain of `n` commits, with the first as a have.
async fn second_page_calls(store: Store, n: i64) -> usize {
    let counting = CountingStore::new(store);
    let h = harness_on(
        counting.clone(),
        Arc::new(MemoryObjectStore::new()),
        RepoId::new(&format!("chain{n}")),
        RULES,
    );
    let all = chain(&h, n).await;
    let first = fetch(
        &h,
        FetchQuery {
            haves: vec![all[0]],
            limit: Some(2),
            ..FetchQuery::default()
        },
    )
    .await;
    let cursor = first.next.expect("a second page");
    counting.take();
    let second = fetch(
        &h,
        FetchQuery {
            cursor: Some(cursor),
            haves: vec![all[0]],
            limit: Some(2),
            ..FetchQuery::default()
        },
    )
    .await;
    assert_eq!(ids(&second), all[3..5].to_vec());
    counting.take()
}

pub async fn the_work_of_a_fetch_page_does_not_grow_with_the_history(store: Store) {
    let short = second_page_calls(store.clone(), 8).await;
    let long = second_page_calls(store, 60).await;
    assert_eq!(short, long, "a page costs the same on a longer history");
}

pub async fn a_walk_past_the_examination_cap_is_refused(store: Store) {
    let h = harness(store);
    let all = chain(&h, 5).await;
    let head = *all.last().unwrap();
    let repo = h.svc.repo();
    let err = h
        .store
        .missing_commits(repo, &[head], &[], None, 10, 3)
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::InvalidRequest(_)), "{err}");
    let err = h
        .store
        .missing_commits(repo, &[all[0]], &[head], None, 10, 3)
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::InvalidRequest(_)), "{err}");
    let found = h
        .store
        .missing_commits(repo, &[head], &[], None, 10, 5)
        .await
        .unwrap();
    assert_eq!(found.iter().map(|c| c.id).collect::<Vec<_>>(), all);
    let tail = h
        .store
        .missing_commits(repo, &[head], &[all[1]], Some(&all[2]), 10, 3)
        .await
        .unwrap();
    assert_eq!(tail.iter().map(|c| c.id).collect::<Vec<_>>(), all[3..]);
}
