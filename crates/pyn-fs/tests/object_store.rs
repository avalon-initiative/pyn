use std::sync::Arc;

use pyn_core::{Keying, ObjectId, ObjectStore, PynError, REPO_ALGORITHM};
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
        hash.to_hex(),
        "8aec4e4876f854f688d0ebfc8f37598f38e5fd6903cccc850ca36591175aeb60"
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
    let missing = ObjectId::from_hex(REPO_ALGORITHM, &"ab".repeat(32)).unwrap();
    assert!(!s.exists(&missing).await.unwrap());
    assert!(s.get(&missing).await.unwrap().is_none());
}

#[tokio::test]
async fn only_sha256_ids_are_stored() {
    let (s, _dir) = store().await;
    let sha1 = ObjectId::from_hex(pyn_object::Algorithm::Sha1, &"ab".repeat(20)).unwrap();
    let err = s.exists(&sha1).await.unwrap_err();
    assert!(matches!(err, PynError::ObjectMissing(_)), "{err}");
}

#[tokio::test]
async fn a_chunk_is_keyed_by_the_raw_sha256_of_its_bytes() {
    let (s, _dir) = store().await;
    let raw = s.put_chunk(b"hello".to_vec()).await.unwrap();
    assert_eq!(
        raw.to_hex(),
        "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
    );
    assert_ne!(raw, s.put(b"hello".to_vec()).await.unwrap());
}

#[tokio::test]
async fn a_body_that_is_not_the_declared_length_is_refused() {
    let (s, dir) = store().await;
    for (len, body) in [(4u64, &b"hello"[..]), (6, &b"hello"[..])] {
        let err = s
            .put_stream(
                Box::pin(std::io::Cursor::new(body.to_vec())),
                Keying::Blob { len },
                None,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, PynError::LengthMismatch { .. }), "{err}");
    }
    let stored = std::fs::read_dir(dir.path().join("objects"))
        .unwrap()
        .count();
    assert_eq!(stored, 0, "nothing is left behind");
}

#[tokio::test]
async fn a_tampered_object_is_reported_not_returned() {
    let (s, _dir) = store().await;
    let hash = s.put(b"original".to_vec()).await.unwrap();
    let path = s.root().join(&hash.to_hex()[..2]).join(&hash.to_hex()[2..]);
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
        .put_stream(
            Box::pin(std::io::Cursor::new(data.clone())),
            Keying::Blob { len: 200_000 },
            None,
        )
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
        .put_stream(
            Box::pin(std::io::Cursor::new(vec![7u8; 1000])),
            Keying::Blob { len: 1000 },
            Some(999),
        )
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

fn files_named(dir: &std::path::Path, suffix: &str) -> usize {
    let mut n = 0;
    for shard in std::fs::read_dir(dir.join("objects")).unwrap().flatten() {
        if shard.path().is_dir() {
            n += std::fs::read_dir(shard.path())
                .unwrap()
                .flatten()
                .filter(|f| f.file_name().to_string_lossy().ends_with(suffix))
                .count();
        }
    }
    n
}

#[tokio::test]
async fn compressible_objects_are_stored_compressed_and_read_back_whole_or_in_ranges() {
    let (s, dir) = store().await;
    let text: Vec<u8> = "the quick brown fox jumps over the lazy dog\n"
        .repeat(5000)
        .into_bytes();
    let hash = s.put(text.clone()).await.unwrap();
    assert_eq!(files_named(dir.path(), ".zst"), 1);
    let on_disk: u64 = std::fs::read_dir(dir.path().join("objects"))
        .unwrap()
        .flatten()
        .filter(|e| e.path().is_dir())
        .flat_map(|d| std::fs::read_dir(d.path()).unwrap().flatten())
        .map(|f| f.metadata().unwrap().len())
        .sum();
    assert!(on_disk < text.len() as u64 / 10, "{on_disk}");

    assert_eq!(s.size(&hash).await.unwrap(), Some(text.len() as u64));
    assert!(s.exists(&hash).await.unwrap());
    assert_eq!(s.get(&hash).await.unwrap().unwrap(), text);
    assert_eq!(s.put(text.clone()).await.unwrap(), hash, "idempotent");
    assert_eq!(files_named(dir.path(), ".zst"), 1);

    let mut part = s.read(&hash, Some(1000..1100)).await.unwrap().unwrap();
    assert_eq!(
        (part.total, part.range.clone()),
        (text.len() as u64, 1000..1100)
    );
    let mut got = Vec::new();
    tokio::io::AsyncReadExt::read_to_end(&mut part.reader, &mut got)
        .await
        .unwrap();
    assert_eq!(got, &text[1000..1100]);

    assert!(s.delete(&hash).await.unwrap());
    assert!(!s.exists(&hash).await.unwrap());
    assert_eq!(files_named(dir.path(), ".zst"), 0);
}

#[tokio::test]
async fn incompressible_and_tiny_objects_stay_raw() {
    let (s, dir) = store().await;
    let mut x = 0x2545F4914F6CDD1Du64;
    let noise: Vec<u8> = (0..100_000)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect();
    let a = s.put(noise.clone()).await.unwrap();
    let b = s.put(b"tiny".to_vec()).await.unwrap();
    assert_eq!(files_named(dir.path(), ".zst"), 0);
    assert_eq!(s.get(&a).await.unwrap().unwrap(), noise);
    assert_eq!(s.get(&b).await.unwrap().unwrap(), b"tiny");
}

#[tokio::test]
async fn a_damaged_compressed_object_is_an_error_not_a_short_read() {
    let (s, dir) = store().await;
    let text = "abc ".repeat(20_000).into_bytes();
    let hash = s.put(text).await.unwrap();
    let packed = std::fs::read_dir(dir.path().join("objects"))
        .unwrap()
        .flatten()
        .filter(|e| e.path().is_dir())
        .flat_map(|d| std::fs::read_dir(d.path()).unwrap().flatten())
        .find(|f| f.file_name().to_string_lossy().ends_with(".zst"))
        .unwrap()
        .path();
    let mut bytes = std::fs::read(&packed).unwrap();
    let keep = bytes.len() / 2;
    bytes.truncate(keep);
    std::fs::write(&packed, bytes).unwrap();
    assert!(s.get(&hash).await.is_err());
}

#[tokio::test]
async fn chunked_objects_on_disk_share_chunks_compress_and_delete() {
    let (fs, dir) = store().await;
    let s = pyn_core::ChunkedStore::new(Arc::new(fs));
    let text: Vec<u8> = "line of source text that repeats\n"
        .repeat(200_000)
        .into_bytes();
    let hash = s.put(text.clone()).await.unwrap();
    assert!(files_named(dir.path(), ".cm") == 1);
    assert!(
        files_named(dir.path(), ".zst") >= 1,
        "chunks are compressed too"
    );
    assert_eq!(s.size(&hash).await.unwrap(), Some(text.len() as u64));
    assert_eq!(s.get(&hash).await.unwrap().unwrap(), text);
    let mut part = s
        .read(&hash, Some(1_500_000..1_500_100))
        .await
        .unwrap()
        .unwrap();
    let mut got = Vec::new();
    tokio::io::AsyncReadExt::read_to_end(&mut part.reader, &mut got)
        .await
        .unwrap();
    assert_eq!(got, &text[1_500_000..1_500_100]);
    assert!(s.delete(&hash).await.unwrap());
    assert!(!s.exists(&hash).await.unwrap());
    assert_eq!(files_named(dir.path(), ".cm"), 0);
}
