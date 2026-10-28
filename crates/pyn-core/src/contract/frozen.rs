//! A frozen owner (decision #429), through `RepoService`: every write is refused with `owner_frozen` and leaves nothing
//! behind, reads and releasing locks continue, and unfreezing restores writes.

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use chrono::Duration;
use pyn_object::{Algorithm, Kind, hash_object};
use tokio::io::{AsyncRead, ReadBuf};

use super::push::{Files, build, objects, set};
use super::{Harness, Store, blob, harness, path, put_on, user, who};
use crate::audit::AuditQuery;
use crate::merge::{FastForward, MergeRequest};
use crate::object::{Keying, ObjectStore};
use crate::{ChangeEntry, Deletion, FileMode, MemoryOwnerWrites, PynError, RepoPath, RevisionId};

const OWNER: &str = "owner";

fn refused<T: std::fmt::Debug>(result: Result<T, PynError>) {
    let err = result.unwrap_err();
    assert!(
        matches!(&err, PynError::OwnerFrozen { owner, .. } if owner == OWNER),
        "{err}"
    );
    assert_eq!(err.code(), "owner_frozen");
}

async fn audit_rows(h: &Harness) -> usize {
    h.svc.audit(&AuditQuery::default()).await.unwrap().len()
}

pub async fn a_frozen_owner_cannot_take_or_renew_a_lock_but_can_release_one(store: Store) {
    let h = harness(store);
    let (kept, dropped, free) = (
        path("Content/kept.umap"),
        path("Content/dropped.umap"),
        path("Content/free.umap"),
    );
    h.svc.checkout(&kept, &user("alice"), None).await.unwrap();
    h.svc
        .checkout(&dropped, &user("alice"), None)
        .await
        .unwrap();
    let before = h.svc.locks().await.unwrap();
    h.writes.freeze(&user(OWNER), "plan lapsed");

    refused(h.svc.checkout(&kept, &user("alice"), None).await);
    refused(h.svc.checkout(&free, &user("bob"), None).await);
    assert_eq!(
        h.svc.locks().await.unwrap(),
        before,
        "a refusal changes no lock"
    );

    h.svc.release(&dropped, &user("alice")).await.unwrap();
    let left = h.svc.locks().await.unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].path, kept);

    h.svc
        .force_unlock(&kept, &user("root"), "owner left")
        .await
        .unwrap();
    assert!(h.svc.locks().await.unwrap().is_empty());
    refused(h.svc.checkout(&kept, &user("alice"), None).await);

    h.writes.unfreeze(&user(OWNER));
    h.svc.checkout(&kept, &user("alice"), None).await.unwrap();
    h.clock.advance(Duration::hours(9));
    assert!(
        h.svc.locks().await.unwrap().is_empty(),
        "a lease still lapses"
    );
}

