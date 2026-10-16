//! Upload staging on the filesystem store: temp files, quota holds and cleanup.

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use pyn_core::memory::{MemoryAuditStore, MemoryMetadataStore};
use pyn_core::{
    ChunkedStore, Keying, MetadataStore, ObjectStore, PynError, RepoId, RepoService, Rules,
    ServiceConfig, SystemClock,
};
use pyn_fs::FsObjectStore;
use tokio::io::AsyncWriteExt;

const RULES: &str = "[meta]\ndefault = \"shared\"\n";

fn files_under(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
            } else {
                out.push(path);
            }
        }
    }
    out
}

fn temp_files(root: &Path) -> Vec<PathBuf> {
    files_under(root)
        .into_iter()
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".tmp-")
        })
        .collect()
}

fn object_files(root: &Path) -> usize {
    files_under(root).len() - temp_files(root).len()
}

struct Rig {
    root: PathBuf,
    meta: Arc<MemoryMetadataStore>,
    svc: Arc<RepoService>,
    _dir: tempfile::TempDir,
}

async fn rig(cap: u64, chunked: bool) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("objects");
    let fs = FsObjectStore::open(&root).await.unwrap();
    let objects: Arc<dyn ObjectStore> = if chunked {
        Arc::new(ChunkedStore::new(Arc::new(fs)))
    } else {
        Arc::new(fs)
    };
    let meta = Arc::new(MemoryMetadataStore::new());
    let svc = Arc::new(
        RepoService::new(
            RepoId::new("game"),
            Rules::from_toml(RULES).unwrap(),
            meta.clone(),
            objects,
            Arc::new(MemoryAuditStore::new()),
            Arc::new(SystemClock),
            ServiceConfig::default(),
        )
        .with_unreferenced_cap(cap),
    );
    Rig {
        root,
        meta,
        svc,
        _dir: dir,
    }
}

#[tokio::test]
async fn a_cancelled_upload_leaves_no_temp_file_and_frees_its_budget() {
    let rig = rig(100, false).await;
    let (mut tx, rx) = tokio::io::duplex(1024);
    tx.write_all(b"partial").await.unwrap();
    let svc = rig.svc.clone();
    let upload = tokio::spawn(async move {
        svc.upload(Box::pin(rx), Keying::Blob { len: 100 }, None, 100)
            .await
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(temp_files(&rig.root).len(), 1);
    upload.abort();
    let _ = upload.await;

    for _ in 0..50 {
        if temp_files(&rig.root).is_empty() && rig.svc.upload_blob(vec![b'z'; 100]).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the temp file or the budget hold stayed behind");
}

#[tokio::test]
async fn a_short_or_long_body_leaves_nothing_behind() {
    for chunked in [false, true] {
        let rig = rig(100 << 20, chunked).await;
        let body = vec![b'q'; (3 << 20) / 2];
        let long = rig
            .svc
            .upload(
                Box::pin(std::io::Cursor::new(body.clone())),
                Keying::Blob { len: 10 },
                None,
                10,
            )
            .await
            .unwrap_err();
        assert!(matches!(long, PynError::LengthMismatch { .. }), "{long}");
        let short = rig
            .svc
            .upload(
                Box::pin(std::io::Cursor::new(body)),
                Keying::Blob { len: 4 << 20 },
                None,
                4 << 20,
            )
            .await
            .unwrap_err();
        assert!(matches!(short, PynError::LengthMismatch { .. }), "{short}");
        assert!(files_under(&rig.root).is_empty(), "chunked={chunked}");
    }
}

#[tokio::test]
async fn parallel_uploads_against_the_budget_store_only_what_was_recorded() {
    let rig = rig(10, false).await;
    let tasks: Vec<_> = (0..8u8)
        .map(|n| {
            let svc = rig.svc.clone();
            tokio::spawn(async move { svc.upload_blob(vec![b'a' + n; 4]).await })
        })
        .collect();
    let mut stored = 0;
    for task in tasks {
        match task.await.unwrap() {
            Ok(_) => stored += 1,
            Err(e) => assert!(matches!(e, PynError::StorageLimitReached { .. }), "{e}"),
        }
    }
    assert_eq!(stored, 2);
    assert_eq!(object_files(&rig.root), 2);
    assert!(temp_files(&rig.root).is_empty());
    let repo = RepoId::new("game");
    assert_eq!(rig.meta.unreferenced_bytes(&repo).await.unwrap(), 8);
}

#[tokio::test]
async fn startup_removes_stale_temp_files_only() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("objects");
    std::fs::create_dir_all(&root).unwrap();
    let old = root.join(".tmp-0123");
    let fresh = root.join(".tmp-4567");
    let other = root.join("keep");
    for p in [&old, &fresh, &other] {
        std::fs::write(p, b"x").unwrap();
    }
    let long_ago = SystemTime::now() - Duration::from_secs(2 * 3600);
    std::fs::File::options()
        .write(true)
        .open(&old)
        .unwrap()
        .set_modified(long_ago)
        .unwrap();

    let store = FsObjectStore::open(&root).await.unwrap();
    assert!(!old.exists());
    assert!(fresh.exists() && other.exists());
    assert_eq!(store.sweep_stale_temp(Duration::ZERO).await.unwrap(), 1);
    assert!(!fresh.exists() && other.exists());
}

#[tokio::test]
async fn the_periodic_sweep_removes_stale_temp_files() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("objects");
    let store = FsObjectStore::open(&root).await.unwrap();
    let stale = root.join(".tmp-aaaa");
    std::fs::write(&stale, b"x").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&stale)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(2 * 3600))
        .unwrap();
    let task = store.spawn_sweeper(Duration::from_millis(20));
    for _ in 0..50 {
        if !stale.exists() {
            task.abort();
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the stale temp file was not swept");
}

#[tokio::test]
async fn storing_existing_bytes_never_rewrites_the_object_file() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("objects");
    let store = FsObjectStore::open(&root).await.unwrap();
    let first = store
        .put_stream_tracked(
            Box::pin(std::io::Cursor::new(b"same bytes".to_vec())),
            Keying::Blob { len: 10 },
            None,
        )
        .await
        .unwrap();
    assert_eq!(first.created, vec![first.id]);
    let file = files_under(&root).pop().unwrap();
    let before = std::fs::metadata(&file).unwrap();

    let second = store
        .put_stream_tracked(
            Box::pin(std::io::Cursor::new(b"same bytes".to_vec())),
            Keying::Blob { len: 10 },
            None,
        )
        .await
        .unwrap();
    assert_eq!(second.id, first.id);
    assert!(second.created.is_empty());
    let after = std::fs::metadata(&file).unwrap();
    assert_eq!((before.ino(), before.len()), (after.ino(), after.len()));
    assert_eq!(std::fs::read(&file).unwrap(), b"same bytes");
    assert!(temp_files(&root).is_empty());
}

#[tokio::test]
async fn racing_puts_of_the_same_bytes_report_one_creator() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("objects");
    let store = Arc::new(FsObjectStore::open(&root).await.unwrap());
    let tasks: Vec<_> = (0..16)
        .map(|_| {
            let store = store.clone();
            tokio::spawn(async move {
                store
                    .put_stream_tracked(
                        Box::pin(std::io::Cursor::new(vec![b'r'; 5000])),
                        Keying::Blob { len: 5000 },
                        None,
                    )
                    .await
                    .unwrap()
            })
        })
        .collect();
    let mut creators = 0;
    for task in tasks {
        creators += usize::from(!task.await.unwrap().created.is_empty());
    }
    assert_eq!(creators, 1);
    assert!(temp_files(&root).is_empty());
}
