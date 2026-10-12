use std::io::Cursor;
use std::ops::Range;
use std::pin::Pin;

use async_trait::async_trait;
use pyn_object::{Algorithm, Kind, ObjectHasher, ObjectId};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::error::{PynError, Result};

/// A pyn server repository is always SHA-256.
pub const REPO_ALGORITHM: Algorithm = Algorithm::Sha256;

pub type ObjectReader = Pin<Box<dyn AsyncRead + Send>>;

/// A stored object's bytes, whole or a range, with the object's full length.
pub struct ObjectRead {
    pub reader: ObjectReader,
    pub total: u64,
    pub range: Range<u64>,
}

/// How a stored object's key derives from its bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keying {
    /// The git blob id; the declared length is hashed in the header, so the bytes must match it.
    Blob { len: u64 },
    /// The raw SHA-256 of the bytes: a chunk's internal key (decision #194).
    Raw,
}

/// Hashes a stream as it is stored, per `Keying`.
pub enum KeyHasher {
    Blob(ObjectHasher),
    Raw(Sha256),
}

impl KeyHasher {
    pub fn new(keying: Keying) -> Self {
        match keying {
            Keying::Blob { len } => Self::Blob(ObjectHasher::new(REPO_ALGORITHM, Kind::Blob, len)),
            Keying::Raw => Self::Raw(Sha256::new()),
        }
    }

    /// `LengthMismatch` as soon as more than the declared length arrives.
    pub fn update(&mut self, data: &[u8]) -> Result<()> {
        match self {
            Self::Blob(h) => h.update(data).map_err(object_error),
            Self::Raw(h) => {
                h.update(data);
                Ok(())
            }
        }
    }

    /// `LengthMismatch` if fewer bytes than declared arrived.
    pub fn finish(self) -> Result<ObjectId> {
        match self {
            Self::Blob(h) => h.finish().map_err(object_error),
            Self::Raw(h) => {
                ObjectId::from_bytes(REPO_ALGORITHM, &h.finalize()).map_err(object_error)
            }
        }
    }
}

fn object_error(e: pyn_object::Error) -> PynError {
    match e {
        pyn_object::Error::LengthMismatch { declared, actual } => {
            PynError::LengthMismatch { declared, actual }
        }
        other => PynError::Storage(other.to_string()),
    }
}

/// Content-addressed storage, so `put` is idempotent. A whole object is keyed by its blob id, a chunk by the raw
/// SHA-256 of its bytes (`Keying`).
#[async_trait]
pub trait ObjectStore: Send + Sync {
    /// Stores what `reader` yields, hashing as it goes; `ObjectTooLarge` if it yields more than `max` bytes.
    async fn put_stream(
        &self,
        reader: ObjectReader,
        keying: Keying,
        max: Option<u64>,
    ) -> Result<ObjectId>;
    /// The object, or the part inside `range` (clamped to its length); `None` if it is not stored.
    async fn read(&self, hash: &ObjectId, range: Option<Range<u64>>) -> Result<Option<ObjectRead>>;
    async fn exists(&self, hash: &ObjectId) -> Result<bool>;
    /// The object's length in bytes, `None` if it is not stored.
    async fn size(&self, hash: &ObjectId) -> Result<Option<u64>>;
    /// Removes the object; `false` if it was not stored.
    async fn delete(&self, hash: &ObjectId) -> Result<bool>;

    /// Keeps a chunk manifest under the whole object's `key` (see `chunking`); stores that cannot refuse.
    async fn put_manifest(&self, _key: &ObjectId, _bytes: Vec<u8>) -> Result<()> {
        Err(PynError::Storage(
            "this store cannot keep chunk manifests".into(),
        ))
    }

    async fn get_manifest(&self, _key: &ObjectId) -> Result<Option<Vec<u8>>> {
        Ok(None)
    }

    /// Makes one object of already stored chunks, in order, and returns the blob id of their concatenation. The
    /// store reads the chunks to compute it, so the caller cannot claim an id.
    async fn assemble(&self, _chunks: &[ObjectId], _max: Option<u64>) -> Result<ObjectId> {
        Err(PynError::Storage(
            "this store cannot assemble chunks".into(),
        ))
    }

    /// The chunk blobs an object is made of; empty for a whole object.
    async fn chunks_of(&self, _hash: &ObjectId) -> Result<Vec<ObjectId>> {
        Ok(Vec::new())
    }

    async fn put(&self, bytes: Vec<u8>) -> Result<ObjectId> {
        let len = bytes.len() as u64;
        self.put_stream(Box::pin(Cursor::new(bytes)), Keying::Blob { len }, None)
            .await
    }

    /// Stores a chunk under the raw SHA-256 of its bytes.
    async fn put_chunk(&self, bytes: Vec<u8>) -> Result<ObjectId> {
        self.put_stream(Box::pin(Cursor::new(bytes)), Keying::Raw, None)
            .await
    }

    /// The whole object, checked against its blob id.
    async fn get(&self, hash: &ObjectId) -> Result<Option<Vec<u8>>> {
        let Some(found) = self.read(hash, None).await? else {
            return Ok(None);
        };
        let mut bytes = Vec::with_capacity(usize::try_from(found.total).unwrap_or(0));
        let mut reader = found.reader;
        reader
            .read_to_end(&mut bytes)
            .await
            .map_err(|e| PynError::Storage(e.to_string()))?;
        let actual =
            pyn_object::hash_object(hash.algorithm(), Kind::Blob, &bytes).map_err(object_error)?;
        if actual != *hash {
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
