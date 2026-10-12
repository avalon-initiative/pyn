//! Merge behaviour every `MetadataStore` must satisfy, through `RepoService`.

use super::{Harness, Store, blob, harness, path, put_on, user, who};
use crate::access::{Permission, Principal};
use crate::merge::{ConflictKind, FastForward, MergeOutcome, MergeRequest, Resolution};
use crate::{FileMode, Mode, ObjectId, PynError, RevisionId};

fn request(source: &str, target: &str) -> MergeRequest {
    MergeRequest {
        source: source.to_string(),
        target: Some(target.to_string()),
        fast_forward: FastForward::Allow,
        message: None,
        expect_target: None,
        resolutions: Vec::new(),
    }
}

async fn head(h: &Harness, branch: &str) -> ObjectId {
    h.svc.head_of(branch).await.unwrap().unwrap()
}

async fn branch_at(h: &Harness, name: &str, start: &str) {
    h.svc
        .create_branch(&who("alice"), name, Some(start))
        .await
        .unwrap();
}

async fn revision_numbers(h: &Harness, branch: &str) -> Vec<(String, u64)> {
    h.svc
        .snapshot_on(branch)
        .await
        .unwrap()
        .entries
        .into_iter()
        .map(|e| (e.path.to_string(), e.revision.id.0))
        .collect()
}

async fn text_on(h: &Harness, branch: &str, p: &str) -> String {
    let (_, bytes) = h.svc.read_on(branch, &path(p), None).await.unwrap();
    String::from_utf8(bytes).unwrap()
}

async fn verified(h: &Harness) {
    let report = h.svc.verify(None, true).await.unwrap();
    assert!(report.problems.is_empty(), "{:?}", report.problems);
}

pub async fn merge_bases_are_the_lowest_common_ancestors(store: Store) {
    let h = harness(store);
    let repo = h.svc.repo().clone();
    let a = path("Source/a.cpp");
    let c0 = put_on(&h, "main", "alice", &a, "0\n", None).await.commit;
    branch_at(&h, "x", "main").await;
    branch_at(&h, "y", "main").await;
    let x1 = put_on(&h, "x", "alice", &path("Source/x.cpp"), "x\n", None)
        .await
        .commit;
    let y1 = put_on(&h, "y", "alice", &path("Source/y.cpp"), "y\n", None)
        .await
        .commit;
    let bases = |a: ObjectId, b: ObjectId| {
        let store = h.store.clone();
        let repo = repo.clone();
        async move { store.merge_bases(&repo, &a, &b).await.unwrap() }
    };
    assert_eq!(bases(x1, y1).await, vec![c0]);
    assert_eq!(bases(y1, x1).await, vec![c0]);
    assert_eq!(bases(c0, x1).await, vec![c0]);
    assert_eq!(bases(x1, x1).await, vec![x1]);

    let m1 = h
        .svc
        .merge(&who("alice"), request("y", "x"))
        .await
        .unwrap()
        .commit
        .unwrap();
    branch_at(&h, "y2", &y1.to_hex()).await;
    let m2 = h
        .svc
        .merge(&who("alice"), request(&x1.to_hex(), "y2"))
        .await
        .unwrap()
        .commit
        .unwrap();
    let mut crossed = vec![x1, y1];
    crossed.sort_by_key(ObjectId::to_hex);
    assert_eq!(bases(m1, m2).await, crossed);
    assert_eq!(bases(m1, x1).await, vec![x1]);
}

pub async fn a_merge_fast_forwards_or_is_up_to_date_without_a_new_commit(store: Store) {
    let h = harness(store);
    let (a, b) = (path("Source/a.cpp"), path("Source/b.cpp"));
    put_on(&h, "main", "alice", &a, "one", None).await;
    branch_at(&h, "feature", "main").await;
    let on_feature = put_on(&h, "feature", "bob", &b, "two", None).await;

    let preview = h
        .svc
        .merge_preview(&request("feature", "main"))
        .await
        .unwrap();
    assert_eq!(preview.outcome, MergeOutcome::FastForward);
    assert_eq!(preview.changes.len(), 1);
    assert_eq!(preview.changes[0].path, b);
    assert_eq!(preview.changes[0].revision, Some(on_feature.id));
    assert_eq!(
        h.svc
            .commits_on("main", None, None)
            .await
            .unwrap()
            .commits
            .len(),
        1
    );

    let done = h
        .svc
        .merge(&who("alice"), request("feature", "main"))
        .await
        .unwrap();
    assert_eq!(done.outcome, MergeOutcome::FastForward);
    assert_eq!(done.commit, Some(on_feature.commit));
    assert_eq!(head(&h, "main").await, on_feature.commit);
    assert_eq!(
        h.svc
            .commits_on("main", None, None)
            .await
            .unwrap()
            .commits
            .len(),
        2
    );

    for (source, target) in [("feature", "main"), ("main", "feature"), ("main", "main")] {
        let again = h
            .svc
            .merge(&who("alice"), request(source, target))
            .await
            .unwrap();
        assert_eq!(
            again.outcome,
            MergeOutcome::UpToDate,
            "{source} into {target}"
        );
        assert_eq!(again.commit, None);
    }
    h.svc
        .delete_branch(&who("alice"), "feature", false)
        .await
        .unwrap();
    verified(&h).await;
}

