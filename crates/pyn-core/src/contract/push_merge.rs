//! Merge commits in a push (issue #338): the push reproduces the merge semantics of the server's own merge, with
//! shared content taken from the pushed tree.

use super::push::{Built, Files, build, build_merge, objects, set};
use super::{Harness, Store, blob, harness, path, put_on, user, who};
use crate::{FileMode, Mode, ObjectId, Permission, Principal, PynError, RevisionId};

const POLICY: &str = ".pyn/pyn.toml";

async fn push(h: &Harness, who_: &str, parts: &[&Built]) -> Result<crate::Pushed, PynError> {
    h.svc
        .push_commits_recorded(&who(who_), "main", objects(parts))
        .await
}

async fn head(h: &Harness) -> Option<ObjectId> {
    h.svc.head_of("main").await.unwrap()
}

async fn pointers(h: &Harness, commit: ObjectId) -> Vec<(String, u64)> {
    h.store
        .commit_pointers(h.svc.repo(), &commit)
        .await
        .unwrap()
        .into_iter()
        .map(|r| (r.path.to_string(), r.id.0))
        .collect()
}

async fn revision_ids(h: &Harness) -> Vec<(String, u64)> {
    h.svc
        .snapshot_on("main")
        .await
        .unwrap()
        .entries
        .into_iter()
        .map(|e| (e.path.to_string(), e.revision.id.0))
        .collect()
}

async fn verified(h: &Harness) {
    let report = h.svc.verify(None, true).await.unwrap();
    assert!(report.problems.is_empty(), "{:?}", report.problems);
}

/// Source/a.cpp and Source/b.cpp (text) and the exclusive Content/e.umap, pushed as the first commit.
async fn seeded(h: &Harness) -> (Files, Built) {
    let text = blob(h, "1\n2\n3\n4\n5\n6\n").await;
    let e = blob(h, "e0").await;
    let mut files = Files::new();
    set(&mut files, "Source/a.cpp", text);
    set(&mut files, "Source/b.cpp", text);
    set(&mut files, "Content/e.umap", e);
    let base = build(&files, None, "base", 1);
    h.svc
        .checkout(&path("Content/e.umap"), &user("alice"), None)
        .await
        .unwrap();
    push(h, "alice", &[&base]).await.unwrap();
    (files, base)
}

/// The server moved on: bob changed b.cpp and the exclusive e under his lock.
async fn server_moved(h: &Harness, files: &Files, base: &Built) -> (Files, Built) {
    let (b1, e1) = (blob(h, "b changed\n").await, blob(h, "e1").await);
    let mut theirs = files.clone();
    set(&mut theirs, "Source/b.cpp", b1);
    set(&mut theirs, "Content/e.umap", e1);
    let s1 = build(&theirs, Some(base.id), "server side", 2);
    h.svc
        .checkout(&path("Content/e.umap"), &user("bob"), Some(RevisionId(1)))
        .await
        .unwrap();
    push(h, "bob", &[&s1]).await.unwrap();
    (theirs, s1)
}

