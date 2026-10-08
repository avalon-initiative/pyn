use std::sync::Arc;

use pyn_core::{ContentHash, ObjectStore, PynError};
use pyn_fs::FsObjectStore;

async fn store() -> (FsObjectStore, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    (
        FsObjectStore::open(dir.path().join("objects"))
            .await
            .unwrap(),
        dir,
    )
}

#[tokio::test]
async fn put_then_get_round_trips() {
    let (s, _dir) = store().await;
    let hash = s.put(b"hello".to_vec()).await.unwrap();
    assert_eq!(
        hash.as_str(),
        "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
    );
    assert_eq!(s.get(&hash).await.unwrap().unwrap(), b"hello");
    assert!(s.exists(&hash).await.unwrap());
}

#[tokio::test]
async fn put_is_idempotent_and_survives_reopen() {
    let (s, dir) = store().await;
    let a = s.put(b"same".to_vec()).await.unwrap();
    let b = s.put(b"same".to_vec()).await.unwrap();
    assert_eq!(a, b);
    let reopened = FsObjectStore::open(dir.path().join("objects"))
        .await
        .unwrap();
    assert_eq!(reopened.get(&a).await.unwrap().unwrap(), b"same");
}

#[tokio::test]
async fn unknown_objects_are_absent() {
    let (s, _dir) = store().await;
    let missing = ContentHash::new("ab".repeat(32));
    assert!(!s.exists(&missing).await.unwrap());
    assert!(s.get(&missing).await.unwrap().is_none());
}

#[tokio::test]
async fn malformed_hashes_cannot_reach_the_filesystem() {
    let (s, _dir) = store().await;
    for bad in [
        "",
        "../../etc/passwd",
        "AB",
        &"g".repeat(64),
        &"A".repeat(64),
    ] {
        let err = s.exists(&ContentHash::new(bad)).await.unwrap_err();
        assert!(matches!(err, PynError::ObjectMissing(_)), "{bad:?}: {err}");
    }
}

#[tokio::test]
async fn a_tampered_object_is_reported_not_returned() {
    let (s, _dir) = store().await;
    let hash = s.put(b"original".to_vec()).await.unwrap();
    let path = s.root().join(&hash.as_str()[..2]).join(&hash.as_str()[2..]);
    std::fs::write(path, b"tampered").unwrap();
    assert!(matches!(s.get(&hash).await, Err(PynError::Storage(_))));
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_puts_of_the_same_content_all_succeed() {
    let (s, _dir) = store().await;
    let s = Arc::new(s);
    let tasks: Vec<_> = (0..16)
        .map(|_| {
            let s = s.clone();
            tokio::spawn(async move { s.put(vec![7u8; 4096]).await.unwrap() })
        })
        .collect();
    let mut hashes = Vec::new();
    for t in tasks {
        hashes.push(t.await.unwrap());
    }
    assert!(hashes.windows(2).all(|w| w[0] == w[1]));
    assert_eq!(s.get(&hashes[0]).await.unwrap().unwrap(), vec![7u8; 4096]);
}
