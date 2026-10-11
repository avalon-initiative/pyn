use std::io::Cursor;
use std::ops::Range;
use std::pin::Pin;

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::error::{PynError, Result};
use crate::types::ContentHash;

pub type ObjectReader = Pin<Box<dyn AsyncRead + Send>>;

/// A stored object's bytes, whole or a range, with the object's full length.
pub struct ObjectRead {
    pub reader: ObjectReader,
    pub total: u64,
    pub range: Range<u64>,
}

/// Content-addressed blob storage keyed by SHA-256 of the content, so `put` is idempotent.
#[async_trait]
pub trait ObjectStore: Send + Sync {
    /// Stores what `reader` yields, hashing as it goes; `ObjectTooLarge` if it yields more than `max` bytes.
    async fn put_stream(&self, reader: ObjectReader, max: Option<u64>) -> Result<ContentHash>;
    /// The object, or the part inside `range` (clamped to its length); `None` if it is not stored.
    async fn read(
        &self,
        hash: &ContentHash,
        range: Option<Range<u64>>,
    ) -> Result<Option<ObjectRead>>;
    async fn exists(&self, hash: &ContentHash) -> Result<bool>;
    /// The object's length in bytes, `None` if it is not stored.
    async fn size(&self, hash: &ContentHash) -> Result<Option<u64>>;
    /// Removes the object; `false` if it was not stored.
    async fn delete(&self, hash: &ContentHash) -> Result<bool>;

    /// Keeps a chunk manifest under the whole object's `key` (see `chunking`); stores that cannot refuse.
    async fn put_manifest(&self, _key: &ContentHash, _bytes: Vec<u8>) -> Result<()> {
        Err(PynError::Storage(
            "this store cannot keep chunk manifests".into(),
        ))
    }

    async fn get_manifest(&self, _key: &ContentHash) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }

    /// Makes one object of already stored chunks, in order, and returns the hash of their concatenation. The
    /// store reads the chunks to compute it, so the caller cannot claim a hash.
    async fn assemble(&self, _chunks: &[ContentHash], _max: Option<u64>) -> Result<ContentHash> {
        Err(PynError::Storage(
            "this store cannot assemble chunks".into(),
        ))
    }

    /// The chunk blobs an object is made of; empty for a whole object.
    async fn chunks_of(&self, _hash: &ContentHash) -> Result<Vec<ContentHash>> {
        Ok(Vec::new())
    }

    async fn put(&self, bytes: Vec<u8>) -> Result<ContentHash> {
        self.put_stream(Box::pin(Cursor::new(bytes)), None).await
    }

    /// The whole object, checked against its hash.
    async fn get(&self, hash: &ContentHash) -> Result<Option<Vec<u8>>> {
        let Some(found) = self.read(hash, None).await? else {
            return Ok(None);
        };
        let mut bytes = Vec::with_capacity(usize::try_from(found.total).unwrap_or(0));
        let mut reader = found.reader;
        reader
            .read_to_end(&mut bytes)
            .await
            .map_err(|e| PynError::Storage(e.to_string()))?;
        if hex::encode(Sha256::digest(&bytes)) != hash.as_str() {
            return Err(PynError::Storage(format!("object {hash} is corrupt")));
        }
        Ok(Some(bytes))
    }
}

/// Clamps a requested range to an object of `total` bytes.
pub fn clamp_range(range: Option<Range<u64>>, total: u64) -> Range<u64> {
    match range {
        Some(r) => r.start.min(total)..r.end.min(total).max(r.start.min(total)),
        None => 0..total,
    }
}

/// Reads at most `max` bytes (all when `None`) into memory; `ObjectTooLarge` beyond it.
pub async fn read_limited(mut reader: ObjectReader, max: Option<u64>) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    match max {
        Some(limit) => {
            (&mut reader)
                .take(limit.saturating_add(1))
                .read_to_end(&mut bytes)
                .await
                .map_err(|e| PynError::Storage(e.to_string()))?;
            if bytes.len() as u64 > limit {
                return Err(PynError::ObjectTooLarge { limit });
            }
        }
        None => {
            reader
                .read_to_end(&mut bytes)
                .await
                .map_err(|e| PynError::Storage(e.to_string()))?;
        }
    }
    Ok(bytes)
}