pub async fn a_merge_can_insist_on_a_commit_or_refuse_to_make_one(store: Store) {
    let h = harness(store);
    let (a, b, c) = (
        path("Source/a.cpp"),
        path("Source/b.cpp"),
        path("Source/c.cpp"),
    );
    put_on(&h, "main", "alice", &a, "one", None).await;
    branch_at(&h, "ahead", "main").await;
    branch_at(&h, "behind", "main").await;
    branch_at(&h, "diverged", "main").await;
    put_on(&h, "ahead", "alice", &b, "two", None).await;
    put_on(&h, "diverged", "alice", &c, "three", None).await;
    put_on(&h, "main", "alice", &a, "four", Some(1)).await;

    let mut only = request("diverged", "main");
    only.fast_forward = FastForward::Only;
    let err = h.svc.merge(&who("alice"), only).await.unwrap_err();
    assert!(matches!(err, PynError::NotFastForward(_)), "{err}");

    let mut never = request("ahead", "behind");
    never.fast_forward = FastForward::Never;
    let made = h.svc.merge(&who("alice"), never).await.unwrap();
    assert_eq!(made.outcome, MergeOutcome::Merge);
    let commit = h.svc.get_commit(&made.commit.unwrap()).await.unwrap();
    assert_eq!(commit.parents.len(), 2);
    assert!(commit.revisions.is_empty());
    assert_eq!(commit.message, "Merge branch 'ahead' into behind");
    verified(&h).await;
}

pub async fn a_merge_of_diverged_branches_makes_a_commit_with_two_parents(store: Store) {
    let h = harness(store);
    let (a, b) = (path("Source/a.cpp"), path("Source/b.cpp"));
    put_on(&h, "main", "alice", &a, "a1", None).await;
    put_on(&h, "main", "alice", &b, "b1", None).await;
    branch_at(&h, "feature", "main").await;
    let on_feature = put_on(&h, "feature", "bob", &a, "a2", Some(1)).await;
    let on_main = put_on(&h, "main", "alice", &b, "b2", Some(1)).await;

    let preview = h
        .svc
        .merge_preview(&request("feature", "main"))
        .await
        .unwrap();
    assert_eq!(preview.outcome, MergeOutcome::Merge);
    assert_eq!(preview.bases.len(), 1);
    assert!(!preview.virtual_base && preview.conflicts.is_empty());
    assert_eq!(h.svc.head_of("main").await.unwrap(), Some(on_main.commit));

    let mut asked = request("feature", "main");
    asked.message = Some("bring in feature".into());
    asked.expect_target = Some(on_main.commit);
    let done = h.svc.merge(&who("carol"), asked).await.unwrap();
    assert_eq!(done.outcome, MergeOutcome::Merge);
    let merged = h.svc.get_commit(&done.commit.unwrap()).await.unwrap();
    assert_eq!(merged.parents, vec![on_main.commit, on_feature.commit]);
    assert_eq!(merged.message, "bring in feature");
    assert_eq!(merged.author.name, "carol");
    assert!(merged.revisions.is_empty());
    assert_eq!(head(&h, "main").await, merged.id);
    assert_eq!(
        revision_numbers(&h, "main").await,
        vec![("Source/a.cpp".into(), 2), ("Source/b.cpp".into(), 2)]
    );
    assert_eq!(text_on(&h, "main", "Source/a.cpp").await, "a2");
    assert_eq!(text_on(&h, "main", "Source/b.cpp").await, "b2");
    assert_eq!(h.svc.history(&a).await.unwrap().len(), 2);

    let next = put_on(&h, "main", "alice", &a, "a3", Some(2)).await;
    assert_eq!(next.id, RevisionId(3));
    let log = h.svc.commits_on("main", None, None).await.unwrap().commits;
    assert!(log.iter().any(|c| c.id == on_feature.commit));
    h.svc
        .delete_branch(&who("alice"), "feature", false)
        .await
        .unwrap();
    verified(&h).await;
}

