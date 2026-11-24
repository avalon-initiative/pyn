//! Line diffs and file lines every `MetadataStore` must satisfy, through `RepoService`.

use super::{Harness, Store, blob, harness, path, put_on, user, who};
use crate::service::{DIFF_MAX_FILE_BYTES, DiffLineKind, DiffOptions, DiffStatus};
use crate::{Deletion, FileMode, Mode, PynError, RevisionId};

async fn branch_at(h: &Harness, name: &str, start: &str) {
    h.svc
        .create_branch(&who("alice"), name, Some(start))
        .await
        .unwrap();
}

fn numbered(n: usize) -> String {
    (1..=n).map(|i| format!("l{i}\n")).collect()
}

pub async fn a_text_diff_has_hunks_counts_and_hidden_line_numbers(store: Store) {
    let h = harness(store);
    let a = path("Source/a.txt");
    put_on(&h, "main", "alice", &a, &numbered(20), None).await;
    branch_at(&h, "f", "main").await;
    let mut edited = numbered(20).replace("l10\n", "ten\n");
    edited.push_str("l21\n");
    put_on(&h, "f", "alice", &a, &edited, Some(1)).await;

    let d = h.svc.diff(None, "f", DiffOptions::default()).await.unwrap();
    assert_eq!(d.files.len(), 1);
    let f = &d.files[0];
    assert_eq!(f.status, DiffStatus::Modified);
    assert_eq!((f.additions, f.deletions), (2, 1));
    assert_eq!(f.mode, Mode::Shared);
    assert!(f.lock.is_none() && !f.binary && !f.too_large);
    assert_eq!(f.hunks.len(), 2);
    let (one, two) = (&f.hunks[0], &f.hunks[1]);
    assert_eq!(one.header, "@@ -7,7 +7,7 @@ l6");
    assert_eq!((one.hidden_before, one.hidden_after), (6, 0));
    assert_eq!((two.hidden_before, two.hidden_after), (4, 0));
    let kinds: Vec<_> = one.lines.iter().map(|l| l.kind).collect();
    assert_eq!(kinds[3..5], [DiffLineKind::Delete, DiffLineKind::Add]);
    assert_eq!(
        (
            one.lines[3].old_no,
            one.lines[3].new_no,
            one.lines[3].text.as_str()
        ),
        (Some(10), None, "l10")
    );
    assert_eq!((one.lines[4].old_no, one.lines[4].new_no), (None, Some(10)));
    let last = two.lines.last().unwrap();
    assert_eq!(
        (last.kind, last.new_no, last.text.as_str()),
        (DiffLineKind::Add, Some(21), "l21")
    );

    let wide = h
        .svc
        .diff(
            None,
            "f",
            DiffOptions {
                context: 20,
                ..DiffOptions::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(wide.files[0].hunks.len(), 1);
    let none = h
        .svc
        .diff(
            None,
            "f",
            DiffOptions {
                context: 0,
                ..DiffOptions::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(none.files[0].hunks.len(), 2);
    assert_eq!(none.files[0].hunks[0].hidden_before, 9);
    assert_eq!(none.files[0].hunks[1].hidden_before, 10);
    let err = h
        .svc
        .diff(
            None,
            "f",
            DiffOptions {
                context: 101,
                ..DiffOptions::default()
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::InvalidRequest(_)), "{err}");
}

pub async fn a_diff_tells_added_deleted_renamed_and_binary_paths_apart(store: Store) {
    let h = harness(store);
    let (keep, gone, moved) = (
        path("Source/keep.txt"),
        path("Source/gone.txt"),
        path("Source/old.txt"),
    );
    put_on(&h, "main", "alice", &keep, "same\n", None).await;
    put_on(&h, "main", "alice", &gone, "bye\n", None).await;
    put_on(&h, "main", "alice", &moved, "carried\n", None).await;
    branch_at(&h, "f", "main").await;
    let bin = h.svc.upload_blob(vec![0, 1, 2, 3]).await.unwrap();
    let carried = blob(&h, "carried\n").await;
    let fresh = blob(&h, "hello\nworld\n").await;
    let entry = |p: &str, content, base| crate::ChangeEntry {
        path: path(p),
        content,
        file_mode: FileMode::Regular,
        base,
    };
    h.svc
        .checkin_changes_on(
            "f",
            &who("alice"),
            vec![
                entry("Source/new.txt", fresh, None),
                entry("Source/blob.bin", bin, None),
                entry("Source/new-name.txt", carried, None),
            ],
            vec![
                Deletion {
                    path: gone.clone(),
                    base: Some(RevisionId(1)),
                },
                Deletion {
                    path: moved.clone(),
                    base: Some(RevisionId(1)),
                },
            ],
            "reshape".into(),
        )
        .await
        .unwrap();

    let d = h.svc.diff(None, "f", DiffOptions::default()).await.unwrap();
    let got: Vec<_> = d
        .files
        .iter()
        .map(|f| {
            (
                f.path.to_string(),
                f.status,
                f.old_path.as_ref().map(ToString::to_string),
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            ("Source/blob.bin".into(), DiffStatus::Added, None),
            ("Source/gone.txt".into(), DiffStatus::Deleted, None),
            (
                "Source/new-name.txt".into(),
                DiffStatus::Renamed,
                Some("Source/old.txt".into())
            ),
            ("Source/new.txt".into(), DiffStatus::Added, None),
        ]
    );
    assert!(d.files[0].binary && d.files[0].hunks.is_empty());
    assert_eq!((d.files[1].additions, d.files[1].deletions), (0, 1));
    assert!(d.files[2].hunks.is_empty(), "an exact rename has no lines");
    assert_eq!(d.files[3].additions, 2);
    assert_eq!(d.files[3].hunks[0].header, "@@ -0,0 +1,2 @@");
}

pub async fn a_diff_shows_the_mode_and_lock_of_each_path(store: Store) {
    let h = harness(store);
    let map = path("Content/a.umap");
    h.svc.checkout(&map, &user("alice"), None).await.unwrap();
    put_on(&h, "main", "alice", &map, "v1\n", None).await;
    branch_at(&h, "f", "main").await;
    h.svc
        .checkout_on("f", &map, &user("alice"), Some(RevisionId(1)))
        .await
        .unwrap();
    put_on(&h, "f", "alice", &map, "v2\n", Some(1)).await;

    let free = h.svc.diff(None, "f", DiffOptions::default()).await.unwrap();
    assert_eq!(free.files[0].mode, Mode::Exclusive);
    assert!(free.files[0].lock.is_none());

    h.svc
        .checkout_on("f", &map, &user("bob"), Some(RevisionId(2)))
        .await
        .unwrap();
    let held = h.svc.diff(None, "f", DiffOptions::default()).await.unwrap();
    let lock = held.files[0].lock.as_ref().expect("lock state");
    assert_eq!(lock.owner, user("bob"));
}

pub async fn a_direct_diff_measures_from_the_base_and_a_merge_base_diff_from_the_fork(
    store: Store,
) {
    let h = harness(store);
    let (a, b) = (path("Source/a.txt"), path("Source/b.txt"));
    put_on(&h, "main", "alice", &a, "a0\n", None).await;
    branch_at(&h, "f", "main").await;
    put_on(&h, "main", "alice", &b, "b\n", None).await;
    put_on(&h, "f", "alice", &a, "a1\n", Some(1)).await;

    let names = |d: &crate::service::Diff| {
        d.files
            .iter()
            .map(|f| f.path.to_string())
            .collect::<Vec<_>>()
    };
    let fork = h.svc.diff(None, "f", DiffOptions::default()).await.unwrap();
    assert_eq!(names(&fork), ["Source/a.txt"]);
    assert!(fork.from_commit.is_some());
    let direct = h
        .svc
        .diff(
            None,
            "f",
            DiffOptions {
                merge_base: false,
                ..DiffOptions::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(names(&direct), ["Source/a.txt", "Source/b.txt"]);
    assert_eq!(direct.files[1].status, DiffStatus::Deleted);
    assert_eq!(direct.from_commit, Some(direct.base_commit));
    let err = h
        .svc
        .diff(None, "nope", DiffOptions::default())
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::CommitNotFound(_)), "{err}");
}

pub async fn a_file_too_large_to_diff_is_listed_without_hunks(store: Store) {
    let h = harness(store);
    let a = path("Source/big.txt");
    put_on(&h, "main", "alice", &a, "x\n", None).await;
    branch_at(&h, "f", "main").await;
    let big = "y\n".repeat(DIFF_MAX_FILE_BYTES as usize / 2 + 1);
    let big = blob(&h, &big).await;
    h.svc
        .checkin_on(
            "f",
            &who("alice"),
            &a,
            big,
            FileMode::Regular,
            Some(RevisionId(1)),
            "big".into(),
        )
        .await
        .unwrap();
    let d = h.svc.diff(None, "f", DiffOptions::default()).await.unwrap();
    let f = &d.files[0];
    assert!(f.too_large && f.hunks.is_empty());
    assert_eq!((f.additions, f.deletions), (0, 0));
    let err = h.svc.file_lines("f", &a, 1, Some(5)).await.unwrap_err();
    assert!(matches!(err, PynError::InvalidRequest(_)), "{err}");
}

pub async fn file_lines_are_a_range_of_a_path_at_a_revision(store: Store) {
    let h = harness(store);
    let a = path("Source/a.txt");
    put_on(&h, "main", "alice", &a, &numbered(10), None).await;
    put_on(&h, "main", "alice", &a, &numbered(12), Some(1)).await;
    let first = h.svc.head_of("main").await.unwrap().unwrap();

    let r = h.svc.file_lines("main", &a, 3, Some(5)).await.unwrap();
    assert_eq!((r.total, r.start, r.revision, r.commit), (12, 3, 2, first));
    assert_eq!(r.lines, ["l3", "l4", "l5"]);
    let by_commit = h
        .svc
        .file_lines(&first.to_hex(), &a, 11, None)
        .await
        .unwrap();
    assert_eq!(by_commit.lines, ["l11", "l12"]);
    let past = h.svc.file_lines("main", &a, 40, Some(50)).await.unwrap();
    assert!(past.lines.is_empty() && past.total == 12);

    for (start, end) in [(0, None), (5, Some(4))] {
        let err = h.svc.file_lines("main", &a, start, end).await.unwrap_err();
        assert!(matches!(err, PynError::InvalidRequest(_)), "{err}");
    }
    let err = h
        .svc
        .file_lines("main", &path("Source/none.txt"), 1, None)
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::RevisionNotFound { .. }), "{err}");
    let bin = h.svc.upload_blob(vec![0, 1]).await.unwrap();
    h.svc
        .checkin_on(
            "main",
            &who("alice"),
            &path("Source/b.bin"),
            bin,
            FileMode::Regular,
            None,
            "bin".into(),
        )
        .await
        .unwrap();
    let err = h
        .svc
        .file_lines("main", &path("Source/b.bin"), 1, None)
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::InvalidRequest(_)), "{err}");
}
