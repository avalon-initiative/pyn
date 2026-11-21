//! Branch comparison every `MetadataStore` must satisfy, through `RepoService`.

use super::{Harness, Store, blob, harness, path, put_on, who};
use crate::merge::{FastForward, MergeRequest};
use crate::service::{COMPARE_MAX_COMMITS, COMPARE_MAX_PATHS};
use crate::{ChangeEntry, ChangeKind, Deletion, FileMode, PynError};

async fn branch_at(h: &Harness, name: &str, start: &str) {
    h.svc
        .create_branch(&who("alice"), name, Some(start))
        .await
        .unwrap();
}

async fn changes(h: &Harness, base: Option<&str>, head: &str) -> Vec<(String, ChangeKind)> {
    h.svc
        .compare(base, head)
        .await
        .unwrap()
        .changes
        .into_iter()
        .map(|c| (c.path.to_string(), c.kind))
        .collect()
}

pub async fn a_diverged_branch_is_ahead_and_behind_its_merge_base(store: Store) {
    let h = harness(store);
    let (a, b) = (path("Source/a.cpp"), path("Source/b.cpp"));
    put_on(&h, "main", "alice", &a, "a0", None).await;
    let base = put_on(&h, "main", "alice", &b, "b0", None).await.commit;
    branch_at(&h, "f", "main").await;
    put_on(&h, "main", "alice", &path("Source/m.cpp"), "m", None).await;
    put_on(&h, "f", "alice", &a, "a1", Some(1)).await;
    put_on(&h, "f", "alice", &path("Source/n.cpp"), "n", None).await;
    h.svc
        .checkin_changes_on(
            "f",
            &who("alice"),
            Vec::new(),
            vec![Deletion {
                path: b.clone(),
                base: Some(crate::RevisionId(1)),
            }],
            "drop b".into(),
        )
        .await
        .unwrap();

    let c = h.svc.compare(None, "f").await.unwrap();
    assert_eq!((c.ahead, c.behind, c.counts_capped), (3, 1, false));
    assert_eq!(c.merge_base, Some(base));
    assert_eq!((c.base.as_str(), c.head.as_str()), ("main", "f"));
    assert_eq!(c.head_commit, h.svc.head_of("f").await.unwrap().unwrap());
    assert!(!c.changes_truncated);
    assert_eq!(
        changes(&h, None, "f").await,
        [
            ("Source/a.cpp".into(), ChangeKind::Modified),
            ("Source/b.cpp".into(), ChangeKind::Deleted),
            ("Source/n.cpp".into(), ChangeKind::Added),
        ]
    );

    let reverse = h.svc.compare(Some("f"), "main").await.unwrap();
    assert_eq!((reverse.ahead, reverse.behind), (1, 3));
    assert_eq!(
        changes(&h, Some("f"), "main").await,
        [("Source/m.cpp".into(), ChangeKind::Added)]
    );
    assert_eq!(c.merge_base, reverse.merge_base);
}

pub async fn a_fast_forwardable_equal_or_merged_branch_has_a_zero_side(store: Store) {
    let h = harness(store);
    let a = path("Source/a.cpp");
    put_on(&h, "main", "alice", &a, "a0", None).await;
    branch_at(&h, "f", "main").await;
    let same = h.svc.compare(None, "f").await.unwrap();
    assert_eq!((same.ahead, same.behind), (0, 0));
    assert!(same.changes.is_empty() && !same.changes_truncated);
    assert_eq!(same.merge_base, Some(same.head_commit));

    put_on(&h, "f", "alice", &a, "a1", Some(1)).await;
    put_on(&h, "f", "alice", &path("Source/b.cpp"), "b", None).await;
    let ahead = h.svc.compare(None, "f").await.unwrap();
    assert_eq!((ahead.ahead, ahead.behind), (2, 0));
    assert_eq!(ahead.merge_base, Some(ahead.base_commit));
    assert_eq!(changes(&h, None, "f").await.len(), 2);

    let behind = h.svc.compare(Some("f"), "main").await.unwrap();
    assert_eq!((behind.ahead, behind.behind), (0, 2));
    assert!(behind.changes.is_empty());

    let merge = MergeRequest {
        source: "f".into(),
        target: Some("main".into()),
        fast_forward: FastForward::Never,
        message: None,
        expect_target: None,
        resolutions: Vec::new(),
    };
    h.svc.merge(&who("alice"), merge).await.unwrap();
    let merged = h.svc.compare(None, "f").await.unwrap();
    assert_eq!((merged.ahead, merged.behind), (0, 1));
    assert!(merged.changes.is_empty());
    let back = h.svc.compare(Some("f"), "main").await.unwrap();
    assert_eq!((back.ahead, back.behind), (1, 0));
    assert_eq!(changes(&h, Some("f"), "main").await, []);

    let id = same.head_commit.to_hex();
    let by_commit = h.svc.compare(Some(&id), "main").await.unwrap();
    assert_eq!(by_commit.base_commit, same.head_commit);
}

