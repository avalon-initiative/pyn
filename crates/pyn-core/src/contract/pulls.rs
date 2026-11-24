//! Pull requests every `MetadataStore` must satisfy, through `RepoService`.

use super::{Harness, Store, harness, path, put_on, user, who};
use crate::access::{Permission, Principal};
use crate::merge::{ConflictKind, FastForward, MergeOutcome};
use crate::pull::{PullEdit, PullEventKind, PullFilter, PullState};
use crate::service::{MergePull, OpenPull};
use crate::{PynError, RevisionId};

fn limited(name: &str, permissions: &[Permission]) -> Principal {
    Principal {
        user: user(name),
        permissions: permissions.iter().copied().collect(),
        email: None,
        acting_admin: None,
    }
}

fn writer(name: &str) -> Principal {
    limited(
        name,
        &[Permission::Read, Permission::Lock, Permission::Checkin],
    )
}

fn open(head: &str) -> OpenPull {
    OpenPull {
        title: format!("Propose {head}"),
        body: "why".into(),
        draft: false,
        base: None,
        head: head.into(),
    }
}

async fn branch_at(h: &Harness, name: &str) {
    h.svc
        .create_branch(&who("alice"), name, Some("main"))
        .await
        .unwrap();
}

/// A repository with `Source/a.txt` on main and a branch `f` that adds `Source/b.txt`.
async fn with_branch(store: Store) -> Harness {
    let h = harness(store);
    put_on(&h, "main", "alice", &path("Source/a.txt"), "a0\n", None).await;
    branch_at(&h, "f").await;
    put_on(&h, "f", "alice", &path("Source/b.txt"), "b\n", None).await;
    h
}

pub async fn a_pull_request_is_opened_listed_and_found_by_number(store: Store) {
    let h = with_branch(store).await;
    let first = h.svc.open_pull(&writer("alice"), open("f")).await.unwrap();
    assert_eq!(first.number, 1);
    assert_eq!(
        (
            first.state,
            first.draft,
            first.base.as_str(),
            first.head.as_str()
        ),
        (PullState::Open, false, "main", "f")
    );
    assert_eq!((first.author.clone(), first.comments), (user("alice"), 0));
    assert!(first.frozen.is_none() && first.merged_at.is_none());

    let err = h
        .svc
        .open_pull(&writer("bob"), open("f"))
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::PullRequestExists(1)), "{err}");

    branch_at(&h, "g").await;
    put_on(&h, "g", "alice", &path("Source/c.txt"), "c\n", None).await;
    let draft = OpenPull {
        draft: true,
        base: Some("f".into()),
        ..open("g")
    };
    let second = h.svc.open_pull(&writer("bob"), draft).await.unwrap();
    assert_eq!((second.number, second.draft), (2, true));

    let all = h
        .svc
        .list_pulls(&PullFilter::default(), None, None)
        .await
        .unwrap();
    assert_eq!(all.iter().map(|p| p.number).collect::<Vec<_>>(), [2, 1]);
    let page = h
        .svc
        .list_pulls(&PullFilter::default(), Some(2), Some(1))
        .await
        .unwrap();
    assert_eq!(page.iter().map(|p| p.number).collect::<Vec<_>>(), [1]);
    let by = |f: PullFilter| {
        let h = &h;
        async move {
            h.svc
                .list_pulls(&f, None, None)
                .await
                .unwrap()
                .iter()
                .map(|p| p.number)
                .collect::<Vec<_>>()
        }
    };
    assert_eq!(
        by(PullFilter {
            author: Some(user("bob")),
            ..Default::default()
        })
        .await,
        [2]
    );
    assert_eq!(
        by(PullFilter {
            base: Some("main".into()),
            ..Default::default()
        })
        .await,
        [1]
    );
    assert_eq!(
        by(PullFilter {
            state: Some(PullState::Merged),
            ..Default::default()
        })
        .await,
        Vec::<u64>::new()
    );
    assert_eq!(h.svc.get_pull(2).await.unwrap().title, "Propose g");
    let err = h.svc.get_pull(9).await.unwrap_err();
    assert!(matches!(err, PynError::PullRequestNotFound(9)), "{err}");
}