pub async fn a_frozen_owner_refuses_every_write_and_leaves_nothing_behind(store: Store) {
    let h = harness(store);
    let (src, locked) = (path("Source/a.cpp"), path("Content/r.umap"));
    let first = put_on(&h, "main", "alice", &src, "one", None).await;
    h.svc
        .create_branch(&who("alice"), "feat", None)
        .await
        .unwrap();
    h.svc
        .create_branch(&who("alice"), "old", None)
        .await
        .unwrap();
    h.svc.checkout(&locked, &user("alice"), None).await.unwrap();
    let r1 = put_on(&h, "main", "alice", &locked, "r1", None).await;
    h.svc
        .checkout(&locked, &user("alice"), Some(r1.id))
        .await
        .unwrap();
    let r2 = put_on(&h, "main", "alice", &locked, "r2", Some(r1.id.0)).await;
    h.svc
        .checkout(&locked, &user("alice"), Some(r2.id))
        .await
        .unwrap();
    let chunk = blob(&h, "chunk").await;
    let extra = blob(&h, "extra").await;
    let head = h.svc.head_commit().await.unwrap();
    let branches = h.svc.branches().await.unwrap();
    let locks = h.svc.locks().await.unwrap();
    let revisions = h.svc.history(&src).await.unwrap().len();
    let rows = audit_rows(&h).await;

    h.writes.freeze(&user(OWNER), "abuse report");
    let alice = who("alice");

    refused(
        h.svc
            .checkin(&alice, &src, extra, Some(first.id), "m".into())
            .await,
    );
    refused(
        h.svc
            .checkin(&alice, &path(".pyn/pyn.toml"), extra, None, "policy".into())
            .await,
    );
    refused(
        h.svc
            .checkin_changeset(
                &alice,
                vec![ChangeEntry {
                    path: path("Source/b.cpp"),
                    content: extra,
                    file_mode: FileMode::Regular,
                    base: None,
                }],
                "set".into(),
            )
            .await,
    );
    refused(
        h.svc
            .checkin_changes_on(
                "main",
                &alice,
                Vec::new(),
                vec![Deletion {
                    path: src.clone(),
                    base: Some(first.id),
                }],
                "rm".into(),
            )
            .await,
    );
    refused(
        h.svc
            .restore(
                &alice,
                &locked,
                r1.id,
                r2.id,
                &format!("{locked}@r{}", r2.id),
                None,
            )
            .await,
    );

    let mut files = Files::new();
    set(&mut files, "Source/z.cpp", extra);
    let pushed = build(&files, None, "pushed", 9);
    refused(
        h.svc
            .push_commits(&alice, "side", objects(&[&pushed]))
            .await,
    );

    refused(h.svc.create_branch(&alice, "new", None).await);
    refused(h.svc.rename_branch(&alice, "old", "older").await);
    refused(h.svc.delete_branch(&alice, "old", true).await);
    refused(
        h.svc
            .merge(
                &alice,
                MergeRequest {
                    source: "feat".into(),
                    target: Some("main".into()),
                    fast_forward: FastForward::Allow,
                    message: None,
                    expect_target: None,
                    resolutions: Vec::new(),
                },
            )
            .await,
    );
    refused(h.svc.upload_blob(b"more".to_vec()).await);
    refused(h.svc.assemble(&[chunk], None).await);

    assert_eq!(h.svc.head_commit().await.unwrap(), head);
    assert_eq!(h.svc.branches().await.unwrap(), branches);
    assert_eq!(h.svc.locks().await.unwrap(), locks);
    assert_eq!(h.svc.history(&src).await.unwrap().len(), revisions);
    assert_eq!(
        audit_rows(&h).await,
        rows,
        "a refusal is not an audited change"
    );
    assert!(h.svc.get_commit(&pushed.id).await.is_err());
    let more = hash_object(Algorithm::Sha256, Kind::Blob, b"more").unwrap();
    assert!(!h.objects.exists(&more).await.unwrap());
}

pub async fn a_frozen_owner_stays_readable_and_unfreezing_restores_writes(store: Store) {
    let h = harness(store);
    let src: RepoPath = path("Source/a.cpp");
    let first = put_on(&h, "main", "alice", &src, "one", None).await;
    h.writes.freeze(&user(OWNER), "plan lapsed");

    assert_eq!(h.svc.files(None, 10).await.unwrap().len(), 1);
    assert_eq!(h.svc.tree(None).await.unwrap().len(), 1);
    assert_eq!(h.svc.history(&src).await.unwrap().len(), 1);
    assert_eq!(h.svc.snapshot_on("main").await.unwrap().entries.len(), 1);
    assert!(h.svc.read(&src, None).await.is_ok());
    assert_eq!(
        h.svc.commits(None, Some(10)).await.unwrap().commits.len(),
        1
    );
    assert!(h.svc.summary(5).await.is_ok());
    assert!(h.svc.verify(None, true).await.unwrap().problems.is_empty());
    assert!(h.svc.audit(&AuditQuery::default()).await.is_ok());

    h.writes.unfreeze(&user(OWNER));
    let extra = blob(&h, "two").await;
    let next = h
        .svc
        .checkin(&who("alice"), &src, extra, Some(first.id), "two".into())
        .await
        .unwrap();
    assert_eq!(next.id, RevisionId(first.id.0 + 1));
}

/// Freezes the owner when the upload first reads its bytes.
struct FreezeOnRead {
    inner: std::io::Cursor<Vec<u8>>,
    writes: Arc<MemoryOwnerWrites>,
}

impl AsyncRead for FreezeOnRead {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        self.writes.freeze(&user(OWNER), "frozen mid-request");
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

pub async fn a_freeze_that_lands_during_an_upload_refuses_it_and_keeps_no_object(store: Store) {
    let h = harness(store);
    let bytes = b"in flight".to_vec();
    let len = bytes.len() as u64;
    let reader = Box::pin(FreezeOnRead {
        inner: std::io::Cursor::new(bytes.clone()),
        writes: h.writes.clone(),
    });
    refused(h.svc.upload(reader, Keying::Blob { len }, None, len).await);
    let id = hash_object(Algorithm::Sha256, Kind::Blob, &bytes).unwrap();
    assert!(!h.objects.exists(&id).await.unwrap());
    assert_eq!(h.store.unreferenced_bytes(h.svc.repo()).await.unwrap(), 0);
}
