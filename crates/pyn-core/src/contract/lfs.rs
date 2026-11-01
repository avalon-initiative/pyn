//! LFS objects: recorded per repository by their sha256 oid, stored once, verified on the way in.

use std::sync::Arc;

use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

use super::{Harness, Store, harness, other_repo, repo_record, service_in, user};
use crate::PynError;
use crate::object::ObjectReader;

fn oid(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn body(bytes: &[u8]) -> ObjectReader {
    Box::pin(std::io::Cursor::new(bytes.to_vec()))
}

async fn put(h: &Harness, bytes: &[u8]) -> crate::Result<(crate::LfsObject, bool)> {
    h.svc
        .lfs_upload(
            &user("alice"),
            &oid(bytes),
            body(bytes),
            bytes.len() as u64,
            None,
        )
        .await
}

async fn read_all(h: &Harness, oid: &str) -> Vec<u8> {
    let (_, mut read) = h.svc.lfs_read(oid, None).await.unwrap();
    let mut out = Vec::new();
    read.reader.read_to_end(&mut out).await.unwrap();
    out
}

pub async fn an_lfs_object_is_recorded_once_and_read_back_by_its_oid(store: Store) {
    let h = harness(store.clone());
    let bytes = b"large file content";
    let (object, fresh) = put(&h, bytes).await.unwrap();
    assert!(fresh);
    assert_eq!(
        (object.oid.as_str(), object.size),
        (oid(bytes).as_str(), 18)
    );
    assert_eq!(object.created_by, user("alice"));
    assert_eq!(read_all(&h, &object.oid).await, bytes);
    assert_eq!(store.lfs_bytes(h.svc.repo()).await.unwrap(), 18);

    let (again, fresh) = put(&h, bytes).await.unwrap();
    assert!(!fresh, "an object the repository has is left alone");
    assert_eq!(again, object);
    assert_eq!(store.lfs_bytes(h.svc.repo()).await.unwrap(), 18);

    let err = h
        .svc
        .lfs_read(&oid(b"other"), None)
        .await
        .map(|_| ())
        .unwrap_err();
    assert!(matches!(err, PynError::ObjectNotFound(_)), "{err}");
}

pub async fn lfs_objects_belong_to_the_repository_that_uploaded_them(store: Store) {
    let h = harness(store);
    let other = other_repo(&h);
    let (object, _) = put(&h, b"mine").await.unwrap();
    assert!(other.svc.lfs_object(&object.oid).await.unwrap().is_none());
    assert!(other.svc.lfs_read(&object.oid, None).await.is_err());
    let (theirs, fresh) = put(&other, b"mine").await.unwrap();
    assert!(
        fresh,
        "the same bytes are a new object in another repository"
    );
    assert_eq!(theirs.object, object.object, "the bytes are stored once");
}

pub async fn an_lfs_upload_that_does_not_hash_to_its_oid_is_refused_and_kept_nowhere(store: Store) {
    let h = harness(store.clone());
    let wanted = oid(b"what was promised");
    let err = h
        .svc
        .lfs_upload(
            &user("alice"),
            &wanted,
            body(b"what arrived....."),
            17,
            None,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&err, PynError::DigestMismatch { expected, actual }
            if expected == &wanted && actual == &oid(b"what arrived.....")),
        "{err}"
    );
    assert_eq!(err.code(), "digest_mismatch");
    assert!(h.svc.lfs_object(&wanted).await.unwrap().is_none());
    assert_eq!(store.unreferenced_bytes(h.svc.repo()).await.unwrap(), 0);
    assert_eq!(store.lfs_bytes(h.svc.repo()).await.unwrap(), 0);
}

pub async fn an_lfs_upload_of_another_length_than_declared_is_refused(store: Store) {
    let h = harness(store.clone());
    let bytes = b"twelve bytes";
    for declared in [11u64, 13] {
        let err = h
            .svc
            .lfs_upload(&user("alice"), &oid(bytes), body(bytes), declared, None)
            .await
            .unwrap_err();
        assert!(matches!(err, PynError::LengthMismatch { .. }), "{err}");
    }
    assert!(h.svc.lfs_object(&oid(bytes)).await.unwrap().is_none());
    assert_eq!(store.unreferenced_bytes(h.svc.repo()).await.unwrap(), 0);

    put(&h, bytes).await.unwrap();
    let err = h
        .svc
        .lfs_upload(&user("alice"), &oid(bytes), body(bytes), 5, None)
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            PynError::LengthMismatch {
                declared: 5,
                actual: 12
            }
        ),
        "{err}"
    );
}

pub async fn an_lfs_oid_must_be_lowercase_sha256_hex(store: Store) {
    let h = harness(store);
    for bad in ["", "abc", &"A".repeat(64), &"g".repeat(64), &"a".repeat(65)] {
        let err = h
            .svc
            .lfs_upload(&user("alice"), bad, body(b"x"), 1, None)
            .await
            .unwrap_err();
        assert!(matches!(err, PynError::InvalidRequest(_)), "{bad}: {err}");
    }
}

pub async fn a_frozen_owner_cannot_upload_an_lfs_object(store: Store) {
    let h = harness(store);
    h.writes.freeze(&user("owner"), "plan lapsed");
    let err = put(&h, b"frozen").await.unwrap_err();
    assert!(matches!(err, PynError::OwnerFrozen { .. }), "{err}");
    assert!(h.svc.lfs_object(&oid(b"frozen")).await.unwrap().is_none());
    h.writes.unfreeze(&user("owner"));
    put(&h, b"frozen").await.unwrap();
}

struct OwnerCap(Option<u64>);

#[async_trait::async_trait]
impl crate::StorageCaps for OwnerCap {
    async fn storage_cap(&self, _owner: &crate::UserId) -> crate::Result<Option<u64>> {
        Ok(self.0)
    }
}

pub async fn lfs_objects_count_against_the_storage_caps(store: Store) {
    let h = harness(store.clone());
    store
        .create_repo(repo_record("lfs", "alice", "lfs"))
        .await
        .unwrap();
    let capped = |owner: Option<u64>, repo: Option<u64>| {
        service_in(&store, &h, "lfs")
            .with_storage_caps(user("alice"), Arc::new(OwnerCap(owner)))
            .with_repo_storage_cap("alice/lfs".into(), repo)
    };
    let up = |svc: crate::RepoService, bytes: &'static [u8]| async move {
        svc.lfs_upload(
            &user("alice"),
            &oid(bytes),
            body(bytes),
            bytes.len() as u64,
            None,
        )
        .await
    };

    up(capped(None, Some(10)), b"123456").await.unwrap();
    let err = up(capped(None, Some(10)), b"abcdef").await.unwrap_err();
    assert!(
        matches!(
            &err,
            PynError::RepoStorageLimitReached {
                limit: 10,
                used: 6,
                ..
            }
        ),
        "{err}"
    );
    let err = up(capped(Some(8), None), b"abcdef").await.unwrap_err();
    assert!(
        matches!(
            &err,
            PynError::StorageLimitReached {
                limit: 8,
                used: 6,
                ..
            }
        ),
        "{err}"
    );
    up(capped(Some(12), Some(12)), b"abcdef").await.unwrap();
}