pub async fn opening_a_pull_request_is_checked(store: Store) {
    let h = with_branch(store).await;
    let reader = limited("rita", &[Permission::Read]);
    let err = h.svc.open_pull(&reader, open("f")).await.unwrap_err();
    assert!(
        matches!(err, PynError::Forbidden(Permission::Checkin)),
        "{err}"
    );
    let same = OpenPull {
        base: Some("f".into()),
        ..open("f")
    };
    for bad in [
        same,
        open("nope"),
        OpenPull {
            base: Some("nope".into()),
            ..open("f")
        },
        OpenPull {
            title: "  ".into(),
            ..open("f")
        },
        OpenPull {
            title: "two\nlines".into(),
            ..open("f")
        },
        OpenPull {
            body: "x".repeat(70_000),
            ..open("f")
        },
        open("main"),
    ] {
        let err = h.svc.open_pull(&writer("alice"), bad).await.unwrap_err();
        assert!(
            matches!(
                err,
                PynError::InvalidRequest(_) | PynError::BranchNotFound(_)
            ),
            "{err}"
        );
    }
    assert!(
        h.svc
            .list_pulls(&PullFilter::default(), None, None)
            .await
            .unwrap()
            .is_empty()
    );
}

pub async fn anyone_who_can_read_comments_and_the_events_are_ordered(store: Store) {
    let h = with_branch(store).await;
    h.svc.open_pull(&writer("alice"), open("f")).await.unwrap();
    let reader = limited("rita", &[Permission::Read]);
    let c = h
        .svc
        .comment_pull(&reader, 1, "  looks fine  ")
        .await
        .unwrap();
    assert_eq!(
        (c.kind, c.actor.clone(), c.body.as_deref()),
        (PullEventKind::Comment, user("rita"), Some("looks fine"))
    );
    h.svc
        .comment_pull(&writer("alice"), 1, "thanks")
        .await
        .unwrap();
    let nobody = limited("eve", &[]);
    let err = h.svc.comment_pull(&nobody, 1, "hi").await.unwrap_err();
    assert!(
        matches!(err, PynError::Forbidden(Permission::Read)),
        "{err}"
    );
    for bad in ["", "   "] {
        let err = h.svc.comment_pull(&reader, 1, bad).await.unwrap_err();
        assert!(matches!(err, PynError::InvalidRequest(_)), "{err}");
    }
    let err = h.svc.comment_pull(&reader, 7, "x").await.unwrap_err();
    assert!(matches!(err, PynError::PullRequestNotFound(7)), "{err}");

    assert_eq!(h.svc.get_pull(1).await.unwrap().comments, 2);
    let events = h.svc.pull_events(1, None, None).await.unwrap();
    assert_eq!(
        events
            .iter()
            .map(|e| e.body.clone().unwrap())
            .collect::<Vec<_>>(),
        ["looks fine", "thanks"]
    );
    assert!(events[0].id < events[1].id);
    let later = h
        .svc
        .pull_events(1, Some(events[0].id), None)
        .await
        .unwrap();
    assert_eq!(later.len(), 1);
    let detail = h.svc.pull_detail(1).await.unwrap();
    assert_eq!(detail.participants, [user("alice"), user("rita")]);
}