pub async fn text_changed_in_different_places_merges_into_a_new_revision(store: Store) {
    let h = harness(store);
    let a = path("Source/a.cpp");
    put_on(&h, "main", "alice", &a, "1\n2\n3\n4\n5\n", None).await;
    branch_at(&h, "feature", "main").await;
    put_on(&h, "feature", "bob", &a, "1\n2\n3\n4\nfive\n", Some(1)).await;
    put_on(&h, "main", "alice", &a, "one\n2\n3\n4\n5\n", Some(1)).await;

    let preview = h
        .svc
        .merge_preview(&request("feature", "main"))
        .await
        .unwrap();
    assert_eq!(preview.outcome, MergeOutcome::Merge);
    let change = &preview.changes[0];
    assert_eq!(change.revision, None);
    let inputs = change.inputs.as_ref().unwrap();
    assert_eq!(inputs.base.as_ref().unwrap().revision, Some(RevisionId(1)));
    assert_eq!(inputs.ours.as_ref().unwrap().revision, Some(RevisionId(3)));
    assert_eq!(
        inputs.theirs.as_ref().unwrap().revision,
        Some(RevisionId(2))
    );

    let done = h
        .svc
        .merge(&who("alice"), request("feature", "main"))
        .await
        .unwrap();
    let commit = h.svc.get_commit(&done.commit.unwrap()).await.unwrap();
    assert_eq!(commit.revisions.len(), 1);
    assert_eq!(commit.revisions[0].id, RevisionId(4));
    assert_eq!(
        text_on(&h, "main", "Source/a.cpp").await,
        "one\n2\n3\n4\nfive\n"
    );
    assert_eq!(h.svc.history(&a).await.unwrap().len(), 4);
    verified(&h).await;
}

pub async fn overlapping_text_changes_conflict_and_a_resolution_completes_the_merge(store: Store) {
    let h = harness(store);
    let (a, b) = (path("Source/a.cpp"), path("Source/b.cpp"));
    put_on(&h, "main", "alice", &a, "1\n2\n3\n", None).await;
    branch_at(&h, "feature", "main").await;
    put_on(&h, "feature", "bob", &a, "1\ntheirs\n3\n", Some(1)).await;
    put_on(&h, "feature", "bob", &b, "new\n", None).await;
    let before = put_on(&h, "main", "alice", &a, "1\nours\n3\n", Some(1)).await;

    let preview = h
        .svc
        .merge_preview(&request("feature", "main"))
        .await
        .unwrap();
    assert_eq!(preview.outcome, MergeOutcome::Conflicted);
    assert_eq!(preview.conflicts.len(), 1);
    let conflict = &preview.conflicts[0];
    assert_eq!(
        (&conflict.path, conflict.kind, conflict.mode),
        (&a, ConflictKind::Content, Mode::Shared)
    );
    assert_eq!(
        conflict.inputs.ours.as_ref().unwrap().revision,
        Some(RevisionId(3))
    );
    assert_eq!(
        conflict.inputs.theirs.as_ref().unwrap().revision,
        Some(RevisionId(2))
    );
    assert_eq!(
        conflict.inputs.base.as_ref().unwrap().revision,
        Some(RevisionId(1))
    );
    assert_eq!(preview.changes.len(), 1);
    assert_eq!(preview.changes[0].path, b);

    let err = h
        .svc
        .merge(&who("alice"), request("feature", "main"))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, PynError::MergeConflict(p) if p == &vec![a.clone()]),
        "{err}"
    );
    assert_eq!(head(&h, "main").await, before.commit);
    assert_eq!(h.svc.history(&b).await.unwrap().len(), 1);
    assert_eq!(
        h.svc
            .commits_on("main", None, None)
            .await
            .unwrap()
            .commits
            .len(),
        2
    );

    let mut resolved = request("feature", "main");
    resolved.resolutions = vec![Resolution {
        path: a.clone(),
        content: blob(&h, "1\nboth\n3\n").await,
        file_mode: FileMode::Regular,
    }];
    let done = h.svc.merge(&who("alice"), resolved).await.unwrap();
    assert_eq!(done.outcome, MergeOutcome::Merge);
    assert_eq!(text_on(&h, "main", "Source/a.cpp").await, "1\nboth\n3\n");
    assert_eq!(text_on(&h, "main", "Source/b.cpp").await, "new\n");
    let commit = h.svc.get_commit(&done.commit.unwrap()).await.unwrap();
    assert_eq!(commit.parents.len(), 2);
    assert_eq!(commit.revisions.len(), 1);
    verified(&h).await;
}

