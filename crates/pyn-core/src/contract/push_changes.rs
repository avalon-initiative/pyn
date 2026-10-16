//! Deletions and policy-file commits in a push (issue #339), through `RepoService::push_commits_recorded`: a deletion
//! is recorded as in a changeset, and a policy commit on the default branch is validated like a direct one.

use std::collections::BTreeSet;

use super::push::{Files, build, objects, set};
use super::{Harness, Store, blob, harness, path, put_on, user, who};
use crate::audit::{AuditAction, AuditQuery};
use crate::{Mode, ObjectId, Permission, Principal, PynError, RevisionId};

const POLICY: &str = ".pyn/pyn.toml";
const A_EXCLUSIVE: &str = "[meta]\ndefault = \"shared\"\n[exclusive]\npaths = [\"Source/a.cpp\"]\n";

async fn push(
    h: &Harness,
    who_: &Principal,
    branch: &str,
    parts: &[&super::push::Built],
) -> Result<crate::Pushed, PynError> {
    h.svc
        .push_commits_recorded(who_, branch, objects(parts))
        .await
}

async fn head(h: &Harness, branch: &str) -> Option<ObjectId> {
    h.svc.head_of(branch).await.unwrap()
}

async fn live_paths(h: &Harness, branch: &str) -> Vec<String> {
    h.svc
        .snapshot_on(branch)
        .await
        .unwrap()
        .entries
        .into_iter()
        .map(|e| e.path.to_string())
        .collect()
}

/// `Source/a.cpp`, `Source/b.cpp` and `Source/sub/c.cpp` pushed as the first commit.
async fn seeded(h: &Harness) -> (Files, super::push::Built) {
    let (x, y, z) = (blob(h, "x").await, blob(h, "y").await, blob(h, "z").await);
    let mut files = Files::new();
    set(&mut files, "Source/a.cpp", x);
    set(&mut files, "Source/b.cpp", y);
    set(&mut files, "Source/sub/c.cpp", z);
    let base = build(&files, None, "base", 1);
    push(h, &who("alice"), "main", &[&base]).await.unwrap();
    (files, base)
}

pub async fn a_push_deletes_shared_paths_and_whole_folders(store: Store) {
    let h = harness(store);
    let (mut files, base) = seeded(&h).await;

    files.remove("Source/b.cpp");
    files.remove("Source/sub/c.cpp");
    let gone = build(&files, Some(base.id), "remove b and sub", 2);
    let pushed = push(&h, &who("alice"), "main", &[&gone]).await.unwrap();

    let removed: Vec<_> = pushed
        .revisions
        .iter()
        .map(|r| (r.path.as_str(), r.deleted, r.id, r.base))
        .collect();
    assert_eq!(
        removed,
        [
            ("Source/b.cpp", true, RevisionId(2), Some(RevisionId(1))),
            ("Source/sub/c.cpp", true, RevisionId(2), Some(RevisionId(1))),
        ]
    );
    assert!(pushed.stranded.is_empty());
    for r in &pushed.revisions {
        r.verify().unwrap();
        assert_eq!(r.hash_version, 3);
        assert_eq!(r.message, "remove b and sub");
        assert_eq!(r.author, user("alice"));
        assert_eq!(r.mode, Some(Mode::Shared));
    }
    assert!(pushed.revisions[0].changeset.is_some());
    assert_eq!(head(&h, "main").await, Some(gone.id));
    assert_eq!(live_paths(&h, "main").await, ["Source/a.cpp"]);
    let kept = h.svc.history(&path("Source/b.cpp")).await.unwrap();
    assert_eq!(kept.len(), 2, "history keeps the deleted path");
    assert_eq!(kept[1].content, kept[0].content);

    let stored = h.svc.get_commit(&gone.id).await.unwrap();
    assert_eq!(stored.raw, gone.raw);
    assert_eq!(stored.revisions.len(), 2);
    let audited = h
        .svc
        .audit(&AuditQuery {
            action: Some(AuditAction::Delete),
            limit: 10,
            ..AuditQuery::default()
        })
        .await
        .unwrap();
    assert_eq!(audited.len(), 2);
    assert!(audited.iter().all(|e| e.detail.contains("(push)")));
    let report = h.svc.verify(None, true).await.unwrap();
    assert!(report.problems.is_empty(), "{report:?}");

    // The path can be added again; its base is the deletion.
    let again = blob(&h, "back").await;
    set(&mut files, "Source/b.cpp", again);
    let back = build(&files, Some(gone.id), "b again", 3);
    let pushed = push(&h, &who("alice"), "main", &[&back]).await.unwrap();
    assert_eq!(
        (pushed.revisions[0].id, pushed.revisions[0].base),
        (RevisionId(3), Some(RevisionId(2)))
    );
    assert!(!pushed.revisions[0].deleted);
}

