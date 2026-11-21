//! Behaviour every `ObjectStore` must satisfy, run against the store passed in.

use std::sync::Arc;

use pyn_object::{Kind, hash_object};
use tokio::io::AsyncReadExt;

use crate::{ChunkedStore, Keying, ObjectId, ObjectStore, PynError, REPO_ALGORITHM};

pub type Objects = Arc<dyn ObjectStore>;

const HELLO_BLOB: &str = "8aec4e4876f854f688d0ebfc8f37598f38e5fd6903cccc850ca36591175aeb60";
const HELLO_RAW: &str = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";

fn bytes_of(len: usize, seed: u64) -> Vec<u8> {
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

fn stream(bytes: &[u8]) -> crate::ObjectReader {
    Box::pin(std::io::Cursor::new(bytes.to_vec()))
}

fn blob_id(bytes: &[u8]) -> ObjectId {
    hash_object(REPO_ALGORITHM, Kind::Blob, bytes).unwrap()
}

fn unknown() -> ObjectId {
    ObjectId::from_hex(REPO_ALGORITHM, &"ab".repeat(32)).unwrap()
}

async fn put_blob(store: &Objects, bytes: &[u8]) -> crate::Stored {
    let len = bytes.len() as u64;
    store
        .put_stream_tracked(stream(bytes), Keying::Blob { len }, None)
        .await
        .unwrap()
}

async fn read_range(
    store: &Objects,
    id: &ObjectId,
    range: Option<std::ops::Range<u64>>,
) -> (u64, std::ops::Range<u64>, Vec<u8>) {
    let found = store.read(id, range).await.unwrap().expect("stored");
    let mut got = Vec::new();
    let mut reader = found.reader;
    reader.read_to_end(&mut got).await.unwrap();
    (found.total, found.range, got)
}

pub async fn store_round_trips_a_blob_and_a_chunk_under_their_keys(store: Objects) {
    let id = store.put(b"hello".to_vec()).await.unwrap();
    assert_eq!(id.to_hex(), HELLO_BLOB);
    assert_eq!(store.get(&id).await.unwrap().unwrap(), b"hello");
    let raw = store.put_chunk(b"hello".to_vec()).await.unwrap();
    assert_eq!(raw.to_hex(), HELLO_RAW);
    assert_ne!(raw, id);
    assert_eq!(store.get_limited(&id, 5).await.unwrap().unwrap(), b"hello");
    assert!(matches!(
        store.get_limited(&id, 4).await,
        Err(PynError::ObjectTooLarge { limit: 4 })
    ));
}

pub async fn store_holds_the_empty_object(store: Objects) {
    let stored = put_blob(&store, b"").await;
    assert_eq!(store.size(&stored.id).await.unwrap(), Some(0));
    assert_eq!(store.get(&stored.id).await.unwrap().unwrap(), b"");
    let (total, range, got) = read_range(&store, &stored.id, Some(0..10)).await;
    assert_eq!((total, range, got.len()), (0, 0..0, 0));
}

pub async fn store_put_is_idempotent_and_reports_what_it_created(store: Objects) {
    let first = put_blob(&store, b"same bytes").await;
    let second = put_blob(&store, b"same bytes").await;
    assert_eq!(first.id, second.id);
    assert!(second.created.is_empty());
    assert!(first.created.is_empty() || first.created == vec![first.id]);
    assert_eq!(store.get(&first.id).await.unwrap().unwrap(), b"same bytes");
}

pub async fn store_reads_ranges_and_clamps_them(store: Objects) {
    let data = bytes_of(10_000, 7);
    let id = put_blob(&store, &data).await.id;

    let (total, range, got) = read_range(&store, &id, None).await;
    assert_eq!((total, range), (10_000, 0..10_000));
    assert_eq!(got, data);

    let (total, range, got) = read_range(&store, &id, Some(100..110)).await;
    assert_eq!((total, range), (10_000, 100..110));
    assert_eq!(got, &data[100..110]);

    let (_, range, got) = read_range(&store, &id, Some(9_990..99_999)).await;
    assert_eq!(range, 9_990..10_000);
    assert_eq!(got, &data[9_990..]);

    let (total, range, got) = read_range(&store, &id, Some(10_000..10_010)).await;
    assert_eq!((total, range, got.len()), (10_000, 10_000..10_000, 0));

    let (total, range, got) = read_range(&store, &id, Some(50_000..60_000)).await;
    assert_eq!((total, range, got.len()), (10_000, 10_000..10_000, 0));

    let (total, range, got) = read_range(&store, &id, Some(500..500)).await;
    assert_eq!((total, range, got.len()), (10_000, 500..500, 0));

    let (_, range, got) = read_range(&store, &id, Some(0..1)).await;
    assert_eq!((range, got), (0..1, vec![data[0]]));
}

pub async fn store_reports_absent_objects_as_absent(store: Objects) {
    let missing = unknown();
    assert!(!store.exists(&missing).await.unwrap());
    assert_eq!(store.size(&missing).await.unwrap(), None);
    assert!(store.read(&missing, None).await.unwrap().is_none());
    assert!(store.read(&missing, Some(0..4)).await.unwrap().is_none());
    assert!(store.get(&missing).await.unwrap().is_none());
    assert!(store.get_manifest(&missing).await.unwrap().is_none());
    assert!(!store.delete(&missing).await.unwrap());
}

pub async fn store_exists_and_size_follow_the_object(store: Objects) {
    let data = bytes_of(1234, 3);
    let id = put_blob(&store, &data).await.id;
    assert!(store.exists(&id).await.unwrap());
    assert_eq!(store.size(&id).await.unwrap(), Some(1234));
    assert!(store.delete(&id).await.unwrap());
    assert!(!store.exists(&id).await.unwrap());
    assert_eq!(store.size(&id).await.unwrap(), None);
}

pub async fn store_delete_removes_the_object_once(store: Objects) {
    let id = store.put(b"to delete".to_vec()).await.unwrap();
    assert!(store.delete(&id).await.unwrap());
    assert!(!store.delete(&id).await.unwrap());
    assert!(store.read(&id, None).await.unwrap().is_none());
    assert_eq!(store.put(b"to delete".to_vec()).await.unwrap(), id);
    assert!(store.exists(&id).await.unwrap());
}

pub async fn store_refuses_an_oversized_stream_and_leaves_nothing(store: Objects) {
    let data = bytes_of(1000, 11);
    let err = store
        .put_stream(stream(&data), Keying::Blob { len: 1000 }, Some(999))
        .await
        .unwrap_err();
    assert!(
        matches!(err, PynError::ObjectTooLarge { limit: 999 }),
        "{err}"
    );
    assert!(!store.exists(&blob_id(&data)).await.unwrap());

    let err = store
        .put_stream(stream(&data), Keying::Raw, Some(10))
        .await
        .unwrap_err();
    assert!(
        matches!(err, PynError::ObjectTooLarge { limit: 10 }),
        "{err}"
    );
    let ok = store
        .put_stream(stream(&data), Keying::Raw, Some(1000))
        .await
        .unwrap();
    assert!(store.exists(&ok).await.unwrap());
}

pub async fn store_refuses_a_stream_that_is_not_the_declared_length(store: Objects) {
    for len in [4u64, 6] {
        let err = store
            .put_stream(stream(b"hello"), Keying::Blob { len }, None)
            .await
            .unwrap_err();
        assert!(matches!(err, PynError::LengthMismatch { .. }), "{err}");
    }
    assert!(!store.exists(&blob_id(b"hello")).await.unwrap());
}

pub async fn store_keeps_and_deletes_manifests_with_the_object_key(store: Objects) {
    let key = unknown();
    assert!(store.get_manifest(&key).await.unwrap().is_none());
    store.put_manifest(&key, b"first".to_vec()).await.unwrap();
    assert_eq!(store.get_manifest(&key).await.unwrap().unwrap(), b"first");
    store.put_manifest(&key, b"second".to_vec()).await.unwrap();
    assert_eq!(store.get_manifest(&key).await.unwrap().unwrap(), b"second");
    assert!(store.exists(&key).await.unwrap());
    assert_eq!(store.size(&key).await.unwrap(), None);
    assert!(store.delete(&key).await.unwrap());
    assert!(store.get_manifest(&key).await.unwrap().is_none());
    assert!(!store.exists(&key).await.unwrap());
}

pub async fn store_handles_objects_across_the_sizes_it_is_given(store: Objects) {
    for (len, seed) in [
        (1usize, 1u64),
        (65_535, 2),
        (65_536, 3),
        (65_537, 4),
        (1 << 20, 5),
        ((1 << 20) + 1, 6),
    ] {
        let data = bytes_of(len, seed);
        let id = put_blob(&store, &data).await.id;
        assert_eq!(store.size(&id).await.unwrap(), Some(len as u64));
        assert_eq!(store.get(&id).await.unwrap().unwrap(), data, "{len}");
    }
    for (len, seed) in [(4 << 20, 8u64), (9 << 20, 9)] {
        let data = bytes_of(len, seed);
        let id = store.put_chunk(data.clone()).await.unwrap();
        let (total, _, got) = read_range(&store, &id, None).await;
        assert_eq!(total, len as u64);
        assert!(got == data, "{len}");
        let mid = len as u64 / 2;
        let (_, _, part) = read_range(&store, &id, Some(mid..mid + 1000)).await;
        assert_eq!(part, &data[mid as usize..mid as usize + 1000]);
    }
}

pub async fn store_serves_concurrent_puts_of_the_same_content(store: Objects) {
    let data = Arc::new(bytes_of(100_000, 13));
    let tasks: Vec<_> = (0..8)
        .map(|_| {
            let (store, data) = (store.clone(), data.clone());
            tokio::spawn(async move { store.put(data.to_vec()).await.unwrap() })
        })
        .collect();
    let mut ids = Vec::new();
    for t in tasks {
        ids.push(t.await.unwrap());
    }
    assert!(ids.windows(2).all(|w| w[0] == w[1]));
    assert_eq!(store.get(&ids[0]).await.unwrap().unwrap(), *data);
}

pub async fn store_serves_chunked_objects_through_chunked_store(store: Objects) {
    let chunked = ChunkedStore::new(store.clone());
    let data = bytes_of(6 << 20, 21);
    let id = chunked.put(data.clone()).await.unwrap();
    assert!(chunked.exists(&id).await.unwrap());
    assert_eq!(chunked.size(&id).await.unwrap(), Some(data.len() as u64));
    assert_eq!(chunked.get(&id).await.unwrap().unwrap(), data);
    let found = chunked
        .read(&id, Some(3_000_000..3_000_100))
        .await
        .unwrap()
        .unwrap();
    let mut got = Vec::new();
    let mut reader = found.reader;
    reader.read_to_end(&mut got).await.unwrap();
    assert_eq!(got, &data[3_000_000..3_000_100]);
    let chunks = chunked.chunks_of(&id).await.unwrap();
    assert!(chunks.len() > 1);
    assert!(store.exists(&chunks[0]).await.unwrap());
    assert!(chunked.delete(&id).await.unwrap());
    assert!(!chunked.exists(&id).await.unwrap());
    assert!(chunked.get(&id).await.unwrap().is_none());
}

/// Generates one `#[tokio::test]` per object-store case. `$factory` is an async expression yielding the store as an
/// `Arc<dyn ObjectStore>` together with a value kept alive for the test (a server, a temporary directory).
#[macro_export]
macro_rules! object_store_contract_tests {
    ($factory:expr $(, #[$attr:meta])*) => {
        $crate::object_store_contract_tests!(@one $factory; [$(#[$attr])*]; store_round_trips_a_blob_and_a_chunk_under_their_keys);
        $crate::object_store_contract_tests!(@one $factory; [$(#[$attr])*]; store_holds_the_empty_object);
        $crate::object_store_contract_tests!(@one $factory; [$(#[$attr])*]; store_put_is_idempotent_and_reports_what_it_created);
        $crate::object_store_contract_tests!(@one $factory; [$(#[$attr])*]; store_reads_ranges_and_clamps_them);
        $crate::object_store_contract_tests!(@one $factory; [$(#[$attr])*]; store_reports_absent_objects_as_absent);
        $crate::object_store_contract_tests!(@one $factory; [$(#[$attr])*]; store_exists_and_size_follow_the_object);
        $crate::object_store_contract_tests!(@one $factory; [$(#[$attr])*]; store_delete_removes_the_object_once);
        $crate::object_store_contract_tests!(@one $factory; [$(#[$attr])*]; store_refuses_an_oversized_stream_and_leaves_nothing);
        $crate::object_store_contract_tests!(@one $factory; [$(#[$attr])*]; store_refuses_a_stream_that_is_not_the_declared_length);
        $crate::object_store_contract_tests!(@one $factory; [$(#[$attr])*]; store_keeps_and_deletes_manifests_with_the_object_key);
        $crate::object_store_contract_tests!(@one $factory; [$(#[$attr])*]; store_handles_objects_across_the_sizes_it_is_given);
        $crate::object_store_contract_tests!(@one $factory; [$(#[$attr])*]; store_serves_concurrent_puts_of_the_same_content);
        $crate::object_store_contract_tests!(@one $factory; [$(#[$attr])*]; store_serves_chunked_objects_through_chunked_store);
    };
    (@one $factory:expr; [$(#[$attr:meta])*]; $name:ident) => {
        #[tokio::test(flavor = "multi_thread")]
        $(#[$attr])*
        async fn $name() {
            let (store, _keep): ($crate::contract::Objects, _) = ($factory).await;
            $crate::contract::$name(store).await;
        }
    };
}