pub async fn a_merge_rejects_a_resolution_for_a_path_it_does_not_touch(store: Store) {
    let h = harness(store);
    let (a, b) = (path("Source/a.cpp"), path("Source/b.cpp"));
    put_on(&h, "main", "alice", &a, "1\n", None).await;
    put_on(&h, "main", "alice", &b, "b\n", None).await;
    branch_at(&h, "feature", "main").await;
    put_on(&h, "feature", "bob", &a, "2\n", Some(1)).await;
    put_on(&h, "main", "alice", &path("Source/c.cpp"), "c\n", None).await;
    let mut asked = request("feature", "main");
    asked.resolutions = vec![Resolution {
        path: b,
        content: blob(&h, "x").await,
        file_mode: FileMode::Regular,
    }];
    let err = h.svc.merge(&who("alice"), asked).await.unwrap_err();
    assert!(matches!(err, PynError::InvalidRequest(_)), "{err}");
}

pub async fn a_binary_file_changed_on_both_sides_is_a_whole_file_conflict(store: Store) {
    let h = harness(store);
    let bin = path("Source/img.bin");
    let put = |branch: &'static str, bytes: Vec<u8>, base: Option<u64>| {
        let (svc, objects) = (h.svc.clone(), h.objects.clone());
        let bin = bin.clone();
        async move {
            let c = crate::ObjectStore::put(&*objects, bytes).await.unwrap();
            svc.checkin_on(
                branch,
                &who("alice"),
                &bin,
                c,
                FileMode::Regular,
                base.map(RevisionId),
                "bin".into(),
            )
            .await
            .unwrap()
        }
    };
    put("main", vec![0, 1, 2, 3], None).await;
    branch_at(&h, "feature", "main").await;
    put("feature", vec![0, 1, 2, 9], Some(1)).await;
    put("main", vec![0, 7, 2, 3], Some(1)).await;

    let preview = h
        .svc
        .merge_preview(&request("feature", "main"))
        .await
        .unwrap();
    assert_eq!(preview.outcome, MergeOutcome::Conflicted);
    assert_eq!(preview.conflicts[0].kind, ConflictKind::Binary);
    let err = h
        .svc
        .merge(&who("alice"), request("feature", "main"))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::MergeConflict(_)), "{err}");

    let mut resolved = request("feature", "main");
    resolved.resolutions = vec![Resolution {
        path: bin.clone(),
        content: crate::ObjectStore::put(&*h.objects, vec![0, 1, 2, 9])
            .await
            .unwrap(),
        file_mode: FileMode::Regular,
    }];
    let done = h.svc.merge(&who("alice"), resolved).await.unwrap();
    assert!(done.commit.is_some());
    let (rev, bytes) = h.svc.read_on("main", &bin, None).await.unwrap();
    assert_eq!((rev.id, bytes), (RevisionId(4), vec![0, 1, 2, 9]));
    verified(&h).await;
}

pub async fn file_modes_merge_apart_from_content(store: Store) {
    let h = harness(store);
    let a = path("Source/run.sh");
    let put = |branch: &'static str, text: &'static str, mode: FileMode, base: Option<u64>| {
        let (svc, objects) = (h.svc.clone(), h.objects.clone());
        let a = a.clone();
        async move {
            let c = crate::ObjectStore::put(&*objects, text.as_bytes().to_vec())
                .await
                .unwrap();
            svc.checkin_on(
                branch,
                &who("alice"),
                &a,
                c,
                mode,
                base.map(RevisionId),
                "x".into(),
            )
            .await
            .unwrap()
        }
    };
    put("main", "echo 1\n", FileMode::Regular, None).await;
    branch_at(&h, "feature", "main").await;
    put("feature", "echo 1\n", FileMode::Executable, Some(1)).await;
    put("main", "echo 2\n", FileMode::Regular, Some(1)).await;
    let done = h
        .svc
        .merge(&who("alice"), request("feature", "main"))
        .await
        .unwrap();
    assert_eq!(done.outcome, MergeOutcome::Merge);
    let snap = h.svc.snapshot_on("main").await.unwrap();
    assert_eq!(snap.entries[0].revision.file_mode, FileMode::Executable);
    assert_eq!(text_on(&h, "main", "Source/run.sh").await, "echo 2\n");
    verified(&h).await;
}

