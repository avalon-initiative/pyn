//! Content-addressed `ObjectStore` on the local filesystem.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use pyn_core::{ContentHash, ObjectStore, PynError, Result};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Objects live at `<root>/<first two hex chars>/<rest>`; writes go through a temp file and rename.
#[derive(Debug, Clone)]
pub struct FsObjectStore {
    root: PathBuf,
}

fn storage(e: impl std::fmt::Display) -> PynError {
    PynError::Storage(e.to_string())
}

fn hash_of(bytes: &[u8]) -> ContentHash {
    ContentHash::new(hex::encode(Sha256::digest(bytes)))
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
    async fn put(&self, bytes: Vec<u8>) -> Result<ContentHash> {
        let hash = hash_of(&bytes);
        let dest = self.path_for(&hash)?;
        if tokio::fs::try_exists(&dest).await.map_err(storage)? {
            return Ok(hash);
        }
        let dir = dest.parent().expect("object path has a parent");
        tokio::fs::create_dir_all(dir).await.map_err(storage)?;

        let n = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = dir.join(format!(".tmp-{}-{n}", std::process::id()));
        let mut file = tokio::fs::File::create(&tmp).await.map_err(storage)?;
        file.write_all(&bytes).await.map_err(storage)?;
        file.sync_all().await.map_err(storage)?;
        drop(file);
        tokio::fs::rename(&tmp, &dest).await.map_err(storage)?;
        Ok(hash)
    }

    async fn get(&self, hash: &ContentHash) -> Result<Option<Vec<u8>>> {
        let path = self.path_for(hash)?;
        let bytes = match tokio::fs::read(&path).await {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(storage(e)),
        };
        if &hash_of(&bytes) != hash {
            return Err(PynError::Storage(format!(
                "object {hash} is corrupt on disk"
            )));
        }
        Ok(Some(bytes))
    }

    async fn exists(&self, hash: &ContentHash) -> Result<bool> {
        let path = self.path_for(hash)?;
        tokio::fs::try_exists(path).await.map_err(storage)
    }
}
