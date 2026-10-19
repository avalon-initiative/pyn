//! The counts a store keeps per commit agree with the commit's snapshot.

use super::{Harness, Store, harness, path, put_on, user, who};
use crate::{Deletion, FileMode, ObjectId, RevisionId};

/// Every commit's stored counts against a recount of its snapshot and of the revisions along its first-parent chain.
pub(super) async fn counts_agree_with_snapshots(h: &Harness) {
    let repo = h.svc.repo();
    let mut commits = Vec::new();
    let mut before = None;
    loop {
        let page = h
            .store
            .commits(repo, None, before.as_ref(), 200)
            .await
            .unwrap();
        let Some(last) = page.last() else { break };
        before = Some(last.id);
        commits.extend(page);
    }
    for c in &commits {
        let snapshot = h.store.snapshot(repo, &c.id).await.unwrap();
        let mut newest = None;
        let mut next: Option<ObjectId> = Some(c.id);
        while let Some(id) = next {
            for r in h.store.commit_pointers(repo, &id).await.unwrap() {
                newest = newest.max(Some(r.created_at));
            }
            next = h
                .store
                .get_commit(repo, &id)
                .await
                .unwrap()
                .and_then(|k| k.parents.first().copied());
        }
        let got = h.store.head_counts(repo, &c.id).await.unwrap();
        let want = (
            snapshot.len() as u64,
            snapshot
                .iter()
                .filter(|r| r.mode == Some(crate::Mode::Exclusive))
                .count() as u64,
            newest,
        );
        assert_eq!(
            (got.files, got.exclusive_files, got.updated_at),
            want,
            "counts of commit {}",
            c.id
        );
    }
}

pub async fn commit_counts_follow_adds_deletes_modes_branches_and_merges(store: Store) {
    use crate::{FastForward, MergeRequest};

    let h = harness(store);
    let none = ObjectId::parse(&"0".repeat(40)).unwrap();
    assert!(matches!(
        h.store.head_counts(h.svc.repo(), &none).await,
        Err(crate::PynError::CommitNotFound(_))
    ));

    let (a, b, map) = (
        path("Source/a.cpp"),
        path("Source/b.cpp"),
        path("Content/m.umap"),
    );
    let counts = |h: &Harness, branch: &'static str| {
        let (store, svc) = (h.store.clone(), h.svc.clone());
        async move {
            let head = svc.head_of(branch).await.unwrap().unwrap();
            let c = store.head_counts(svc.repo(), &head).await.unwrap();
            (c.files, c.exclusive_files)
        }
    };
    put_on(&h, "main", "alice", &a, "a1", None).await;
    assert_eq!(counts(&h, "main").await, (1, 0));
    put_on(&h, "main", "alice", &a, "a2", Some(1)).await;
    assert_eq!(
        counts(&h, "main").await,
        (1, 0),
        "a new revision of a live path adds no file"
    );
    h.svc.checkout(&map, &user("alice"), None).await.unwrap();
    put_on(&h, "main", "alice", &map, "m1", None).await;
    put_on(&h, "main", "alice", &b, "b1", None).await;
    assert_eq!(counts(&h, "main").await, (3, 1));

    h.svc
        .create_branch(&who("alice"), "feature", None)
        .await
        .unwrap();
    assert_eq!(
        counts(&h, "feature").await,
        (3, 1),
        "a branch starts with its commit's counts"
    );
    h.svc
        .checkin_changes_on(
            "feature",
            &who("alice"),
            Vec::new(),
            vec![Deletion {
                path: b.clone(),
                base: Some(RevisionId(1)),
            }],
            "remove b".into(),
        )
        .await
        .unwrap();
    assert_eq!(counts(&h, "feature").await, (2, 1));
    assert_eq!(
        counts(&h, "main").await,
        (3, 1),
        "another branch is untouched"
    );
    put_on(&h, "feature", "alice", &b, "b2", Some(2)).await;
    assert_eq!(
        counts(&h, "feature").await,
        (3, 1),
        "a deleted path comes back"
    );
    h.svc
        .checkout_on("feature", &map, &user("alice"), Some(RevisionId(1)))
        .await
        .unwrap();
    h.svc
        .checkin_changes_on(
            "feature",
            &who("alice"),
            Vec::new(),
            vec![Deletion {
                path: map.clone(),
                base: Some(RevisionId(1)),
            }],
            "remove map".into(),
        )
        .await
        .unwrap();
    assert_eq!(
        counts(&h, "feature").await,
        (2, 0),
        "deleting an exclusive path lowers both"
    );

    put_on(&h, "main", "alice", &path("Docs/d.md"), "d", None).await;
    let done = h
        .svc
        .merge(
            &who("alice"),
            MergeRequest {
                source: "feature".into(),
                target: Some("main".into()),
                message: None,
                fast_forward: FastForward::Allow,
                expect_target: None,
                resolutions: Vec::new(),
            },
        )
        .await
        .unwrap();
    assert_eq!(done.outcome, crate::MergeOutcome::Merge);
    counts_agree_with_snapshots(&h).await;
    let summary = h.svc.summary(5).await.unwrap();
    assert_eq!(
        summary.files,
        h.svc.snapshot(None).await.unwrap().entries.len() as u64
    );
    assert_eq!(
        summary.shared_files + summary.exclusive_files,
        summary.files
    );
}