pub async fn a_pull_request_is_edited_closed_and_reopened(store: Store) {
    let h = with_branch(store).await;
    let draft = OpenPull {
        draft: true,
        ..open("f")
    };
    h.svc.open_pull(&writer("alice"), draft).await.unwrap();
    let err = h
        .svc
        .edit_pull(
            &limited("rita", &[Permission::Read]),
            1,
            PullEdit::default(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::Forbidden(_)), "{err}");
    let edited = h
        .svc
        .edit_pull(
            &writer("alice"),
            1,
            PullEdit {
                title: Some(" New title ".into()),
                body: Some("more".into()),
                draft: Some(false),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        (edited.title.as_str(), edited.body.as_str(), edited.draft),
        ("New title", "more", false)
    );

    let closed = h.svc.close_pull(&writer("alice"), 1).await.unwrap();
    assert_eq!(closed.state, PullState::Closed);
    assert!(closed.closed_at.is_some() && closed.frozen.is_some());
    let err = h.svc.close_pull(&writer("alice"), 1).await.unwrap_err();
    assert!(
        matches!(err, PynError::PullRequestNotOpen { number: 1, .. }),
        "{err}"
    );
    let err = h
        .svc
        .edit_pull(&writer("alice"), 1, PullEdit::default())
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::PullRequestNotOpen { .. }), "{err}");
    let err = h
        .svc
        .reopen_pull(&limited("rita", &[Permission::Read]), 1)
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::Forbidden(_)), "{err}");

    // a second proposal of the same branches blocks the reopen
    let twin = h.svc.open_pull(&writer("bob"), open("f")).await.unwrap();
    let err = h.svc.reopen_pull(&writer("alice"), 1).await.unwrap_err();
    assert!(matches!(err, PynError::PullRequestExists(2)), "{err}");
    h.svc.close_pull(&writer("bob"), twin.number).await.unwrap();
    let reopened = h.svc.reopen_pull(&writer("alice"), 1).await.unwrap();
    assert!(
        reopened.state == PullState::Open
            && reopened.closed_at.is_none()
            && reopened.frozen.is_none()
    );

    let kinds: Vec<_> = h
        .svc
        .pull_events(1, None, None)
        .await
        .unwrap()
        .iter()
        .map(|e| e.kind)
        .collect();
    assert_eq!(kinds, [PullEventKind::Closed, PullEventKind::Reopened]);

    let author_only = limited("alice", &[Permission::Read]);
    assert_eq!(
        h.svc.close_pull(&author_only, 1).await.unwrap().state,
        PullState::Closed
    );
}

pub async fn a_pull_request_merges_by_fast_forward_or_merge_commit(store: Store) {
    let h = with_branch(store).await;
    let pull = h.svc.open_pull(&writer("alice"), open("f")).await.unwrap();
    let head = h.svc.head_of("f").await.unwrap().unwrap();
    let before = h.svc.head_of("main").await.unwrap().unwrap();

    let detail = h.svc.pull_detail(pull.number).await.unwrap();
    assert_eq!(
        detail.mergeability.unwrap().outcome,
        MergeOutcome::FastForward
    );
    assert_eq!(
        (
            detail.commits,
            detail.changed_files,
            detail.additions,
            detail.deletions
        ),
        (1, 1, 1, 0)
    );

    let err = h
        .svc
        .merge_pull(
            &limited("rita", &[Permission::Read]),
            1,
            MergePull::default(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, PynError::Forbidden(Permission::Checkin)),
        "{err}"
    );
    let stale = MergePull {
        expect_target: Some(head),
        ..Default::default()
    };
    let err = h
        .svc
        .merge_pull(&writer("bob"), 1, stale)
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::MergeStale(_)), "{err}");
    let stale = MergePull {
        expect_head: Some(before),
        ..Default::default()
    };
    let err = h
        .svc
        .merge_pull(&writer("bob"), 1, stale)
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::MergeStale(_)), "{err}");

    let merged = h
        .svc
        .merge_pull(
            &writer("bob"),
            1,
            MergePull {
                expect_target: Some(before),
                expect_head: Some(head),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(merged.state, PullState::Merged);
    assert_eq!(
        (merged.merged_by.clone(), merged.merge_commit),
        (Some(user("bob")), Some(head))
    );
    assert_eq!(merged.frozen, Some((before, head)));
    assert_eq!(h.svc.head_of("main").await.unwrap(), Some(head));
    let err = h
        .svc
        .merge_pull(&writer("bob"), 1, MergePull::default())
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::PullRequestNotOpen { .. }), "{err}");
    let err = h.svc.reopen_pull(&writer("bob"), 1).await.unwrap_err();
    assert!(matches!(err, PynError::PullRequestNotOpen { .. }), "{err}");
    let events = h.svc.pull_events(1, None, None).await.unwrap();
    assert_eq!(
        (events[0].kind, events[0].commit),
        (PullEventKind::Merged, Some(head))
    );

    // the range stays readable once the base holds the head
    assert_eq!(h.svc.pull_commits(1).await.unwrap().len(), 1);
    let diff = h.svc.pull_diff(1, 3).await.unwrap();
    assert_eq!(diff.files.len(), 1);
    assert_eq!((diff.base.as_str(), diff.head.as_str()), ("main", "f"));
    let listed = h
        .svc
        .list_pulls(
            &PullFilter {
                state: Some(PullState::Merged),
                ..Default::default()
            },
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);

    // diverged branches and a request for no fast-forward make a merge commit
    branch_at(&h, "g").await;
    put_on(&h, "g", "alice", &path("Source/c.txt"), "c\n", None).await;
    put_on(&h, "main", "alice", &path("Source/d.txt"), "d\n", None).await;
    h.svc.open_pull(&writer("alice"), open("g")).await.unwrap();
    let by_commit = h
        .svc
        .merge_pull(
            &writer("alice"),
            2,
            MergePull {
                fast_forward: FastForward::Only,
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(by_commit, PynError::NotFastForward(_)),
        "{by_commit}"
    );
    let merged = h
        .svc
        .merge_pull(&writer("alice"), 2, MergePull::default())
        .await
        .unwrap();
    let commit = h
        .svc
        .get_commit(&merged.merge_commit.unwrap())
        .await
        .unwrap();
    assert_eq!(commit.parents.len(), 2);
    assert_eq!(commit.message, "Merge pull request #2 from g");
}