pub async fn a_comparison_names_an_unknown_branch_with_the_usual_error(store: Store) {
    let h = harness(store);
    put_on(&h, "main", "alice", &path("Source/a.cpp"), "a", None).await;
    for (base, head) in [(None, "nope"), (Some("nope"), "main")] {
        let err = h.svc.compare(base, head).await.unwrap_err();
        assert!(matches!(err, PynError::CommitNotFound(_)), "{err}");
    }
}

pub async fn a_comparison_lists_a_bounded_number_of_paths(store: Store) {
    let h = harness(store);
    put_on(&h, "main", "alice", &path("Source/a.cpp"), "a", None).await;
    branch_at(&h, "f", "main").await;
    branch_at(&h, "g", "main").await;
    let content = blob(&h, "x").await;
    let entries = |n: usize| -> Vec<ChangeEntry> {
        (0..n)
            .map(|i| ChangeEntry {
                path: path(&format!("Source/gen/f{i:04}.cpp")),
                content,
                file_mode: FileMode::Regular,
                base: None,
            })
            .collect()
    };
    let at = |branch: &'static str, n: usize| {
        let h = &h;
        let entries = entries(n);
        async move {
            h.svc
                .checkin_changes_on(branch, &who("alice"), entries, Vec::new(), "many".into())
                .await
                .unwrap();
            h.svc.compare(None, branch).await.unwrap()
        }
    };
    let exact = at("f", COMPARE_MAX_PATHS).await;
    assert_eq!(exact.changes.len(), COMPARE_MAX_PATHS);
    assert!(!exact.changes_truncated);
    let more = at("g", COMPARE_MAX_PATHS + 1).await;
    assert_eq!(more.changes.len(), COMPARE_MAX_PATHS);
    assert!(more.changes_truncated);
    assert_eq!(more.changes[0].path.to_string(), "Source/gen/f0000.cpp");
}

pub async fn a_comparison_caps_its_commit_counts(store: Store) {
    let h = harness(store);
    let a = path("Source/a.cpp");
    put_on(&h, "main", "alice", &a, "0", None).await;
    branch_at(&h, "f", "main").await;
    let content = blob(&h, "x").await;
    for i in 0..=COMPARE_MAX_COMMITS {
        let entry = ChangeEntry {
            path: path(&format!("Source/gen/c{i:04}.cpp")),
            content,
            file_mode: FileMode::Regular,
            base: None,
        };
        h.svc
            .checkin_changes_on("f", &who("alice"), vec![entry], Vec::new(), "c".into())
            .await
            .unwrap();
        if i + 1 == COMPARE_MAX_COMMITS {
            let c = h.svc.compare(None, "f").await.unwrap();
            assert_eq!(c.ahead as usize, COMPARE_MAX_COMMITS);
            assert!(!c.counts_capped && !c.changes_truncated);
        }
    }
    let c = h.svc.compare(None, "f").await.unwrap();
    assert_eq!((c.ahead as usize, c.behind), (COMPARE_MAX_COMMITS, 0));
    assert!(c.counts_capped && c.changes_truncated);
    assert_eq!(c.changes.len(), COMPARE_MAX_COMMITS);
}

pub async fn a_branch_reports_the_committed_time_of_its_head(store: Store) {
    let h = harness(store);
    let a = path("Source/a.cpp");
    let empty = h.svc.branch("main").await.unwrap();
    assert_eq!(h.svc.branch_updated_at(&empty).await.unwrap(), None);
    put_on(&h, "main", "alice", &a, "a0", None).await;
    branch_at(&h, "f", "main").await;
    h.clock.advance(chrono::Duration::hours(2));
    put_on(&h, "f", "alice", &a, "a1", Some(1)).await;
    let time = |name: &'static str| {
        let h = &h;
        async move {
            let b = h.svc.branch(name).await.unwrap();
            h.svc.branch_updated_at(&b).await.unwrap().unwrap()
        }
    };
    let (main, f) = (time("main").await, time("f").await);
    assert_eq!(f - main, chrono::Duration::hours(2));
    let head = h.svc.head_of("f").await.unwrap().unwrap();
    assert_eq!(f, h.svc.get_commit(&head).await.unwrap().created_at);
}

pub async fn the_summary_of_a_branch_counts_that_branchs_files(store: Store) {
    let h = harness(store);
    put_on(&h, "main", "alice", &path("Source/a.cpp"), "a", None).await;
    branch_at(&h, "f", "main").await;
    h.clock.advance(chrono::Duration::hours(1));
    put_on(&h, "f", "alice", &path("Source/b.cpp"), "b", None).await;
    let main = h.svc.summary_on(None, 5).await.unwrap();
    let f = h.svc.summary_on(Some("f"), 5).await.unwrap();
    assert_eq!((main.files, f.files), (1, 2));
    assert_eq!(f.exclusive_files, 0);
    assert!(f.updated_at > main.updated_at);
    assert_eq!(f.default_branch, "main");
    assert_eq!(f.branch_count, 2);
    let err = h.svc.summary_on(Some("nope"), 5).await.unwrap_err();
    assert!(matches!(err, PynError::BranchNotFound(_)), "{err}");
}