pub async fn a_listing_reads_heads_a_page_at_a_time_and_stops_at_its_bound(store: Store) {
    use crate::PynError;

    let h = harness(store);
    let names = [
        "Source/a.cpp",
        "Source/b.cpp",
        "Source/sub/c.cpp",
        "Docs/d.md",
        "Docs/e.md",
        "Content/m.umap",
        "top.txt",
    ];
    h.svc
        .checkout(&path("Content/m.umap"), &user("alice"), None)
        .await
        .unwrap();
    for name in names {
        put_on(&h, "main", "alice", &path(name), name, None).await;
    }
    h.svc
        .checkout(&path("Content/lock-only.umap"), &user("bob"), None)
        .await
        .unwrap();

    let shape = |entries: &[crate::TreeEntry]| {
        entries
            .iter()
            .map(|e| {
                (
                    e.name.clone(),
                    e.kind,
                    e.mode,
                    e.last_change.as_ref().map(|r| (r.path.to_string(), r.id)),
                    e.lock.as_ref().map(|l| l.owner.to_string()),
                )
            })
            .collect::<Vec<_>>()
    };
    let whole = h.svc.tree(None).await.unwrap();
    assert_eq!(whole.len(), 4, "Content, Docs, Source and top.txt");
    for dir in [None, Some(path("Source")), Some(path("Content"))] {
        let want = shape(&h.svc.tree(dir.as_ref()).await.unwrap());
        for page in [0, 1, 2, 3, 7, 8, 100] {
            let got = h
                .svc
                .tree_bounded("main", dir.as_ref(), page, usize::MAX)
                .await
                .unwrap();
            assert_eq!(shape(&got), want, "page {page}, folder {dir:?}");
        }
    }
    let content = h.svc.tree(Some(&path("Content"))).await.unwrap();
    assert_eq!(
        content.len(),
        2,
        "a locked path without a revision still lists"
    );

    h.svc.tree_bounded("main", None, 3, 7).await.unwrap();
    for page in [1, 3, 100] {
        let err = h.svc.tree_bounded("main", None, page, 6).await.unwrap_err();
        assert!(
            matches!(err, PynError::InvalidRequest(_)),
            "page {page}: {err}"
        );
    }
    h.svc
        .tree_bounded("main", Some(&path("Source")), 1, 3)
        .await
        .unwrap();
    let err = h
        .svc
        .tree_bounded("main", Some(&path("Source")), 1, 2)
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::InvalidRequest(_)), "{err}");
}

/// Records `text` as the policy file on main, after `base`.
async fn set_policy(h: &Harness, text: &str, base: Option<u64>) {
    let content = h.svc.upload_blob(text.as_bytes().to_vec()).await.unwrap();
    if let Some(base) = base {
        let policy = path(".pyn/pyn.toml");
        h.svc
            .checkout(&policy, &user("alice"), Some(RevisionId(base)))
            .await
            .unwrap();
    }
    h.svc
        .checkin_recorded(
            "main",
            &who("alice"),
            &path(".pyn/pyn.toml"),
            content,
            FileMode::Regular,
            base.map(RevisionId),
            "policy".into(),
        )
        .await
        .unwrap();
}

pub async fn the_summary_counts_exclusive_paths_against_the_live_policy(store: Store) {
    let h = harness(store);
    let (a, b) = (path("Source/a.cpp"), path("Source/b.cpp"));
    let both = "[meta]\ndefault = \"shared\"\n[exclusive]\npaths = [\"Source/*.cpp\"]\n";
    let one = "[meta]\ndefault = \"shared\"\n[exclusive]\npaths = [\"Source/a.cpp\"]\n";
    let none = "[meta]\ndefault = \"shared\"\n";
    // The policy file is always exclusive, so one is always counted once it exists.
    let exclusive = |h: &Harness| {
        let svc = h.svc.clone();
        async move {
            let s = svc.summary(1).await.unwrap();
            assert_eq!(s.shared_files + s.exclusive_files, s.files);
            s.exclusive_files
        }
    };

    put_on(&h, "main", "alice", &a, "a\n", None).await;
    put_on(&h, "main", "alice", &b, "b\n", None).await;
    assert_eq!(exclusive(&h).await, 0);
    set_policy(&h, both, None).await;
    assert_eq!(exclusive(&h).await, 3);

    set_policy(&h, one, Some(1)).await;
    assert_eq!(exclusive(&h).await, 2, "b became shared without a revision");
    set_policy(&h, none, Some(2)).await;
    assert_eq!(exclusive(&h).await, 1);
    put_on(&h, "main", "alice", &b, "b2\n", Some(2)).await;
    assert_eq!(exclusive(&h).await, 1, "an edit does not count it twice");

    set_policy(&h, both, Some(3)).await;
    assert_eq!(exclusive(&h).await, 3);
    set_policy(&h, none, Some(4)).await;
    assert_eq!(exclusive(&h).await, 1);
}