pub async fn a_draft_cannot_merge_until_it_is_ready(store: Store) {
    let h = with_branch(store).await;
    h.svc
        .open_pull(
            &writer("alice"),
            OpenPull {
                draft: true,
                ..open("f")
            },
        )
        .await
        .unwrap();
    let err = h
        .svc
        .merge_pull(&writer("alice"), 1, MergePull::default())
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::PullRequestDraft(1)), "{err}");
    h.svc
        .edit_pull(
            &writer("alice"),
            1,
            PullEdit {
                draft: Some(false),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    h.svc
        .merge_pull(&writer("alice"), 1, MergePull::default())
        .await
        .unwrap();
}

pub async fn a_head_already_in_the_base_merges_without_a_commit(store: Store) {
    let h = with_branch(store).await;
    h.svc.open_pull(&writer("alice"), open("f")).await.unwrap();
    let merge = crate::merge::MergeRequest {
        source: "f".into(),
        target: Some("main".into()),
        fast_forward: FastForward::Allow,
        message: None,
        expect_target: None,
        resolutions: Vec::new(),
    };
    h.svc.merge(&who("alice"), merge).await.unwrap();
    let detail = h.svc.pull_detail(1).await.unwrap();
    assert_eq!(detail.mergeability.unwrap().outcome, MergeOutcome::UpToDate);
    let merged = h
        .svc
        .merge_pull(&writer("alice"), 1, MergePull::default())
        .await
        .unwrap();
    assert_eq!(
        (merged.state, merged.merge_commit),
        (PullState::Merged, None)
    );
}

pub async fn a_conflicted_pull_request_reports_the_paths_and_refuses_to_merge(store: Store) {
    let h = harness(store);
    let a = path("Source/a.txt");
    put_on(&h, "main", "alice", &a, "one\n", None).await;
    branch_at(&h, "f").await;
    put_on(&h, "main", "alice", &a, "main\n", Some(1)).await;
    put_on(&h, "f", "alice", &a, "feature\n", Some(1)).await;
    h.svc.open_pull(&writer("alice"), open("f")).await.unwrap();
    let detail = h.svc.pull_detail(1).await.unwrap();
    let m = detail.mergeability.unwrap();
    assert_eq!(m.outcome, MergeOutcome::Conflicted);
    assert_eq!(m.conflicts, [(a, ConflictKind::Content)]);
    let err = h
        .svc
        .merge_pull(&writer("alice"), 1, MergePull::default())
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::MergeConflict(_)), "{err}");
    assert_eq!(h.svc.get_pull(1).await.unwrap().state, PullState::Open);
}

