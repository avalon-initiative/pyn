//! Policy on branches every `MetadataStore` must satisfy, through `RepoService`: the effective policy of a branch and
//! the stranded-version report (decisions #38, #195 and #233).

use super::{Harness, Store, blob, harness, path, put_on, user, who};
use crate::audit::{AuditAction, AuditQuery};
use crate::merge::{ConflictKind, FastForward, MergeOutcome, MergeRequest};
use crate::{FileMode, Mode, PynError, RepoPath, Revision, RevisionId};

const POLICY: &str = ".pyn/pyn.toml";
const A_EXCLUSIVE: &str = "[meta]\ndefault = \"shared\"\n[exclusive]\npaths = [\"Source/a.cpp\"]\n";

fn merge_of(source: &str, target: &str) -> MergeRequest {
    MergeRequest {
        source: source.to_string(),
        target: Some(target.to_string()),
        fast_forward: FastForward::Allow,
        message: None,
        expect_target: None,
        resolutions: Vec::new(),
    }
}

async fn policy_on(h: &Harness, branch: &str, text: &str, base: Option<u64>) -> crate::Recorded {
    let content = blob(h, text).await;
    h.svc
        .checkin_recorded(
            branch,
            &who("alice"),
            &path(POLICY),
            content,
            FileMode::Regular,
            base.map(RevisionId),
            "policy".into(),
        )
        .await
        .unwrap()
}

async fn mode_on(h: &Harness, branch: &str, p: &RepoPath) -> Mode {
    h.svc
        .snapshot_on(branch)
        .await
        .unwrap()
        .entries
        .into_iter()
        .find(|e| &e.path == p)
        .unwrap()
        .mode
}

/// `a.cpp` on main, a branch `feature` with a diverged edit by bob, and a branch `idle` with none.
async fn diverged(h: &Harness) -> (RepoPath, Revision) {
    let a = path("Source/a.cpp");
    put_on(h, "main", "alice", &a, "base\n", None).await;
    put_on(h, "main", "alice", &path("Source/b.cpp"), "b\n", None).await;
    for name in ["feature", "idle"] {
        h.svc
            .create_branch(&who("alice"), name, Some("main"))
            .await
            .unwrap();
    }
    let edit = put_on(h, "feature", "bob", &a, "feature edit\n", Some(1)).await;
    (a, edit)
}

pub async fn a_branch_keeps_the_policy_it_has_until_it_picks_up_a_later_one(store: Store) {
    let h = harness(store);
    let a = path("Source/a.cpp");
    put_on(&h, "main", "alice", &a, "base\n", None).await;
    policy_on(&h, "main", "[meta]\ndefault = \"shared\"\n", None).await;
    let first = h.svc.head_of("main").await.unwrap().unwrap().to_hex();
    for name in ["old", "old2"] {
        h.svc
            .create_branch(&who("alice"), name, Some(&first))
            .await
            .unwrap();
    }
    assert_eq!(mode_on(&h, "old", &a).await, Mode::Shared);

    h.svc
        .checkout(&path(POLICY), &user("alice"), Some(RevisionId(1)))
        .await
        .unwrap();
    policy_on(&h, "main", A_EXCLUSIVE, Some(1)).await;
    h.svc
        .create_branch(&who("alice"), "new", Some("main"))
        .await
        .unwrap();
    assert_eq!(mode_on(&h, "main", &a).await, Mode::Exclusive);
    assert_eq!(
        mode_on(&h, "new", &a).await,
        Mode::Exclusive,
        "a branch starts with the policy of its start commit"
    );
    assert_eq!(
        mode_on(&h, "old", &a).await,
        Mode::Shared,
        "a branch that has not picked up the policy commit keeps the earlier policy"
    );
    let on_old = h.svc.files_on("old", None, 10).await.unwrap();
    assert!(on_old.iter().any(|f| f.path == a && f.mode == Mode::Shared));

    let err = h
        .svc
        .checkout_on("old", &a, &user("bob"), Some(RevisionId(2)))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::NotExclusive(_)), "{err}");
    h.svc
        .checkout_on("new", &a, &user("bob"), Some(RevisionId(2)))
        .await
        .unwrap();
    let err = h
        .svc
        .checkout_on("main", &a, &user("carol"), Some(RevisionId(2)))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::LockHeld { .. }), "{err}");

    let kept = put_on(&h, "old2", "bob", &a, "edit on old2\n", Some(1)).await;
    assert_eq!(
        kept.mode,
        Some(Mode::Shared),
        "no lock on a branch that sees the path as shared"
    );

    let update = h
        .svc
        .merge(&who("alice"), merge_of("main", "old"))
        .await
        .unwrap();
    assert_eq!(update.outcome, MergeOutcome::FastForward);
    assert_eq!(mode_on(&h, "old", &a).await, Mode::Exclusive);
    let blocked = h
        .svc
        .merge_preview(&merge_of("main", "old2"))
        .await
        .unwrap();
    assert_eq!(blocked.outcome, MergeOutcome::Conflicted);
    assert_eq!(blocked.conflicts[0].kind, ConflictKind::ExclusiveConversion);
    assert_eq!(mode_on(&h, "old2", &a).await, Mode::Shared);
}