pub async fn an_exclusive_path_takes_the_later_revision_without_a_lock(store: Store) {
    let h = harness(store);
    let (map, other) = (path("Content/a.umap"), path("Source/a.cpp"));
    h.svc.checkout(&map, &user("alice"), None).await.unwrap();
    put_on(&h, "main", "alice", &map, "v1", None).await;
    put_on(&h, "main", "alice", &other, "one", None).await;
    branch_at(&h, "feature", "main").await;
    h.svc
        .checkout(&map, &user("bob"), Some(RevisionId(1)))
        .await
        .unwrap();
    let r2 = put_on(&h, "feature", "bob", &map, "v2", Some(1)).await;
    put_on(&h, "main", "alice", &other, "two", Some(1)).await;

    let preview = h
        .svc
        .merge_preview(&request("feature", "main"))
        .await
        .unwrap();
    assert_eq!(preview.outcome, MergeOutcome::Merge);
    assert_eq!(preview.changes.len(), 1);
    assert_eq!(
        (preview.changes[0].revision, preview.changes[0].mode),
        (Some(r2.id), Mode::Exclusive)
    );

    let done = h
        .svc
        .merge(&who("carol"), request("feature", "main"))
        .await
        .unwrap();
    let commit = h.svc.get_commit(&done.commit.unwrap()).await.unwrap();
    assert!(commit.revisions.is_empty());
    assert_eq!(text_on(&h, "main", "Content/a.umap").await, "v2");
    assert_eq!(h.svc.history(&map).await.unwrap().len(), 2);

    let back = h
        .svc
        .merge(&who("carol"), request("main", "feature"))
        .await
        .unwrap();
    assert_eq!(back.outcome, MergeOutcome::FastForward);
    assert_eq!(head(&h, "feature").await, commit.id);
    verified(&h).await;
}

async fn converted_setup(h: &Harness) -> (crate::RepoPath, crate::Revision, crate::Revision) {
    let a = path("Source/a.cpp");
    put_on(h, "main", "alice", &a, "base\n", None).await;
    put_on(h, "main", "alice", &path("Source/other.cpp"), "o\n", None).await;
    branch_at(h, "feature", "main").await;
    let diverged = put_on(h, "feature", "bob", &a, "feature edit\n", Some(1)).await;
    let policy = blob(
        h,
        "[meta]\ndefault = \"shared\"\n[exclusive]\npaths = [\"Source/a.cpp\"]\n",
    )
    .await;
    h.svc
        .checkin(
            &who("alice"),
            &path(".pyn/pyn.toml"),
            policy,
            None,
            "a.cpp is exclusive".into(),
        )
        .await
        .unwrap();
    let conversion = h.svc.history(&a).await.unwrap().last().cloned().unwrap();
    assert_eq!(conversion.mode, Some(Mode::Exclusive));
    assert!(diverged.id < conversion.id);
    (a, diverged, conversion)
}

pub async fn a_shared_version_that_met_a_conversion_is_a_conflict_the_person_decides(store: Store) {
    let h = harness(store);
    let (a, _, conversion) = converted_setup(&h).await;
    put_on(&h, "feature", "bob", &path("Source/b.cpp"), "b\n", None).await;

    let preview = h
        .svc
        .merge_preview(&request("feature", "main"))
        .await
        .unwrap();
    assert_eq!(preview.outcome, MergeOutcome::Conflicted);
    assert_eq!(preview.conflicts.len(), 1);
    assert_eq!(preview.conflicts[0].kind, ConflictKind::ExclusiveConversion);
    assert_eq!(preview.conflicts[0].mode, Mode::Exclusive);
    assert_eq!(
        preview.changes.len(),
        1,
        "the rest of the merge is computed as usual"
    );
    let before = head(&h, "main").await;
    let err = h
        .svc
        .merge(&who("alice"), request("feature", "main"))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::MergeConflict(_)), "{err}");
    assert_eq!(head(&h, "main").await, before);

    let mut keep_main = request("feature", "main");
    keep_main.resolutions = vec![Resolution {
        path: a.clone(),
        content: conversion.content,
        file_mode: FileMode::Regular,
    }];
    let kept = h.svc.merge(&who("alice"), keep_main).await.unwrap();
    assert!(kept.commit.is_some());
    assert_eq!(
        h.svc.history(&a).await.unwrap().len(),
        3,
        "no revision and no lock for the later content"
    );
    assert_eq!(text_on(&h, "main", "Source/a.cpp").await, "base\n");
}

