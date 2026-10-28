//! A repository's own storage cap and its usage breakdown: every store must refuse only writes that add stored bytes,
//! atomically with the write, and report distinct bytes by folder and path.

use std::sync::Arc;

use super::{Harness, Store, blob, entry, harness, path, repo_record, service_in, user, who};
use crate::{Deletion, FolderUsage, PathUsage, PynError, RevisionId};

struct OwnerCap(Option<u64>);

#[async_trait::async_trait]
impl crate::StorageCaps for OwnerCap {
    async fn storage_cap(&self, _owner: &crate::UserId) -> crate::Result<Option<u64>> {
        Ok(self.0)
    }
}

fn service(
    store: &Store,
    h: &Harness,
    id: &str,
    owner_cap: Option<u64>,
    repo_cap: Option<u64>,
) -> crate::RepoService {
    service_in(store, h, id)
        .with_storage_caps(user("alice"), Arc::new(OwnerCap(owner_cap)))
        .with_repo_storage_cap(format!("alice/{id}"), repo_cap)
}

async fn repo(store: &Store, id: &str) {
    store
        .create_repo(repo_record(id, "alice", id))
        .await
        .unwrap();
}

pub async fn a_repository_cap_refuses_only_writes_that_add_stored_bytes(store: Store) {
    let h = harness(store.clone());
    for id in ["game", "other"] {
        repo(&store, id).await;
    }
    let game = service(&store, &h, "game", None, Some(10));
    let other = service(&store, &h, "other", None, Some(10));
    let (a, b, c) = (path("Source/a"), path("Source/b"), path("Source/c"));
    let six = blob(&h, "123456").await;
    game.checkin(&who("alice"), &a, six, None, "a".into())
        .await
        .unwrap();

    let err = game
        .checkin(
            &who("alice"),
            &b,
            game.upload_blob(b"abcdef".to_vec()).await.unwrap(),
            None,
            "b".into(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&err, PynError::RepoStorageLimitReached { repository, limit: 10, used: 6 }
            if repository == "alice/game"),
        "{err}"
    );
    assert_eq!(err.code(), "repo_storage_limit_reached");
    assert!(
        game.head(&b).await.unwrap().is_none(),
        "a refused checkin leaves no revision"
    );

    game.checkin(&who("alice"), &b, six, None, "same bytes".into())
        .await
        .unwrap();
    other
        .checkin(
            &who("alice"),
            &a,
            other.upload_blob(b"abcdefghi".to_vec()).await.unwrap(),
            None,
            "the cap is per repository".into(),
        )
        .await
        .unwrap();

    // A cap below current use deletes nothing; writes that add no bytes, deletions and reads still work.
    let shrunk = service(&store, &h, "game", None, Some(1));
    shrunk
        .checkin(&who("alice"), &c, six, None, "no new bytes".into())
        .await
        .unwrap();
    shrunk
        .checkin_changes_on(
            "main",
            &who("alice"),
            Vec::new(),
            vec![Deletion {
                path: c.clone(),
                base: Some(RevisionId(1)),
            }],
            "free".into(),
        )
        .await
        .unwrap();
    assert!(shrunk.head(&a).await.unwrap().is_some());
    assert_eq!(
        store
            .repo_usage(&crate::RepoId::new("game"))
            .await
            .unwrap()
            .stored_bytes,
        6
    );
    let err = shrunk
        .checkin(
            &who("alice"),
            &c,
            shrunk.upload_blob(b"z".to_vec()).await.unwrap(),
            Some(RevisionId(2)),
            "grows".into(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            PynError::RepoStorageLimitReached {
                limit: 1,
                used: 6,
                ..
            }
        ),
        "{err}"
    );

    service(&store, &h, "game", None, None)
        .checkin(
            &who("alice"),
            &c,
            blob(&h, "x".repeat(100).as_str()).await,
            Some(RevisionId(2)),
            "uncapped".into(),
        )
        .await
        .unwrap();
}

pub async fn the_lower_of_the_owner_and_repository_caps_refuses(store: Store) {
    let h = harness(store.clone());
    repo(&store, "game").await;
    let tight_repo = service(&store, &h, "game", Some(100), Some(4));
    let err = tight_repo
        .checkin(
            &who("alice"),
            &path("Source/a"),
            blob(&h, "12345").await,
            None,
            "a".into(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, PynError::RepoStorageLimitReached { limit: 4, .. }),
        "{err}"
    );
    let tight_owner = service(&store, &h, "game", Some(4), Some(100));
    let err = tight_owner
        .checkin(
            &who("alice"),
            &path("Source/a"),
            blob(&h, "12345").await,
            None,
            "a".into(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, PynError::StorageLimitReached { limit: 4, .. }),
        "{err}"
    );
    assert!(tight_owner.head(&path("Source/a")).await.unwrap().is_none());
}

pub async fn a_changeset_past_the_repository_cap_applies_nothing(store: Store) {
    let h = harness(store.clone());
    repo(&store, "game").await;
    let svc = service(&store, &h, "game", None, Some(10));
    let (a, b) = (path("Source/a"), path("Source/b"));
    let (six, other) = (blob(&h, "123456").await, blob(&h, "abcdef").await);
    let err = svc
        .checkin_changeset(
            &who("alice"),
            vec![entry(&a, &six, None), entry(&b, &other, None)],
            "big".into(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, PynError::RepoStorageLimitReached { .. }),
        "{err}"
    );
    assert!(svc.head(&a).await.unwrap().is_none());
    svc.checkin(&who("alice"), &a, six, None, "fits".into())
        .await
        .unwrap();
}