pub async fn a_policy_file_on_a_branch_only_counts_once_it_reaches_the_default_branch(
    store: Store,
) {
    let h = harness(store);
    let a = path("Source/a.cpp");
    put_on(&h, "main", "alice", &a, "base\n", None).await;
    policy_on(&h, "main", "[meta]\ndefault = \"shared\"\n", None).await;
    h.svc
        .create_branch(&who("alice"), "feature", Some("main"))
        .await
        .unwrap();
    h.svc
        .checkout(&path(POLICY), &user("alice"), Some(RevisionId(1)))
        .await
        .unwrap();
    policy_on(&h, "feature", A_EXCLUSIVE, Some(1)).await;
    assert_eq!(
        mode_on(&h, "feature", &a).await,
        Mode::Shared,
        "validated but never applied on the branch that made it"
    );
    h.svc
        .create_branch(&who("alice"), "child", Some("feature"))
        .await
        .unwrap();
    assert_eq!(mode_on(&h, "child", &a).await, Mode::Shared);
    assert_eq!(h.svc.mode_for(&a), Mode::Shared);

    h.svc
        .merge(&who("alice"), merge_of("feature", "main"))
        .await
        .unwrap();
    assert_eq!(h.svc.mode_for(&a), Mode::Exclusive);
    assert_eq!(
        mode_on(&h, "feature", &a).await,
        Mode::Exclusive,
        "the branch's own policy commit reached the default branch"
    );
    assert_eq!(mode_on(&h, "child", &a).await, Mode::Exclusive);
}

pub async fn shared_to_exclusive_strands_diverged_branches_without_refusing(store: Store) {
    let h = harness(store);
    let (a, edit) = diverged(&h).await;

    let preview = h
        .svc
        .preview_policy(&who("alice"), A_EXCLUSIVE)
        .await
        .unwrap();
    assert_eq!(preview.to_exclusive, vec![a.clone()]);
    assert_eq!(
        preview.stranded.len(),
        1,
        "the idle branch has nothing diverged"
    );
    let v = &preview.stranded[0];
    assert_eq!(
        (
            &v.path,
            v.branch.as_str(),
            &v.branch_owner,
            v.revision,
            &v.author,
            v.conversion
        ),
        (&a, "feature", &user("alice"), edit.id, &user("bob"), None)
    );

    let recorded = policy_on(&h, "main", A_EXCLUSIVE, None).await;
    let line = h.svc.history(&a).await.unwrap();
    let conversion = line.last().unwrap();
    assert_eq!(conversion.mode, Some(Mode::Exclusive));
    assert_eq!(recorded.stranded.len(), 1);
    assert_eq!(recorded.stranded[0].revision, edit.id);
    assert_eq!(recorded.stranded[0].conversion, Some(conversion.id));

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
    assert_eq!(told[0].path.as_ref(), Some(&a));
    assert_eq!(told[0].actor, user("alice"));
    for part in [
        "feature",
        "created by alice",
        "by bob",
        "converted to exclusive",
    ] {
        assert!(told[0].detail.contains(part), "{}", told[0].detail);
    }
    assert!(
        h.svc
            .history_on("feature", &a)
            .await
            .unwrap()
            .iter()
            .any(|r| r.id == edit.id),
        "the stranded version stays in history"
    );
    let report = h.svc.verify(None, true).await.unwrap();
    assert!(report.problems.is_empty(), "{:?}", report.problems);
}

pub async fn a_merge_after_a_conversion_shows_what_happened_and_when(store: Store) {
    let h = harness(store);
    let (a, edit) = diverged(&h).await;
    h.clock.advance(chrono::Duration::hours(2));
    policy_on(&h, "main", A_EXCLUSIVE, None).await;
    let conversion = h.svc.history(&a).await.unwrap().pop().unwrap();
    assert!(conversion.created_at > edit.created_at);

    for (source, target) in [("feature", "main"), ("main", "feature")] {
        let preview = h
            .svc
            .merge_preview(&merge_of(source, target))
            .await
            .unwrap();
        assert_eq!(preview.outcome, MergeOutcome::Conflicted);
        let conflict = &preview.conflicts[0];
        assert_eq!(conflict.kind, ConflictKind::ExclusiveConversion);
        let note = conflict.conversion.as_ref().expect("a conversion note");
        assert_eq!(note.revision, conversion.id);
        assert_eq!(note.policy, Some(RevisionId(1)));
        assert_eq!(
            (note.by.clone(), note.at),
            (user("alice"), conversion.created_at)
        );
        assert_eq!(
            (note.diverged, note.diverged_by.clone(), note.diverged_at),
            (edit.id, user("bob"), edit.created_at)
        );
    }
    let other = h
        .svc
        .merge_preview(&merge_of("idle", "main"))
        .await
        .unwrap();
    assert_eq!(other.outcome, MergeOutcome::UpToDate);
}