pub async fn choosing_the_diverged_version_of_a_converted_path_takes_the_lock(store: Store) {
    let h = harness(store);
    let (a, diverged, conversion) = converted_setup(&h).await;
    let mut choose = request("feature", "main");
    choose.resolutions = vec![Resolution {
        path: a.clone(),
        content: diverged.content,
        file_mode: FileMode::Regular,
    }];

    let err = h.svc.merge(&who("bob"), choose.clone()).await.unwrap_err();
    assert!(matches!(err, PynError::LockRequired(_)), "{err}");
    h.svc
        .checkout(&a, &user("alice"), Some(conversion.id))
        .await
        .unwrap();
    let err = h.svc.merge(&who("bob"), choose.clone()).await.unwrap_err();
    assert!(matches!(err, PynError::LockHeld { .. }), "{err}");
    assert_eq!(h.svc.history(&a).await.unwrap().len(), 3);

    let done = h.svc.merge(&who("alice"), choose).await.unwrap();
    let commit = h.svc.get_commit(&done.commit.unwrap()).await.unwrap();
    assert_eq!(commit.revisions.len(), 1);
    assert_eq!(commit.revisions[0].id, RevisionId(conversion.id.0 + 1));
    assert_eq!(commit.revisions[0].mode, Some(Mode::Exclusive));
    assert_eq!(text_on(&h, "main", "Source/a.cpp").await, "feature edit\n");
    assert!(
        h.svc.locks().await.unwrap().is_empty(),
        "the lock goes with the revision"
    );
    verified(&h).await;
}

pub async fn updating_a_branch_from_a_conversion_conflicts_the_same_way(store: Store) {
    let h = harness(store);
    let (a, diverged, conversion) = converted_setup(&h).await;
    let preview = h
        .svc
        .merge_preview(&request("main", "feature"))
        .await
        .unwrap();
    assert_eq!(preview.outcome, MergeOutcome::Conflicted);
    let conflict = &preview.conflicts[0];
    assert_eq!(
        (conflict.path.clone(), conflict.kind),
        (a.clone(), ConflictKind::ExclusiveConversion)
    );
    assert_eq!(
        conflict.inputs.ours.as_ref().unwrap().revision,
        Some(diverged.id)
    );
    assert_eq!(
        conflict.inputs.theirs.as_ref().unwrap().revision,
        Some(conversion.id)
    );

    let mut take_theirs = request("main", "feature");
    take_theirs.resolutions = vec![Resolution {
        path: a.clone(),
        content: conversion.content,
        file_mode: FileMode::Regular,
    }];
    h.svc.merge(&who("bob"), take_theirs).await.unwrap();
    assert_eq!(text_on(&h, "feature", "Source/a.cpp").await, "base\n");
    assert_eq!(h.svc.history(&a).await.unwrap().len(), 3);

    verified(&h).await;
}

pub async fn an_exclusive_path_converted_to_shared_merges_by_content(store: Store) {
    let h = harness(store);
    let map = path("Content/a.umap");
    h.svc.checkout(&map, &user("alice"), None).await.unwrap();
    put_on(&h, "main", "alice", &map, "1\n2\n3\n", None).await;
    branch_at(&h, "feature", "main").await;
    h.svc
        .checkout(&map, &user("alice"), Some(RevisionId(1)))
        .await
        .unwrap();
    put_on(&h, "main", "alice", &map, "1\n2\nthree\n", Some(1)).await;
    let shared = blob(&h, "[meta]\ndefault = \"shared\"\n").await;
    h.svc
        .checkin(
            &who("alice"),
            &path(".pyn/pyn.toml"),
            shared,
            None,
            "everything shared".into(),
        )
        .await
        .unwrap();
    assert_eq!(h.svc.mode_for(&map), Mode::Shared);
    put_on(&h, "feature", "bob", &map, "ONE\n2\n3\n", Some(1)).await;
    let done = h
        .svc
        .merge(&who("alice"), request("main", "feature"))
        .await
        .unwrap();
    assert_eq!(done.outcome, MergeOutcome::Merge);
    assert_eq!(
        text_on(&h, "feature", "Content/a.umap").await,
        "ONE\n2\nthree\n"
    );
    verified(&h).await;
}