pub async fn a_pushed_merge_adopts_the_other_sides_pointers_without_a_lock(store: Store) {
    let h = harness(store);
    let (files, base) = seeded(&h).await;
    let (theirs, s1) = server_moved(&h, &files, &base).await;
    let (a1, b1) = (blob(&h, "a changed\n").await, theirs["Source/b.cpp"].1);
    let mut ours = files.clone();
    set(&mut ours, "Source/a.cpp", a1);
    let local = build(&ours, Some(base.id), "local", 3);
    let mut merged = theirs.clone();
    set(&mut merged, "Source/a.cpp", a1);
    set(&mut merged, "Source/b.cpp", b1);
    let merge = build_merge(&merged, vec![local.id, s1.id], "Merge", 4);

    let pushed = push(&h, "alice", &[&local, &merge]).await.unwrap();
    assert_eq!(head(&h).await, Some(merge.id));
    assert_eq!(pushed.revisions.len(), 1, "only the local commit made one");
    assert_eq!(pushed.revisions[0].path.as_str(), "Source/a.cpp");
    assert_eq!(pushed.revisions[0].base, Some(RevisionId(1)));
    let stored = h.svc.get_commit(&merge.id).await.unwrap();
    assert_eq!(stored.parents, vec![local.id, s1.id]);
    assert_eq!(stored.raw, merge.raw);
    assert!(stored.revisions.is_empty());
    assert_eq!(
        pointers(&h, merge.id).await,
        [
            ("Content/e.umap".to_string(), 2),
            ("Source/b.cpp".to_string(), 2)
        ]
    );
    assert_eq!(
        revision_ids(&h).await,
        [
            ("Content/e.umap".to_string(), 2),
            ("Source/a.cpp".to_string(), 2),
            ("Source/b.cpp".to_string(), 2)
        ]
    );
    assert!(h.svc.locks().await.unwrap().is_empty());
    verified(&h).await;
    let report = h.svc.verify(None, false).await.unwrap();
    assert_eq!(report.commits, 4);
}

pub async fn a_pushed_merge_of_text_changed_in_different_places_is_a_new_revision(store: Store) {
    let h = harness(store);
    let (files, base) = seeded(&h).await;
    let (theirs, s1) = server_moved(&h, &files, &base).await;
    let (a_theirs, a_ours) = (
        blob(&h, "one\n2\n3\n4\n5\n6\n").await,
        blob(&h, "1\n2\n3\n4\n5\nsix\n").await,
    );
    // The server side also changed a.cpp, on its first line.
    let mut theirs2 = theirs.clone();
    set(&mut theirs2, "Source/a.cpp", a_theirs);
    let s2 = build(&theirs2, Some(s1.id), "server a", 5);
    push(&h, "bob", &[&s2]).await.unwrap();

    let mut ours = files.clone();
    set(&mut ours, "Source/a.cpp", a_ours);
    let local = build(&ours, Some(base.id), "local", 3);
    let both = blob(&h, "one\n2\n3\n4\n5\nsix\n").await;
    let mut merged = theirs2.clone();
    set(&mut merged, "Source/a.cpp", both);
    let merge = build_merge(&merged, vec![local.id, s2.id], "Merge", 6);

    let pushed = push(&h, "alice", &[&local, &merge]).await.unwrap();
    let a: Vec<_> = pushed
        .revisions
        .iter()
        .map(|r| (r.path.as_str(), r.id.0, r.base.map(|b| b.0), r.content))
        .collect();
    assert_eq!(
        a,
        [
            ("Source/a.cpp", 3, Some(1), a_ours),
            ("Source/a.cpp", 4, Some(3), both)
        ]
    );
    let (_, bytes) = h
        .svc
        .read_on("main", &path("Source/a.cpp"), None)
        .await
        .unwrap();
    assert_eq!(bytes, b"one\n2\n3\n4\n5\nsix\n");
    assert_eq!(
        pointers(&h, merge.id).await[0],
        ("Content/e.umap".to_string(), 2)
    );
    verified(&h).await;
}