pub async fn a_file_turned_into_a_folder_or_back_deletes_and_adds(store: Store) {
    let h = harness(store);
    let (mut files, base) = seeded(&h).await;
    let n = blob(&h, "nested").await;

    files.remove("Source/b.cpp");
    set(&mut files, "Source/b.cpp/inner.cpp", n);
    let folder = build(&files, Some(base.id), "b becomes a folder", 2);
    let pushed = push(&h, &who("alice"), "main", &[&folder]).await.unwrap();
    let changed: Vec<_> = pushed
        .revisions
        .iter()
        .map(|r| (r.path.as_str(), r.deleted))
        .collect();
    assert_eq!(
        changed,
        [("Source/b.cpp", true), ("Source/b.cpp/inner.cpp", false)]
    );
    assert_eq!(
        live_paths(&h, "main").await,
        ["Source/a.cpp", "Source/b.cpp/inner.cpp", "Source/sub/c.cpp"]
    );

    files.remove("Source/b.cpp/inner.cpp");
    set(&mut files, "Source/b.cpp", n);
    let file = build(&files, Some(folder.id), "b is a file again", 3);
    let pushed = push(&h, &who("alice"), "main", &[&file]).await.unwrap();
    let changed: Vec<_> = pushed
        .revisions
        .iter()
        .map(|r| (r.path.as_str(), r.deleted))
        .collect();
    assert_eq!(
        changed,
        [("Source/b.cpp", false), ("Source/b.cpp/inner.cpp", true)]
    );
    let report = h.svc.verify(None, true).await.unwrap();
    assert!(report.problems.is_empty(), "{report:?}");
}

pub async fn deleting_an_exclusive_path_in_a_push_needs_the_pushers_live_lock(store: Store) {
    let h = harness(store);
    let map = path("Content/a.umap");
    let (m, s) = (blob(&h, "map").await, blob(&h, "src").await);
    h.svc.checkout(&map, &user("alice"), None).await.unwrap();
    let mut files = Files::new();
    set(&mut files, "Content/a.umap", m);
    set(&mut files, "Source/a.cpp", s);
    let base = build(&files, None, "base", 1);
    push(&h, &who("alice"), "main", &[&base]).await.unwrap();
    assert!(h.svc.locks().await.unwrap().is_empty());

    files.remove("Content/a.umap");
    let gone = build(&files, Some(base.id), "delete the map", 2);
    let err = push(&h, &who("alice"), "main", &[&gone]).await.unwrap_err();
    assert!(matches!(err, PynError::LockRequired(_)), "{err}");

    h.svc
        .checkout(&map, &user("bob"), Some(RevisionId(1)))
        .await
        .unwrap();
    let err = push(&h, &who("alice"), "main", &[&gone]).await.unwrap_err();
    assert!(
        matches!(err, PynError::LockHeld { ref owner, .. } if owner == &user("bob")),
        "{err}"
    );
    assert_eq!(h.svc.locks().await.unwrap().len(), 1, "bob keeps his lock");
    assert_eq!(head(&h, "main").await, Some(base.id));
    assert_eq!(h.svc.history(&map).await.unwrap().len(), 1);

    h.svc.release(&map, &user("bob")).await.unwrap();
    h.svc
        .checkout(&map, &user("alice"), Some(RevisionId(1)))
        .await
        .unwrap();
    h.clock.advance(chrono::Duration::hours(9));
    let err = push(&h, &who("alice"), "main", &[&gone]).await.unwrap_err();
    assert!(
        matches!(err, PynError::LockRequired(_)),
        "a lapsed lease is refused: {err}"
    );

    h.svc
        .checkout(&map, &user("alice"), Some(RevisionId(1)))
        .await
        .unwrap();
    let pushed = push(&h, &who("alice"), "main", &[&gone]).await.unwrap();
    let rev = &pushed.revisions[0];
    assert!(rev.deleted && rev.mode == Some(Mode::Exclusive));
    assert_eq!((rev.id, rev.base), (RevisionId(2), Some(RevisionId(1))));
    assert!(
        h.svc.locks().await.unwrap().is_empty(),
        "the lock is used up"
    );
    assert_eq!(live_paths(&h, "main").await, ["Source/a.cpp"]);
}