pub async fn a_criss_cross_merge_uses_a_virtual_base(store: Store) {
    let h = harness(store);
    let a = path("Source/a.cpp");
    let text = |l: [&str; 5]| l.iter().map(|s| format!("{s}\n")).collect::<String>();
    put_on(
        &h,
        "main",
        "alice",
        &a,
        &text(["1", "2", "3", "4", "5"]),
        None,
    )
    .await;
    branch_at(&h, "x", "main").await;
    branch_at(&h, "y", "main").await;
    let x1 = put_on(
        &h,
        "x",
        "alice",
        &a,
        &text(["X", "2", "3", "4", "5"]),
        Some(1),
    )
    .await;
    let y1 = put_on(
        &h,
        "y",
        "alice",
        &a,
        &text(["1", "2", "3", "4", "Y"]),
        Some(1),
    )
    .await;
    branch_at(&h, "x1", &x1.commit.to_hex()).await;
    branch_at(&h, "y1", &y1.commit.to_hex()).await;
    h.svc
        .merge(&who("alice"), request("y1", "x"))
        .await
        .unwrap();
    h.svc
        .merge(&who("alice"), request("x1", "y"))
        .await
        .unwrap();
    assert_eq!(
        text_on(&h, "x", "Source/a.cpp").await,
        text(["X", "2", "3", "4", "Y"])
    );
    let rx = put_on(
        &h,
        "x",
        "alice",
        &a,
        &text(["X", "two", "3", "4", "Y"]),
        Some(4),
    )
    .await;
    assert!(rx.id.0 >= 4);
    let latest = h.svc.history_on("y", &a).await.unwrap().last().unwrap().id;
    put_on(
        &h,
        "y",
        "alice",
        &a,
        &text(["X", "2", "3", "four", "Y"]),
        Some(latest.0),
    )
    .await;

    let preview = h.svc.merge_preview(&request("y", "x")).await.unwrap();
    assert!(preview.virtual_base, "{preview:?}");
    assert_eq!(preview.bases.len(), 2);
    assert_eq!(
        preview.outcome,
        MergeOutcome::Merge,
        "{:?}",
        preview.conflicts
    );
    let done = h.svc.merge(&who("alice"), request("y", "x")).await.unwrap();
    assert!(done.commit.is_some());
    assert_eq!(
        text_on(&h, "x", "Source/a.cpp").await,
        text(["X", "two", "3", "four", "Y"])
    );
    verified(&h).await;
}

pub async fn a_merge_computed_on_an_old_head_is_refused(store: Store) {
    let h = harness(store);
    let a = path("Source/a.cpp");
    put_on(&h, "main", "alice", &a, "1\n", None).await;
    branch_at(&h, "feature", "main").await;
    put_on(&h, "feature", "bob", &path("Source/b.cpp"), "b\n", None).await;
    let seen = put_on(&h, "main", "alice", &a, "2\n", Some(1)).await.commit;
    put_on(&h, "main", "alice", &a, "3\n", Some(2)).await;
    let mut asked = request("feature", "main");
    asked.expect_target = Some(seen);
    let err = h.svc.merge(&who("alice"), asked).await.unwrap_err();
    assert!(matches!(err, PynError::MergeStale(_)), "{err}");
}

pub async fn a_merge_needs_the_checkin_permission_and_an_existing_source(store: Store) {
    let h = harness(store);
    put_on(&h, "main", "alice", &path("Source/a.cpp"), "1\n", None).await;
    let reader = Principal {
        user: user("rae"),
        permissions: [Permission::Read].into(),
        email: None,
    };
    let err = h
        .svc
        .merge(&reader, request("main", "main"))
        .await
        .unwrap_err();
    assert!(
        matches!(err, PynError::Forbidden(Permission::Checkin)),
        "{err}"
    );
    let err = h
        .svc
        .merge(&who("alice"), request("nope", "main"))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::CommitNotFound(_)), "{err}");
    let err = h
        .svc
        .merge(&who("alice"), request("main", "nope"))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::BranchNotFound(_)), "{err}");
}

