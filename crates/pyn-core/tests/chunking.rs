use std::io::Cursor;
use std::sync::Arc;

use pyn_core::chunking::{Manifest, SMALL_OBJECT};
use pyn_core::memory::MemoryObjectStore;
use pyn_core::{ChunkedStore, ObjectStore, PynError};
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

fn stores() -> (ChunkedStore, Arc<MemoryObjectStore>) {
    let inner = Arc::new(MemoryObjectStore::new());
    (ChunkedStore::new(inner.clone()), inner)
}

async fn manifest_of(inner: &MemoryObjectStore, key: &pyn_core::ContentHash) -> Manifest {
    Manifest::decode(&inner.get_manifest(key).await.unwrap().unwrap()).unwrap()
}

#[tokio::test]
async fn small_objects_are_one_blob_with_no_manifest() {
    let (s, inner) = stores();
    let data = noise(SMALL_OBJECT, 1);
    let hash = s.put(data.clone()).await.unwrap();
    assert!(inner.get_manifest(&hash).await.unwrap().is_none());
    assert_eq!(s.get(&hash).await.unwrap().unwrap(), data);
}

#[tokio::test]
async fn large_objects_are_chunked_and_read_back_whole_and_in_ranges() {
    let (s, inner) = stores();
    let data = noise(9_000_000, 7);
    let hash = s
        .put_stream(Box::pin(Cursor::new(data.clone())), None)
        .await
        .unwrap();
    assert_eq!(hash.as_str(), hex::encode(Sha256::digest(&data)));

    let m = manifest_of(&inner, &hash).await;
    assert_eq!(m.total, data.len() as u64);
    assert!(m.chunks.len() >= 3, "{}", m.chunks.len());
    let (last, rest) = m.chunks.split_last().unwrap();
    assert!(
        rest.iter()
            .all(|(_, len)| (256 << 10..=4 << 20).contains(&(*len as usize)))
    );
    assert!(last.1 as usize <= 4 << 20);
    assert_eq!(s.size(&hash).await.unwrap(), Some(data.len() as u64));
    assert!(s.exists(&hash).await.unwrap());
    assert_eq!(s.get(&hash).await.unwrap().unwrap(), data);

    let boundary = m.chunks[0].1;
    for range in [
        100..200,
        boundary - 10..boundary + 10,
        boundary - 10..m.chunks[0].1 + m.chunks[1].1 + 5,
        8_999_990..9_100_000,
    ] {
        let mut part = s.read(&hash, Some(range.clone())).await.unwrap().unwrap();
        let end = range.end.min(data.len() as u64);
        let mut got = Vec::new();
        part.reader.read_to_end(&mut got).await.unwrap();
        assert_eq!(got, &data[range.start as usize..end as usize], "{range:?}");
    }
}

#[tokio::test]
async fn an_insertion_leaves_most_chunks_shared() {
    let (s, inner) = stores();
    let base = noise(12_000_000, 3);
    let a = s.put(base.clone()).await.unwrap();
    let mut edited = b"inserted at the front".to_vec();
    edited.extend_from_slice(&base);
    let b = s.put(edited).await.unwrap();
    let (ma, mb) = (manifest_of(&inner, &a).await, manifest_of(&inner, &b).await);
    let shared = mb
        .chunks
        .iter()
        .filter(|c| ma.chunks.iter().any(|d| d.0 == c.0))
        .count();
    assert!(
        shared * 10 >= mb.chunks.len() * 7,
        "{shared} of {}",
        mb.chunks.len()
    );
}

#[tokio::test]
async fn the_limit_applies_to_chunked_streams() {
    let (s, _) = stores();
    let err = s
        .put_stream(Box::pin(Cursor::new(noise(5_000_000, 9))), Some(4_000_000))
        .await
        .unwrap_err();
    assert!(
        matches!(err, PynError::ObjectTooLarge { limit: 4_000_000 }),
        "{err}"
    );
}

#[tokio::test]
async fn a_missing_chunk_is_a_read_error() {
    let (s, inner) = stores();
    let data = noise(5_000_000, 11);
    let hash = s.put(data).await.unwrap();
    let m = manifest_of(&inner, &hash).await;
    inner.delete(&m.chunks[1].0).await.unwrap();
    assert!(s.get(&hash).await.is_err());
    assert_eq!(s.chunks_of(&hash).await.unwrap().len(), m.chunks.len());
}

#[test]
fn manifests_round_trip_and_reject_damage() {
    let m = Manifest {
        total: 7,
        chunks: vec![
            (pyn_core::ContentHash::new("ab"), 3),
            (pyn_core::ContentHash::new("cd"), 4),
        ],
    };
    assert_eq!(Manifest::decode(&m.encode()).unwrap(), m);
    assert!(Manifest::decode(b"garbage").is_err());
    assert!(
        Manifest::decode(b"pyn-manifest 1\n9\nab 3\n").is_err(),
        "sizes must add up"
    );
}