pub async fn a_pushed_merge_with_diverged_exclusive_pointers_needs_a_resolution_under_the_lock(
    store: Store,
) {
    let h = harness(store);
    let e = path("Content/e.umap");
    let (files, base) = seeded(&h).await;
    let (theirs, s1) = server_moved(&h, &files, &base).await;
    let mine = blob(&h, "e mine").await;
    let mut ours = files.clone();
    set(&mut ours, "Content/e.umap", mine);
    let local = build(&ours, Some(base.id), "local e", 3);

    let chosen = blob(&h, "e chosen").await;
    let mut resolved = theirs.clone();
    set(&mut resolved, "Content/e.umap", chosen);
    let merge = build_merge(&resolved, vec![local.id, s1.id], "resolve e", 5);

    // No live lease: refused whole.
    let err = push(&h, "alice", &[&local, &merge]).await.unwrap_err();
    assert!(matches!(err, PynError::LockRequired(_)), "{err}");
    assert_eq!(head(&h).await, Some(s1.id));
    assert!(h.svc.get_commit(&local.id).await.is_err());

    // Someone else's lease.
    h.svc
        .checkout(&e, &user("bob"), Some(RevisionId(2)))
        .await
        .unwrap();
    let err = push(&h, "alice", &[&local, &merge]).await.unwrap_err();
    assert!(matches!(err, PynError::LockHeld { .. }), "{err}");
    h.svc.release(&e, &user("bob")).await.unwrap();

    // A lapsed lease.
    h.svc
        .checkout(&e, &user("alice"), Some(RevisionId(2)))
        .await
        .unwrap();
    h.clock.advance(chrono::Duration::hours(9));
    let err = push(&h, "alice", &[&local, &merge]).await.unwrap_err();
    assert!(matches!(err, PynError::LockRequired(_)), "{err}");
    h.svc
        .checkout(&e, &user("alice"), Some(RevisionId(2)))
        .await
        .unwrap();

    let pushed = push(&h, "alice", &[&local, &merge]).await.unwrap();
    let e_revs: Vec<_> = pushed
        .revisions
        .iter()
        .map(|r| (r.id.0, r.base.map(|b| b.0), r.content))
        .collect();
    assert_eq!(e_revs, [(3, Some(1), mine), (4, Some(3), chosen)]);
    assert_eq!(head(&h).await, Some(merge.id));
    assert!(
        h.svc.locks().await.unwrap().is_empty(),
        "the lease is spent"
    );
    verified(&h).await;
}

/// main took bob's e (r2) and feature took alice's (r3), both based on r1: the pointers diverged.
async fn diverged(h: &Harness) -> (Files, Built, Files, ObjectId) {
    let (files, base) = seeded(h).await;
    let (theirs, s1) = server_moved(h, &files, &base).await;
    let e = path("Content/e.umap");
    h.svc
        .create_branch(&who("alice"), "feature", Some(&base.id.to_hex()))
        .await
        .unwrap();
    h.svc
        .checkout_on("feature", &e, &user("alice"), Some(RevisionId(1)))
        .await
        .unwrap();
    let on_feature = put_on(h, "feature", "alice", &e, "e feature", Some(1)).await;
    assert_eq!(on_feature.id, RevisionId(3));
    let feature = h.svc.head_of("feature").await.unwrap().unwrap();
    (theirs, s1, files, feature)
}

pub async fn a_diverged_exclusive_path_is_a_conflict_unless_the_pusher_holds_its_lock(
    store: Store,
) {
    let h = harness(store);
    let e = path("Content/e.umap");
    let (main_files, s1, _, feature) = diverged(&h).await;
    let (ours_content, feature_content) =
        (main_files["Content/e.umap"].1, blob(&h, "e feature").await);

    // Keeping main's content with no lock is no resolution: the path is named.
    let keep = build_merge(&main_files, vec![s1.id, feature], "keep main's", 4);
    let err = push(&h, "alice", &[&keep]).await.unwrap_err();
    assert!(
        matches!(err, PynError::MergeConflict(ref p) if p == std::slice::from_ref(&e)),
        "{err}"
    );
    assert_eq!(err.code(), "merge_conflict");

    // Taking the feature's content is a new revision, and that needs the lock.
    let mut taken = main_files.clone();
    set(&mut taken, "Content/e.umap", feature_content);
    let take = build_merge(&taken, vec![s1.id, feature], "take feature's", 5);
    let err = push(&h, "alice", &[&take]).await.unwrap_err();
    assert!(matches!(err, PynError::LockRequired(_)), "{err}");
    assert_eq!(head(&h).await, Some(s1.id));

    // With the lock, keeping main's content records the choice as a revision that follows both sides.
    h.svc
        .checkout(&e, &user("alice"), Some(RevisionId(2)))
        .await
        .unwrap();
    let pushed = push(&h, "alice", &[&keep]).await.unwrap();
    let made = &pushed.revisions[0];
    assert_eq!(
        (made.id.0, made.base, made.content),
        (4, Some(RevisionId(2)), ours_content)
    );
    assert_eq!(head(&h).await, Some(keep.id));
    assert!(h.svc.locks().await.unwrap().is_empty());
    assert_eq!(
        pointers(&h, keep.id).await,
        [("Content/e.umap".to_string(), 4)]
    );
    verified(&h).await;
}

