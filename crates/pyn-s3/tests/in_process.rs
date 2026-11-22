//! S3-specific behaviour against the in-process server: layout, spooling, multipart, credentials.

mod common;

use std::time::Duration;

use common::{Server, in_process};
use pyn_core::{Keying, ObjectId, ObjectStore, PynError, REPO_ALGORITHM};
use pyn_s3::S3ObjectStore;

fn noise(len: usize) -> Vec<u8> {
    let mut x = 0x2545F4914F6CDD1Du64;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

fn spool_files(server: &Server) -> usize {
    let dir = server.settings().spool_dir;
    std::fs::read_dir(dir).map(|d| d.count()).unwrap_or(0)
}

fn stream(bytes: Vec<u8>) -> pyn_core::ObjectReader {
    Box::pin(std::io::Cursor::new(bytes))
}

#[tokio::test]
async fn objects_land_at_prefix_shard_and_rest_with_manifests_beside_them() {
    let server = Server::start();
    let mut settings = server.settings();
    settings.prefix = "repo-a".into();
    let store = S3ObjectStore::open(settings).await.unwrap();
    let id = store.put(b"hello".to_vec()).await.unwrap();
    let hex = id.to_hex();
    let at = server
        .root
        .join(common::BUCKET)
        .join("repo-a")
        .join(&hex[..2])
        .join(&hex[2..]);
    assert_eq!(std::fs::read(&at).unwrap(), b"hello");
    store.put_manifest(&id, b"m".to_vec()).await.unwrap();
    let manifest = at.with_file_name(format!("{}.cm", &hex[2..]));
    assert_eq!(std::fs::read(&manifest).unwrap(), b"m");
    assert!(store.delete(&id).await.unwrap());
    assert!(!at.exists() && !manifest.exists());
}

#[tokio::test]
async fn only_sha256_ids_are_stored() {
    let (store, _server) = in_process().await;
    let sha1 = ObjectId::from_hex(pyn_object::Algorithm::Sha1, &"ab".repeat(20)).unwrap();
    let err = store.exists(&sha1).await.unwrap_err();
    assert!(matches!(err, PynError::ObjectMissing(_)), "{err}");
    assert_eq!(REPO_ALGORITHM, pyn_object::Algorithm::Sha256);
}

#[tokio::test]
async fn a_spooled_put_leaves_no_spool_file_on_any_outcome() {
    let (store, server) = in_process().await;
    let data = noise(300_000);
    let len = data.len() as u64;

    let ok = store
        .put_stream_tracked(stream(data.clone()), Keying::Blob { len }, None)
        .await
        .unwrap();
    assert_eq!(ok.created, vec![ok.id]);
    assert_eq!(store.get(&ok.id).await.unwrap().unwrap(), data);
    assert_eq!(spool_files(&server), 0);

    let again = store
        .put_stream_tracked(stream(data.clone()), Keying::Blob { len }, None)
        .await
        .unwrap();
    assert!(again.created.is_empty());
    assert_eq!(spool_files(&server), 0, "an existing object is not re-sent");

    let too_big = store
        .put_stream(stream(data.clone()), Keying::Blob { len }, Some(200_000))
        .await;
    assert!(matches!(too_big, Err(PynError::ObjectTooLarge { .. })));
    assert_eq!(spool_files(&server), 0);

    let short = store
        .put_stream(stream(data.clone()), Keying::Blob { len: len + 1 }, None)
        .await;
    assert!(matches!(short, Err(PynError::LengthMismatch { .. })));
    assert_eq!(spool_files(&server), 0);

    let long = store
        .put_stream(stream(data), Keying::Blob { len: len - 1 }, None)
        .await;
    assert!(matches!(long, Err(PynError::LengthMismatch { .. })));
    assert_eq!(spool_files(&server), 0);
}

#[tokio::test]
async fn open_sweeps_only_stale_spool_files() {
    let server = Server::start();
    let settings = server.settings();
    std::fs::create_dir_all(&settings.spool_dir).unwrap();
    let stale = settings.spool_dir.join(".s3spool-stale");
    let other = settings.spool_dir.join("unrelated");
    std::fs::write(&stale, b"x").unwrap();
    std::fs::write(&other, b"x").unwrap();
    let store = S3ObjectStore::open(settings).await.unwrap();
    assert!(stale.exists(), "a fresh file is not stale");
    assert_eq!(store.sweep_stale_spool(Duration::ZERO).await.unwrap(), 1);
    assert!(!stale.exists());
    assert!(other.exists());
}

#[tokio::test]
async fn content_over_the_multipart_threshold_goes_up_in_parts_at_the_final_key() {
    let server = Server::start();
    let mut settings = server.settings();
    settings.multipart_threshold = 1 << 20;
    settings.part_size = 5 << 20;
    let store = S3ObjectStore::open(settings).await.unwrap();
    let data = noise(12 << 20);
    let stored = store
        .put_stream_tracked(stream(data.clone()), Keying::Raw, None)
        .await
        .unwrap();
    assert_eq!(stored.created, vec![stored.id]);
    assert_eq!(store.size(&stored.id).await.unwrap(), Some(12 << 20));
    let found = store
        .read(&stored.id, Some(5_242_870..5_242_890))
        .await
        .unwrap()
        .unwrap();
    let mut got = Vec::new();
    tokio::io::AsyncReadExt::read_to_end(&mut { found.reader }, &mut got)
        .await
        .unwrap();
    assert_eq!(got, &data[5_242_870..5_242_890]);
    assert_eq!(spool_files(&server), 0);
}

#[tokio::test]
async fn wrong_credentials_are_refused_with_the_operation_status_and_code() {
    let server = Server::start();
    let mut settings = server.settings();
    settings.secret_access_key = "not-the-secret".into();
    let store = S3ObjectStore::open(settings).await.unwrap();
    let err = store.put(b"x".to_vec()).await.unwrap_err();
    let text = err.to_string();
    assert!(matches!(err, PynError::Storage(_)), "{text}");
    assert!(text.contains("HeadObject"), "{text}");
    assert!(text.contains("403"), "{text}");
    assert!(!text.contains("not-the-secret"), "{text}");
}

#[tokio::test]
async fn the_probe_leaves_no_marker_and_names_the_failing_operation() {
    let server = Server::start();
    let mut settings = server.settings();
    settings.prefix = "repo-a".into();
    let store = S3ObjectStore::open(settings.clone()).await.unwrap();
    store.probe().await.unwrap();
    let markers = server.root.join(common::BUCKET).join("repo-a/.pyn-probe");
    let left = std::fs::read_dir(&markers).map(|d| d.count()).unwrap_or(0);
    assert_eq!(left, 0, "the marker is deleted");

    settings.secret_access_key = "not-the-secret".into();
    let text = S3ObjectStore::open(settings)
        .await
        .unwrap()
        .probe()
        .await
        .unwrap_err()
        .to_string();
    assert!(text.contains("PutObject") && text.contains("403"), "{text}");
    assert!(!text.contains("not-the-secret"), "{text}");
}
