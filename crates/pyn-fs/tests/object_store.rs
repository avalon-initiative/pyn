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

#[tokio::test]
async fn streams_in_ranges_and_deletes() {
    let (s, dir) = store().await;
    let data: Vec<u8> = (0..200_000u32).map(|i| (i % 253) as u8).collect();
    let hash = s
        .put_stream(Box::pin(std::io::Cursor::new(data.clone())), None)
        .await
        .unwrap();
    assert_eq!(hash, s.put(data.clone()).await.unwrap());
    assert_eq!(s.size(&hash).await.unwrap(), Some(200_000));

    let mut part = s.read(&hash, Some(100..110)).await.unwrap().unwrap();
    assert_eq!((part.total, part.range.clone()), (200_000, 100..110));
    let mut got = Vec::new();
    tokio::io::AsyncReadExt::read_to_end(&mut part.reader, &mut got)
        .await
        .unwrap();
    assert_eq!(got, &data[100..110]);

    let clamped = s
        .read(&hash, Some(199_990..999_999))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(clamped.range, 199_990..200_000);

    assert!(s.delete(&hash).await.unwrap());
    assert!(!s.delete(&hash).await.unwrap());
    assert!(s.read(&hash, None).await.unwrap().is_none());
    let leftovers: Vec<_> = std::fs::read_dir(dir.path().join("objects"))
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty());
}

#[tokio::test]
async fn an_oversized_stream_is_refused_and_leaves_nothing() {
    let (s, dir) = store().await;
    let err = s
        .put_stream(Box::pin(std::io::Cursor::new(vec![7u8; 1000])), Some(999))
        .await
        .unwrap_err();
    assert!(
        matches!(err, PynError::ObjectTooLarge { limit: 999 }),
        "{err}"
    );
    let entries = std::fs::read_dir(dir.path().join("objects"))
        .unwrap()
        .count();
    assert_eq!(entries, 0);
}