pub async fn the_other_branch_adopts_a_resolution_by_a_pushed_merge_without_a_lock(store: Store) {
    let h = harness(store);
    let e = path("Content/e.umap");
    let (main_files, s1, _, feature) = diverged(&h).await;
    h.svc
        .checkout(&e, &user("alice"), Some(RevisionId(2)))
        .await
        .unwrap();
    let chosen = blob(&h, "e chosen").await;
    let mut resolved = main_files.clone();
    set(&mut resolved, "Content/e.umap", chosen);
    let merge = build_merge(&resolved, vec![s1.id, feature], "resolve", 4);
    push(&h, "alice", &[&merge]).await.unwrap();

    // The resolution follows both sides, so feature takes it as the descendant of its own r3.
    let mut on_feature = main_files.clone();
    set(&mut on_feature, "Content/e.umap", chosen);
    let back = build_merge(&on_feature, vec![feature, merge.id], "main into feature", 5);
    let pushed = h
        .svc
        .push_commits_recorded(&who("bob"), "feature", objects(&[&back]))
        .await
        .unwrap();
    assert!(pushed.revisions.is_empty());
    assert!(
        pointers(&h, back.id)
            .await
            .contains(&("Content/e.umap".to_string(), 4))
    );
    assert_eq!(
        h.svc.revision_on("feature", &e).await.unwrap().unwrap().id,
        RevisionId(4)
    );
    verified(&h).await;
}

pub async fn a_push_with_a_merge_must_still_contain_the_branch_head(store: Store) {
    let h = harness(store);
    let (files, base) = seeded(&h).await;
    let (_, s1) = server_moved(&h, &files, &base).await;
    let a1 = blob(&h, "a1\n").await;
    let mut ours = files.clone();
    set(&mut ours, "Source/a.cpp", a1);
    let local = build(&ours, Some(base.id), "local", 3);
    // Alone, a commit on an older parent is not a fast-forward.
    let err = push(&h, "alice", &[&local]).await.unwrap_err();
    assert!(matches!(err, PynError::NotFastForward(_)), "{err}");

    // Merging in the seed again does not bring the head along.
    let merge = build_merge(&ours, vec![local.id, base.id], "Merge", 4);
    let err = push(&h, "alice", &[&local, &merge]).await.unwrap_err();
    assert!(matches!(err, PynError::NotFastForward(_)), "{err}");
    assert_eq!(head(&h).await, Some(s1.id));
    assert!(h.svc.get_commit(&local.id).await.is_err());
}

pub async fn a_pushed_merge_names_commits_the_server_has(store: Store) {
    let h = harness(store);
    let (files, base) = seeded(&h).await;
    let (theirs, s1) = server_moved(&h, &files, &base).await;
    let a1 = blob(&h, "a1\n").await;
    let mut ours = files.clone();
    set(&mut ours, "Source/a.cpp", a1);
    let local = build(&ours, Some(base.id), "local", 3);
    let mut merged = theirs.clone();
    set(&mut merged, "Source/a.cpp", a1);

    let unknown = build_merge(&files, vec![s1.id], "elsewhere", 9);
    let err = push(
        &h,
        "alice",
        &[
            &local,
            &build_merge(&merged, vec![local.id, unknown.id], "m", 4),
        ],
    )
    .await
    .unwrap_err();
    assert!(matches!(err, PynError::PushInvalid(_)), "{err}");

    let merge = build_merge(&merged, vec![local.id, s1.id], "Merge", 4);
    let bare = Principal {
        user: user("wes"),
        permissions: [Permission::Read].into(),
        email: None,
    };
    let err = h
        .svc
        .push_commits_recorded(&bare, "main", objects(&[&local, &merge]))
        .await
        .unwrap_err();
    assert!(
        matches!(err, PynError::Forbidden(Permission::Checkin)),
        "{err}"
    );
    assert_eq!(head(&h).await, Some(s1.id));
}