pub async fn a_version_made_on_a_stale_branch_after_the_conversion_is_still_diverged(store: Store) {
    let h = harness(store);
    let (a, edit) = diverged(&h).await;
    policy_on(&h, "main", A_EXCLUSIVE, None).await;
    let conversion = h.svc.history(&a).await.unwrap().pop().unwrap();
    let later = put_on(&h, "feature", "bob", &a, "after\n", Some(edit.id.0)).await;
    assert!(later.id > conversion.id);

    let preview = h
        .svc
        .merge_preview(&merge_of("feature", "main"))
        .await
        .unwrap();
    assert_eq!(preview.outcome, MergeOutcome::Conflicted);
    let note = preview.conflicts[0].conversion.as_ref().unwrap();
    assert_eq!(note.diverged, later.id);

    let same = h
        .svc
        .preview_policy(&who("alice"), "[meta]\ndefault = \"shared\"\n")
        .await
        .unwrap();
    assert!(same.stranded.is_empty(), "back to shared strands nothing");
}

pub async fn exclusive_to_shared_releases_locks_and_never_strands(store: Store) {
    let h = harness(store);
    let map = path("Content/a.umap");
    h.svc.checkout(&map, &user("bob"), None).await.unwrap();
    put_on(&h, "main", "bob", &map, "1\n2\n3\n", None).await;
    h.svc
        .create_branch(&who("alice"), "feature", Some("main"))
        .await
        .unwrap();
    h.svc
        .checkout(&map, &user("bob"), Some(RevisionId(1)))
        .await
        .unwrap();

    let recorded = policy_on(&h, "main", "[meta]\ndefault = \"shared\"\n", None).await;
    assert!(recorded.stranded.is_empty());
    assert!(h.svc.locks().await.unwrap().is_empty());
    let released = h
        .svc
        .audit(&AuditQuery {
            action: Some(AuditAction::ForceUnlock),
            limit: 10,
            ..AuditQuery::default()
        })
        .await
        .unwrap();
    assert_eq!(released.len(), 1);
    assert_eq!(h.svc.mode_for(&map), Mode::Shared);

    assert_eq!(mode_on(&h, "feature", &map).await, Mode::Exclusive);
    h.svc
        .checkout_on("feature", &map, &user("carol"), Some(RevisionId(1)))
        .await
        .unwrap();
    put_on(&h, "feature", "carol", &map, "ONE\n2\n3\n", Some(1)).await;
    let err = h
        .svc
        .checkout_on("main", &map, &user("dave"), Some(RevisionId(1)))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::NotExclusive(_)), "{err}");
    put_on(&h, "main", "dave", &map, "1\n2\nthree\n", Some(1)).await;

    let done = h
        .svc
        .merge(&who("alice"), merge_of("main", "feature"))
        .await
        .unwrap();
    assert_eq!(done.outcome, MergeOutcome::Merge);
    assert_eq!(mode_on(&h, "feature", &map).await, Mode::Shared);
    let report = h.svc.verify(None, true).await.unwrap();
    assert!(report.problems.is_empty(), "{:?}", report.problems);
}

pub async fn a_policy_arriving_by_merge_reports_the_versions_it_strands(store: Store) {
    let h = harness(store);
    let (a, edit) = diverged(&h).await;
    h.svc
        .create_branch(&who("alice"), "policy", Some("main"))
        .await
        .unwrap();
    let policy = path(POLICY);
    h.svc
        .checkout_on("policy", &policy, &user("alice"), None)
        .await
        .unwrap();
    policy_on(&h, "policy", A_EXCLUSIVE, None).await;
    assert_eq!(h.svc.mode_for(&a), Mode::Shared);

    let done = h
        .svc
        .merge(&who("alice"), merge_of("policy", "main"))
        .await
        .unwrap();
    assert_eq!(h.svc.mode_for(&a), Mode::Exclusive);
    assert_eq!(done.stranded.len(), 1);
    assert_eq!(done.stranded[0].revision, edit.id);
    assert_eq!(done.stranded[0].branch, "feature");
    assert!(done.stranded[0].conversion.is_some());
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
}