pub async fn locks_others_hold_on_changed_paths_block_a_merge_unless_overridden(store: Store) {
    let h = harness(store);
    let map = path("Content/a.umap");
    h.svc.checkout(&map, &user("alice"), None).await.unwrap();
    put_on(&h, "main", "alice", &map, "v1\n", None).await;
    branch_at(&h, "f").await;
    h.svc
        .checkout_on("f", &map, &user("alice"), Some(RevisionId(1)))
        .await
        .unwrap();
    put_on(&h, "f", "alice", &map, "v2\n", Some(1)).await;
    h.svc.open_pull(&writer("alice"), open("f")).await.unwrap();

    let free = h.svc.pull_detail(1).await.unwrap();
    assert!(free.locks.is_empty());

    h.svc
        .checkout_on("f", &map, &user("bob"), Some(RevisionId(2)))
        .await
        .unwrap();
    let detail = h.svc.pull_detail(1).await.unwrap();
    assert_eq!(
        detail
            .locks
            .iter()
            .map(|l| (l.path.to_string(), l.owner.clone()))
            .collect::<Vec<_>>(),
        [("Content/a.umap".to_string(), user("bob"))]
    );

    let err = h
        .svc
        .merge_pull(&writer("carol"), 1, MergePull::default())
        .await
        .unwrap_err();
    match err {
        PynError::PullLocksHeld(held) => assert_eq!(held, [(map.clone(), user("bob"))]),
        other => panic!("{other}"),
    }
    assert_eq!(h.svc.get_pull(1).await.unwrap().state, PullState::Open);
    // the holder's own lock does not block
    let own = h
        .svc
        .merge_pull(&writer("bob"), 1, MergePull::default())
        .await
        .unwrap();
    assert_eq!(own.state, PullState::Merged);
}

pub async fn a_force_unlock_holder_may_merge_over_locks(store: Store) {
    let h = harness(store);
    let map = path("Content/a.umap");
    h.svc.checkout(&map, &user("alice"), None).await.unwrap();
    put_on(&h, "main", "alice", &map, "v1\n", None).await;
    branch_at(&h, "f").await;
    h.svc
        .checkout_on("f", &map, &user("alice"), Some(RevisionId(1)))
        .await
        .unwrap();
    put_on(&h, "f", "alice", &map, "v2\n", Some(1)).await;
    h.svc.open_pull(&writer("alice"), open("f")).await.unwrap();
    h.svc
        .checkout_on("f", &map, &user("bob"), Some(RevisionId(2)))
        .await
        .unwrap();
    let maintainer = limited(
        "maria",
        &[
            Permission::Read,
            Permission::Lock,
            Permission::Checkin,
            Permission::ForceUnlock,
        ],
    );
    let merged = h
        .svc
        .merge_pull(&maintainer, 1, MergePull::default())
        .await
        .unwrap();
    assert_eq!(merged.state, PullState::Merged);
    assert!(
        h.svc
            .locks()
            .await
            .unwrap()
            .iter()
            .any(|l| l.owner == user("bob")),
        "the lock stays"
    );
}

pub async fn renaming_a_branch_keeps_its_pull_requests(store: Store) {
    let h = with_branch(store).await;
    h.svc.open_pull(&writer("alice"), open("f")).await.unwrap();
    h.svc
        .rename_branch(&who("alice"), "f", "feature")
        .await
        .unwrap();
    assert_eq!(h.svc.get_pull(1).await.unwrap().head, "feature");
    h.svc
        .merge_pull(&writer("alice"), 1, MergePull::default())
        .await
        .unwrap();
}

pub async fn deleting_a_repository_removes_its_pull_requests(store: Store) {
    let h = with_branch(store.clone()).await;
    store
        .create_repo(super::repo_record("game", "alice", "game"))
        .await
        .unwrap();
    h.svc.open_pull(&writer("alice"), open("f")).await.unwrap();
    h.svc.comment_pull(&writer("alice"), 1, "x").await.unwrap();
    store
        .delete_repo(&crate::RepoId::new("game"))
        .await
        .unwrap();
    assert!(
        store
            .pulls()
            .get_pull(&crate::RepoId::new("game"), 1)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .pulls()
            .pull_events(&crate::RepoId::new("game"), 1, None, 10)
            .await
            .unwrap()
            .is_empty()
    );
}
