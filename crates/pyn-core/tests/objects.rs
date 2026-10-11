use pyn_core::memory::MemoryObjectStore;
use pyn_core::{ObjectStore, PynError};
use tokio::io::AsyncReadExt;

#[tokio::test]
async fn the_memory_store_streams_ranges_and_deletes() {
    let s = MemoryObjectStore::new();
    let hash = s
        .put_stream(Box::pin(std::io::Cursor::new(b"0123456789".to_vec())), None)
        .await
        .unwrap();
    let mut part = s.read(&hash, Some(2..5)).await.unwrap().unwrap();
    assert_eq!((part.total, part.range.clone()), (10, 2..5));
    let mut got = Vec::new();
    part.reader.read_to_end(&mut got).await.unwrap();
    assert_eq!(got, b"234");
    assert_eq!(s.get(&hash).await.unwrap().unwrap(), b"0123456789");
    assert!(s.delete(&hash).await.unwrap());
    assert!(s.get(&hash).await.unwrap().is_none());
}

#[tokio::test]
async fn the_memory_store_refuses_more_than_the_limit() {
    let s = MemoryObjectStore::new();
    let err = s
        .put_stream(Box::pin(std::io::Cursor::new(vec![0u8; 11])), Some(10))
        .await
        .unwrap_err();
    assert!(
        matches!(err, PynError::ObjectTooLarge { limit: 10 }),
        "{err}"
    );
}