pub async fn a_policy_commit_on_the_default_branch_is_validated_and_applied(store: Store) {
    let h = harness(store);
    let (mut files, base) = seeded(&h).await;
    let a = path("Source/a.cpp");
    assert_eq!(h.svc.mode_for(&a), Mode::Shared);

    let policy = blob(&h, A_EXCLUSIVE).await;
    set(&mut files, POLICY, policy);
    let commit = build(&files, Some(base.id), "policy", 2);
    let pushed = push(&h, &who("alice"), "main", &[&commit]).await.unwrap();

    assert_eq!(pushed.revisions.len(), 1);
    assert_eq!(pushed.revisions[0].path.as_str(), POLICY);
    h.svc.get_commit(&commit.id).await.unwrap();
    assert_eq!(h.svc.mode_for(&a), Mode::Exclusive, "the rules now apply");
    let conv = h.svc.history(&a).await.unwrap();
    assert_eq!(conv.len(), 2, "the path converted to exclusive");
    assert_eq!(conv[1].mode, Some(Mode::Exclusive));

    let told = h
        .svc
        .audit(&AuditQuery {
            action: Some(AuditAction::PolicyChanged),
            limit: 10,
            ..AuditQuery::default()
        })
        .await
        .unwrap();
    assert_eq!(told.len(), 1);
    let report = h.svc.verify(None, true).await.unwrap();
    assert!(report.problems.is_empty(), "{report:?}");

    // Later edits go through the lock the new policy asks for.
    let next = blob(&h, "next").await;
    set(&mut files, "Source/a.cpp", next);
    let tip = head(&h, "main").await;
    let after = build(&files, tip, "edit a", 3);
    let err = push(&h, &who("alice"), "main", &[&after])
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::LockRequired(_)), "{err}");

    // Only the first policy file is the owner's bootstrap; the next needs the lock like any exclusive path.
    set(
        &mut files,
        POLICY,
        blob(&h, "[meta]\ndefault = \"shared\"\n").await,
    );
    let again = build(&files, tip, "policy again", 4);
    let err = push(&h, &who("alice"), "main", &[&again])
        .await
        .unwrap_err();
    assert!(
        matches!(err, PynError::LockRequired(ref p) if p.as_str() == POLICY),
        "{err}"
    );
}

