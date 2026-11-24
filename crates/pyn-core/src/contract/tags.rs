//! Tags every `MetadataStore` must keep, through `RepoService`: immutable names pointing at a commit.

use super::{Harness, Store, harness, path, put_on, repo_record, who};
use crate::audit::AuditStore;
use crate::{AuditAction, AuditQuery, AuditScope, PynError};

async fn names(h: &Harness) -> Vec<String> {
    h.svc
        .tags()
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.name)
        .collect()
}

pub async fn a_tag_points_at_a_commit_and_never_moves(store: Store) {
    let h = harness(store);
    let a = path("Source/a.cpp");
    let err = h
        .svc
        .create_tag(&who("alice"), "v1", None, None)
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::InvalidRequest(_)), "{err}");

    let c1 = put_on(&h, "main", "alice", &a, "one", None).await.commit;
    let v1 = h
        .svc
        .create_tag(&who("alice"), "v1", None, Some("first release"))
        .await
        .unwrap();
    assert_eq!(
        (v1.commit, v1.message.as_deref(), v1.created_by.as_str()),
        (c1, Some("first release"), "alice")
    );
    assert_eq!(h.svc.tag("v1").await.unwrap(), v1);

    let c2 = put_on(&h, "main", "alice", &a, "two", Some(1)).await.commit;
    assert_eq!(
        h.svc.tag("v1").await.unwrap().commit,
        c1,
        "main moved, the tag did not"
    );
    let err = h
        .svc
        .create_tag(&who("alice"), "v1", Some(&c2.to_hex()), None)
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::TagExists(_)), "{err}");
    assert_eq!(
        h.svc.tag("v1").await.unwrap().commit,
        c1,
        "never overwritten"
    );

    let by_id = h
        .svc
        .create_tag(&who("bob"), "v2", Some(&c2.to_hex()), None)
        .await
        .unwrap();
    assert_eq!((by_id.commit, by_id.message), (c2, None));
    let by_branch = h
        .svc
        .create_tag(&who("bob"), "tip", Some("main"), None)
        .await
        .unwrap();
    assert_eq!(by_branch.commit, c2);
    let by_tag = h
        .svc
        .create_tag(&who("bob"), "again", Some("v1"), None)
        .await
        .unwrap();
    assert_eq!(by_tag.commit, c1);
    assert_eq!(names(&h).await, ["again", "tip", "v1", "v2"]);
}

pub async fn tag_names_follow_ref_rules_and_a_tag_must_start_at_a_commit(store: Store) {
    let h = harness(store);
    put_on(&h, "main", "alice", &path("Source/a.cpp"), "one", None).await;
    h.svc
        .create_tag(&who("alice"), "rel/1", None, None)
        .await
        .unwrap();
    for bad in ["", "bad name", "a..b", "-x", "x.lock", "HEAD"] {
        let err = h
            .svc
            .create_tag(&who("alice"), bad, None, None)
            .await
            .unwrap_err();
        assert!(matches!(err, PynError::InvalidTagName(_)), "{bad:?}: {err}");
    }
    for folder in ["rel", "rel/1/a"] {
        let err = h
            .svc
            .create_tag(&who("alice"), folder, None, None)
            .await
            .unwrap_err();
        assert!(matches!(err, PynError::TagExists(_)), "{folder}: {err}");
    }
    let err = h
        .svc
        .create_tag(&who("alice"), "x", Some("deadbeefdead"), None)
        .await
        .unwrap_err();
    assert!(matches!(err, PynError::CommitNotFound(_)), "{err}");
    assert!(matches!(
        h.svc.tag("missing").await.unwrap_err(),
        PynError::TagNotFound(_)
    ));
    assert_eq!(names(&h).await, ["rel/1"]);
}

pub async fn a_deleted_tag_frees_its_name_and_leaves_the_commit(store: Store) {
    let h = harness(store);
    let a = path("Source/a.cpp");
    let c1 = put_on(&h, "main", "alice", &a, "one", None).await.commit;
    let c2 = put_on(&h, "main", "alice", &a, "two", Some(1)).await.commit;
    h.svc
        .create_tag(&who("alice"), "v1", Some(&c1.to_hex()), None)
        .await
        .unwrap();

    h.svc.delete_tag(&who("alice"), "v1").await.unwrap();
    let err = h.svc.delete_tag(&who("alice"), "v1").await.unwrap_err();
    assert!(matches!(err, PynError::TagNotFound(_)), "{err}");
    assert!(names(&h).await.is_empty());
    assert!(h.svc.get_commit(&c1).await.is_ok(), "the commit stays");

    let again = h
        .svc
        .create_tag(&who("alice"), "v1", Some(&c2.to_hex()), None)
        .await
        .unwrap();
    assert_eq!(again.commit, c2);

    let actions: Vec<_> = h
        .audit
        .list(
            &AuditScope::Repo(h.svc.repo().clone()),
            &AuditQuery {
                limit: 100,
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.action)
        .filter(|a| matches!(a, AuditAction::TagCreated | AuditAction::TagDeleted))
        .collect();
    assert_eq!(
        actions,
        [
            AuditAction::TagCreated,
            AuditAction::TagDeleted,
            AuditAction::TagCreated
        ]
    );
}

pub async fn tags_and_branches_keep_separate_names_and_a_tag_can_start_a_branch(store: Store) {
    let h = harness(store);
    let c1 = put_on(&h, "main", "alice", &path("Source/a.cpp"), "one", None)
        .await
        .commit;
    h.svc
        .create_branch(&who("alice"), "v1", None)
        .await
        .unwrap();
    h.svc
        .create_tag(&who("alice"), "v1", None, None)
        .await
        .unwrap();
    h.svc
        .delete_branch(&who("alice"), "v1", true)
        .await
        .unwrap();
    assert_eq!(h.svc.tag("v1").await.unwrap().commit, c1);

    let from_tag = h
        .svc
        .create_branch(&who("alice"), "hotfix", Some("v1"))
        .await
        .unwrap();
    assert_eq!(from_tag.head, Some(c1));
}

pub async fn deleting_a_repository_removes_its_tags(store: Store) {
    let h = harness(store.clone());
    store
        .create_repo(repo_record("game", "alice", "game"))
        .await
        .unwrap();
    put_on(&h, "main", "alice", &path("Source/a.cpp"), "one", None).await;
    h.svc
        .create_tag(&who("alice"), "v1", None, None)
        .await
        .unwrap();
    assert!(store.delete_repo(h.svc.repo()).await.unwrap());
    assert!(names(&h).await.is_empty());
}