pub async fn a_policy_file_arriving_by_merge_is_policy_on_the_default_branch(store: Store) {
    let h = harness(store);
    let (a, policy) = (path("Source/a.cpp"), path(".pyn/pyn.toml"));
    put_on(&h, "main", "alice", &a, "one\n", None).await;
    put_on(
        &h,
        "main",
        "alice",
        &policy,
        "[meta]\ndefault = \"shared\"\n",
        None,
    )
    .await;
    h.svc.load_policy().await.unwrap();
    branch_at(&h, "feature", "main").await;
    h.svc
        .checkout(&policy, &user("alice"), Some(RevisionId(1)))
        .await
        .unwrap();
    let wide = blob(&h, "[meta]\ndefault = \"exclusive\"\n").await;
    h.svc
        .checkin_on(
            "feature",
            &who("alice"),
            &policy,
            wide,
            FileMode::Regular,
            Some(RevisionId(1)),
            "all exclusive".into(),
        )
        .await
        .unwrap();
    put_on(&h, "main", "alice", &path("Source/b.cpp"), "b\n", None).await;
    assert_eq!(h.svc.mode_for(&a), Mode::Shared);

    let writer = Principal {
        user: user("wes"),
        permissions: [Permission::Read, Permission::Checkin].into(),
        email: None,
    };
    let err = h
        .svc
        .merge(&writer, request("feature", "main"))
        .await
        .unwrap_err();
    assert!(
        matches!(err, PynError::Forbidden(Permission::EditPolicy)),
        "{err}"
    );
    assert_eq!(h.svc.mode_for(&a), Mode::Shared);

    h.svc
        .merge(&who("alice"), request("feature", "main"))
        .await
        .unwrap();
    assert_eq!(h.svc.mode_for(&a), Mode::Exclusive);
    let line = h.svc.history(&a).await.unwrap();
    assert_eq!(
        line.last().unwrap().mode,
        Some(Mode::Exclusive),
        "converted by the merged policy"
    );

    let into_feature = request("main", "feature");
    assert_eq!(
        h.svc.merge_preview(&into_feature).await.unwrap().outcome,
        MergeOutcome::FastForward
    );
}

pub async fn merges_racing_into_one_branch_each_land_or_are_told_to_retry(store: Store) {
    let h = harness(store);
    put_on(
        &h,
        "main",
        "alice",
        &path("Source/base.cpp"),
        "base\n",
        None,
    )
    .await;
    let names: Vec<String> = (0..6).map(|i| format!("b{i}")).collect();
    for name in &names {
        branch_at(&h, name, "main").await;
        put_on(
            &h,
            name,
            "alice",
            &path(&format!("Source/{name}.cpp")),
            name,
            None,
        )
        .await;
    }
    put_on(
        &h,
        "main",
        "alice",
        &path("Source/base.cpp"),
        "moved\n",
        Some(1),
    )
    .await;
    let tasks: Vec<_> = names
        .iter()
        .map(|name| {
            let (svc, name) = (h.svc.clone(), name.clone());
            tokio::spawn(async move {
                loop {
                    match svc.merge(&who("alice"), request(&name, "main")).await {
                        Err(PynError::MergeStale(_)) => tokio::task::yield_now().await,
                        other => return other.map(|r| r.outcome),
                    }
                }
            })
        })
        .collect();
    for t in tasks {
        assert_eq!(t.await.unwrap().unwrap(), MergeOutcome::Merge);
    }
    let files: Vec<_> = revision_numbers(&h, "main")
        .await
        .into_iter()
        .map(|(p, _)| p)
        .collect();
    assert_eq!(files.len(), 7, "{files:?}");
    verified(&h).await;
}

pub async fn a_file_on_one_side_and_a_folder_on_the_other_cannot_merge(store: Store) {
    let h = harness(store);
    put_on(&h, "main", "alice", &path("Source/keep.cpp"), "k\n", None).await;
    branch_at(&h, "feature", "main").await;
    put_on(&h, "feature", "alice", &path("Source/x"), "file\n", None).await;
    put_on(&h, "main", "alice", &path("Source/x/y.cpp"), "y\n", None).await;
    let err = h
        .svc
        .merge_preview(&request("feature", "main"))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, PynError::InvalidRequest(m) if m.contains("Source/x")),
        "{err}"
    );
    let err = h
        .svc
        .merge(&who("alice"), request("feature", "main"))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::InvalidRequest(_)), "{err}");
}