pub async fn a_policy_commit_changes_the_rules_for_the_commits_after_it(store: Store) {
    let h = harness(store);
    let (mut files, base) = seeded(&h).await;
    let policy = blob(
        &h,
        "[meta]\ndefault = \"shared\"\n[exclusive]\npaths = [\"Tools/\"]\n",
    )
    .await;
    let tool = blob(&h, "tool").await;
    set(&mut files, POLICY, policy);
    let first = build(&files, Some(base.id), "policy", 2);
    set(&mut files, "Tools/run.sh", tool);
    let second = build(&files, Some(first.id), "tool", 3);

    let err = push(&h, &who("alice"), "main", &[&first, &second])
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::LockRequired(_)), "{err}");
    assert_eq!(head(&h, "main").await, Some(base.id));
    assert_eq!(h.svc.mode_for(&path("Tools/run.sh")), Mode::Shared);
    assert!(h.svc.head(&path(POLICY)).await.unwrap().is_none());
}

pub async fn an_invalid_policy_refuses_the_whole_push(store: Store) {
    let h = harness(store);
    let (mut files, base) = seeded(&h).await;
    let extra = blob(&h, "extra").await;
    set(&mut files, "Source/extra.cpp", extra);
    let first = build(&files, Some(base.id), "fine", 2);
    let bad = blob(&h, "this is not [valid toml").await;
    set(&mut files, POLICY, bad);
    let second = build(&files, Some(first.id), "bad policy", 3);

    let err = push(&h, &who("alice"), "main", &[&first, &second])
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::InvalidRules(_)), "{err}");
    assert_eq!(err.code(), "invalid_rules");
    assert_eq!(head(&h, "main").await, Some(base.id));
    for id in [first.id, second.id] {
        let err = h.svc.get_commit(&id).await.unwrap_err();
        assert!(matches!(err, PynError::CommitNotFound(_)), "{err}");
    }
    assert!(
        h.svc
            .head(&path("Source/extra.cpp"))
            .await
            .unwrap()
            .is_none()
    );
    assert!(h.svc.head(&path(POLICY)).await.unwrap().is_none());
    assert_eq!(h.svc.mode_for(&path("Content/a.umap")), Mode::Exclusive);
    let report = h.svc.verify(None, false).await.unwrap();
    assert!(report.problems.is_empty(), "{report:?}");

    // The same push with a valid policy lands.
    let good = blob(&h, A_EXCLUSIVE).await;
    set(&mut files, POLICY, good);
    let second = build(&files, Some(first.id), "good policy", 3);
    push(&h, &who("alice"), "main", &[&first, &second])
        .await
        .unwrap();
    h.svc.get_commit(&second.id).await.unwrap();
}

pub async fn changing_the_policy_in_a_push_needs_the_policy_permission(store: Store) {
    let h = harness(store);
    let (mut files, base) = seeded(&h).await;
    let policy = blob(&h, A_EXCLUSIVE).await;
    set(&mut files, POLICY, policy);
    let commit = build(&files, Some(base.id), "policy", 2);
    let writer = Principal {
        user: user("wanda"),
        permissions: BTreeSet::from([Permission::Read, Permission::Checkin]),
        email: None,
    };
    let err = push(&h, &writer, "main", &[&commit]).await.unwrap_err();
    assert!(
        matches!(err, PynError::Forbidden(Permission::EditPolicy)),
        "{err}"
    );
    assert_eq!(head(&h, "main").await, Some(base.id));
}

pub async fn the_policy_file_cannot_be_deleted_by_a_push_to_the_default_branch(store: Store) {
    let h = harness(store);
    let (mut files, base) = seeded(&h).await;
    let policy = blob(&h, A_EXCLUSIVE).await;
    set(&mut files, POLICY, policy);
    let with = build(&files, Some(base.id), "policy", 2);
    push(&h, &who("alice"), "main", &[&with]).await.unwrap();

    files.remove(POLICY);
    let without = build(&files, Some(with.id), "drop the policy", 3);
    let err = push(&h, &who("alice"), "main", &[&without])
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::PushUnsupported(_)), "{err}");
    assert_eq!(err.code(), "push_unsupported");
    assert!(h.svc.head(&path(POLICY)).await.unwrap().is_some());
    assert_eq!(h.svc.mode_for(&path("Source/a.cpp")), Mode::Exclusive);
}

