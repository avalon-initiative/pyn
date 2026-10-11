//! Content-addressed `ObjectStore` on the local filesystem.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use pyn_core::object::clamp_range;
use pyn_core::{ContentHash, ObjectRead, ObjectReader, ObjectStore, PynError, Result};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Objects live at `<root>/<first two hex chars>/<rest>`; writes go through a temp file and rename.
#[derive(Debug, Clone)]
pub struct FsObjectStore {
    root: PathBuf,
}

fn storage(e: impl std::fmt::Display) -> PynError {
    PynError::Storage(e.to_string())
}

impl FsObjectStore {
    pub async fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        tokio::fs::create_dir_all(&root).await.map_err(storage)?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    async fn write_temp(
        &self,
        reader: &mut ObjectReader,
        max: Option<u64>,
        tmp: &Path,
    ) -> Result<ContentHash> {
        let mut file = tokio::fs::File::create(tmp).await.map_err(storage)?;
        let mut hasher = Sha256::new();
        let mut total = 0u64;
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = reader.read(&mut buf).await.map_err(storage)?;
            if n == 0 {
                break;
            }
            total += n as u64;
            if max.is_some_and(|limit| total > limit) {
                return Err(PynError::ObjectTooLarge {
                    limit: max.unwrap_or_default(),
                });
            }
            hasher.update(&buf[..n]);
            file.write_all(&buf[..n]).await.map_err(storage)?;
        }
        file.sync_all().await.map_err(storage)?;
        Ok(ContentHash::new(hex::encode(hasher.finalize())))
    }

    /// Rejects anything that is not 64 lowercase hex characters, so a hash can never escape the root.
    fn path_for(&self, hash: &ContentHash) -> Result<PathBuf> {
        let h = hash.as_str();
        let valid = h.len() == 64 && h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        if !valid {
            return Err(PynError::ObjectMissing(h.to_string()));
        }
        Ok(self.root.join(&h[..2]).join(&h[2..]))
    }
}

#[async_trait]
impl ObjectStore for FsObjectStore {
    async fn put_stream(&self, mut reader: ObjectReader, max: Option<u64>) -> Result<ContentHash> {
        let n = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = self.root.join(format!(".tmp-{}-{n}", std::process::id()));
        let written = self.write_temp(&mut reader, max, &tmp).await;
        let hash = match written {
            Ok(hash) => hash,
            Err(e) => {
                let _ = tokio::fs::remove_file(&tmp).await;
                return Err(e);
            }
        };
        let dest = self.path_for(&hash)?;
        let dir = dest.parent().expect("object path has a parent");
        let placed = async {
            if tokio::fs::try_exists(&dest).await.map_err(storage)? {
                return tokio::fs::remove_file(&tmp).await.map_err(storage);
            }
            tokio::fs::create_dir_all(dir).await.map_err(storage)?;
            tokio::fs::rename(&tmp, &dest).await.map_err(storage)
        }
        .await;
        if placed.is_err() {
            let _ = tokio::fs::remove_file(&tmp).await;
        }
        placed.map(|()| hash)
    }

    async fn read(
        &self,
        hash: &ContentHash,
        range: Option<Range<u64>>,
    ) -> Result<Option<ObjectRead>> {
        let path = self.path_for(hash)?;
        let mut file = match tokio::fs::File::open(&path).await {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(storage(e)),
        };
        let total = file.metadata().await.map_err(storage)?.len();
        let range = clamp_range(range, total);
        file.seek(std::io::SeekFrom::Start(range.start))
            .await
            .map_err(storage)?;
        Ok(Some(ObjectRead {
            reader: Box::pin(file.take(range.end - range.start)),
            total,
            range,
        }))
    }

    async fn delete(&self, hash: &ContentHash) -> Result<bool> {
        let path = self.path_for(hash)?;
        match tokio::fs::remove_file(path).await {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(storage(e)),
        }
    }

    async fn exists(&self, hash: &ContentHash) -> Result<bool> {
        let path = self.path_for(hash)?;
        tokio::fs::try_exists(path).await.map_err(storage)
    }

    async fn size(&self, hash: &ContentHash) -> Result<Option<u64>> {
        let path = self.path_for(hash)?;
        match tokio::fs::metadata(path).await {
            Ok(meta) => Ok(Some(meta.len())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(storage(e)),
        }
    }
}