pub async fn racing_checkins_cannot_exceed_the_repository_cap(store: Store) {
    let h = harness(store.clone());
    repo(&store, "game").await;
    let svc = Arc::new(service(&store, &h, "game", None, Some(12)));
    let mut tasks = Vec::new();
    for i in 0..10 {
        let content = blob(&h, &format!("{i:04}")).await;
        let svc = svc.clone();
        tasks.push(tokio::spawn(async move {
            svc.checkin(
                &who("alice"),
                &path(&format!("Source/{i}")),
                content,
                None,
                "x".into(),
            )
            .await
        }));
    }
    let mut won = 0;
    for task in tasks {
        match task.await.unwrap() {
            Ok(_) => won += 1,
            Err(e) => assert!(matches!(e, PynError::RepoStorageLimitReached { .. }), "{e}"),
        }
    }
    assert_eq!(won, 3);
    assert_eq!(
        store
            .repo_usage(&crate::RepoId::new("game"))
            .await
            .unwrap()
            .stored_bytes,
        12
    );
}

pub async fn the_repository_cap_is_stored_and_cleared_with_the_record(store: Store) {
    repo(&store, "game").await;
    let id = crate::RepoId::new("game");
    assert_eq!(
        store.get_repo(&id).await.unwrap().unwrap().storage_cap,
        None
    );
    let set = crate::RepoUpdate {
        storage_cap: Some(Some(5 << 30)),
        ..Default::default()
    };
    let updated = store.update_repo(&id, set).await.unwrap();
    assert_eq!(updated.storage_cap, Some(5 << 30));
    let rename = crate::RepoUpdate {
        name: Some("engine".into()),
        ..Default::default()
    };
    let renamed = store.update_repo(&id, rename).await.unwrap();
    assert_eq!(
        renamed.storage_cap,
        Some(5 << 30),
        "other updates keep the cap"
    );
    let clear = crate::RepoUpdate {
        storage_cap: Some(None),
        ..Default::default()
    };
    assert_eq!(
        store.update_repo(&id, clear).await.unwrap().storage_cap,
        None
    );
}

pub async fn usage_is_broken_down_by_folder_and_path_counting_content_once(store: Store) {
    let h = harness(store.clone());
    repo(&store, "game").await;
    let svc = service(&store, &h, "game", None, None);
    let (a, b, c, d) = (
        path("Art/a.png"),
        path("Art/b.png"),
        path("Source/c.cpp"),
        path("README"),
    );
    let (big, small) = (blob(&h, "0123456789").await, blob(&h, "abc").await);
    svc.checkin(&who("alice"), &a, big, None, "a".into())
        .await
        .unwrap();
    svc.checkin(&who("alice"), &b, big, None, "same bytes".into())
        .await
        .unwrap();
    svc.checkin(&who("alice"), &c, small, None, "c".into())
        .await
        .unwrap();
    svc.checkin(&who("alice"), &c, big, Some(RevisionId(1)), "c2".into())
        .await
        .unwrap();
    svc.checkin(&who("alice"), &d, small, None, "d".into())
        .await
        .unwrap();

    let report = store
        .repo_breakdown(&crate::RepoId::new("game"))
        .await
        .unwrap();
    let folder = |name: &str, bytes, files| FolderUsage {
        folder: name.into(),
        stored_bytes: bytes,
        files,
    };
    assert_eq!(
        report.folders,
        vec![
            folder("Source", 13, 1),
            folder("Art", 10, 2),
            folder("", 3, 1)
        ]
    );
    let paths = |p: &str, bytes, revisions| PathUsage {
        path: p.into(),
        stored_bytes: bytes,
        revisions,
    };
    assert_eq!(
        report.largest_paths,
        vec![
            paths("Source/c.cpp", 13, 2),
            paths("Art/a.png", 10, 1),
            paths("Art/b.png", 10, 1),
            paths("README", 3, 1),
        ]
    );
    assert_eq!(
        store
            .repo_usage(&crate::RepoId::new("game"))
            .await
            .unwrap()
            .stored_bytes,
        13
    );
    assert!(
        store
            .repo_breakdown(&crate::RepoId::new("empty"))
            .await
            .unwrap()
            .folders
            .is_empty()
    );
}

pub async fn a_push_past_the_repository_cap_applies_nothing(store: Store) {
    use super::push::{Files, build, objects, set};

    let h = harness(store.clone());
    repo(&store, "game").await;
    let svc = service(&store, &h, "game", None, Some(10));
    let (six, other) = (blob(&h, "123456").await, blob(&h, "abcdef").await);
    let mut files = Files::new();
    set(&mut files, "Source/a", six);
    let first = build(&files, None, "fits", 1);
    set(&mut files, "Source/b", other);
    let second = build(&files, Some(first.id), "past the cap", 2);

    let err = svc
        .push_commits(&who("alice"), "main", objects(&[&first, &second]))
        .await
        .unwrap_err();
    assert!(
        matches!(err, PynError::RepoStorageLimitReached { .. }),
        "{err}"
    );
    assert!(svc.head(&path("Source/a")).await.unwrap().is_none());
    assert_eq!(svc.head_of("main").await.unwrap(), None);
    svc.push_commits(&who("alice"), "main", objects(&[&first]))
        .await
        .unwrap();
}