pub async fn a_push_reports_the_versions_its_policy_strands(store: Store) {
    let h = harness(store);
    let a = path("Source/a.cpp");
    let edit = {
        put_on(&h, "main", "alice", &a, "base\n", None).await;
        put_on(&h, "main", "alice", &path("Source/b.cpp"), "b\n", None).await;
        h.svc
            .create_branch(&who("alice"), "feature", Some("main"))
            .await
            .unwrap();
        put_on(&h, "feature", "bob", &a, "feature edit\n", Some(1)).await
    };
    let tip = head(&h, "main").await.unwrap();
    let mut files = Files::new();
    set(&mut files, "Source/a.cpp", blob(&h, "base\n").await);
    set(&mut files, "Source/b.cpp", blob(&h, "b\n").await);
    set(&mut files, POLICY, blob(&h, A_EXCLUSIVE).await);
    let commit = build(&files, Some(tip), "policy", 5);

    let pushed = push(&h, &who("alice"), "main", &[&commit]).await.unwrap();
    assert_eq!(pushed.stranded.len(), 1);
    let v = &pushed.stranded[0];
    let conversion = h.svc.history(&a).await.unwrap().last().unwrap().id;
    assert_eq!(
        (
            &v.path,
            v.branch.as_str(),
            &v.branch_owner,
            v.revision,
            &v.author,
            v.conversion
        ),
        (
            &a,
            "feature",
            &user("alice"),
            edit.id,
            &user("bob"),
            Some(conversion)
        )
    );
    let told = h
        .svc
        .audit(&AuditQuery {
            action: Some(AuditAction::VersionStranded),
            limit: 10,
            ..AuditQuery::default()
        })
        .await
        .unwrap();
    assert_eq!(told.len(), 1);
    assert!(
        h.svc
            .history_on("feature", &a)
            .await
            .unwrap()
            .iter()
            .any(|r| r.id == edit.id),
        "the stranded version stays in history"
    );
}

pub async fn a_policy_file_pushed_to_another_branch_is_just_a_file(store: Store) {
    let h = harness(store);
    let (mut files, base) = seeded(&h).await;
    h.svc
        .create_branch(&who("alice"), "feature", Some("main"))
        .await
        .unwrap();

    // Not even valid TOML is checked there, and the permission is not asked for.
    let junk = blob(&h, "this is not [valid toml").await;
    set(&mut files, POLICY, junk);
    let first = build(&files, Some(base.id), "junk policy", 2);
    let writer = Principal {
        user: user("wanda"),
        permissions: BTreeSet::from([Permission::Read, Permission::Checkin]),
        email: None,
    };
    let policy_path = path(POLICY);
    h.svc
        .checkout_on("feature", &policy_path, &user("wanda"), None)
        .await
        .unwrap();
    let pushed = push(&h, &writer, "feature", &[&first]).await.unwrap();
    assert!(pushed.stranded.is_empty());
    assert_eq!(pushed.revisions[0].path.as_str(), POLICY);
    assert_eq!(head(&h, "feature").await, Some(first.id));
    assert_eq!(head(&h, "main").await, Some(base.id));
    assert_eq!(h.svc.mode_for(&path("Source/a.cpp")), Mode::Shared);
    assert!(
        h.svc
            .audit(&AuditQuery {
                action: Some(AuditAction::PolicyChanged),
                limit: 10,
                ..AuditQuery::default()
            })
            .await
            .unwrap()
            .is_empty()
    );

    // It can be deleted there too.
    files.remove(POLICY);
    let second = build(&files, Some(first.id), "drop it", 3);
    h.svc
        .checkout_on("feature", &policy_path, &user("alice"), Some(RevisionId(1)))
        .await
        .unwrap();
    let pushed = push(&h, &who("alice"), "feature", &[&second])
        .await
        .unwrap();
    assert!(pushed.revisions[0].deleted);
}