pub async fn a_pushed_merge_can_delete_a_path_and_is_one_commit_with_two_parents(store: Store) {
    let h = harness(store);
    let (files, base) = seeded(&h).await;
    let (theirs, s1) = server_moved(&h, &files, &base).await;
    let a1 = blob(&h, "a1\n").await;
    let mut ours = files.clone();
    set(&mut ours, "Source/a.cpp", a1);
    let local = build(&ours, Some(base.id), "local", 3);
    let mut merged = theirs.clone();
    set(&mut merged, "Source/a.cpp", a1);
    merged.remove("Source/b.cpp");
    let merge = build_merge(&merged, vec![local.id, s1.id], "Merge and drop b", 4);
    let pushed = push(&h, "alice", &[&local, &merge]).await.unwrap();
    let last = pushed.revisions.last().unwrap();
    assert_eq!(
        (last.path.as_str(), last.deleted, last.id.0),
        ("Source/b.cpp", true, 3)
    );
    assert_eq!(
        last.base,
        Some(RevisionId(1)),
        "based on the target's pointer"
    );
    let live: Vec<_> = h
        .svc
        .snapshot_on("main")
        .await
        .unwrap()
        .entries
        .into_iter()
        .map(|e| e.path.to_string())
        .collect();
    assert_eq!(live, ["Content/e.umap", "Source/a.cpp"]);
    verified(&h).await;
}

pub async fn a_policy_file_a_pushed_merge_adopts_applies_on_the_default_branch(store: Store) {
    let h = harness(store);
    let (a, policy) = (path("Source/a.cpp"), path(POLICY));
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
    h.svc
        .create_branch(&who("alice"), "feature", Some("main"))
        .await
        .unwrap();
    h.svc
        .checkout(&policy, &user("alice"), Some(RevisionId(1)))
        .await
        .unwrap();
    let wide = blob(&h, "[meta]\ndefault = \"exclusive\"\n").await;
    let on_feature = h
        .svc
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

    let main_head = head(&h).await.unwrap();
    let feature_head = h.svc.head_of("feature").await.unwrap().unwrap();
    let mut files = Files::new();
    for e in h.svc.snapshot_on("main").await.unwrap().entries {
        files.insert(
            e.path.to_string(),
            (e.revision.file_mode, e.revision.content),
        );
    }
    files.insert(POLICY.to_string(), (FileMode::Regular, on_feature.content));
    let merge = build_merge(&files, vec![main_head, feature_head], "Merge feature", 9);

    let writer = Principal {
        user: user("wes"),
        permissions: [Permission::Read, Permission::Checkin].into(),
        email: None,
    };
    let err = h
        .svc
        .push_commits_recorded(&writer, "main", objects(&[&merge]))
        .await
        .unwrap_err();
    assert!(
        matches!(err, PynError::Forbidden(Permission::EditPolicy)),
        "{err}"
    );
    assert_eq!(head(&h).await, Some(main_head));
    assert_eq!(h.svc.mode_for(&a), Mode::Shared);

    push(&h, "alice", &[&merge]).await.unwrap();
    // The policy's conversion of a.cpp is a commit after the merge.
    assert_eq!(
        h.svc.get_commit(&merge.id).await.unwrap().parents,
        vec![main_head, feature_head]
    );
    assert_eq!(h.svc.mode_for(&a), Mode::Exclusive);
    verified(&h).await;
}
