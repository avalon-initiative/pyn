use async_trait::async_trait;

use crate::error::Result;
use crate::types::ContentHash;

/// Content-addressed blob storage. Keys are SHA-256 of the content, so `put` is idempotent and
/// dedupes. This whole-buffer interface is a placeholder: large-file support needs chunked,
/// streaming, resumable put/get, and that will change this trait before it has a second
/// implementation (tracked in NEXT_TASKS).
#[async_trait]
pub trait ObjectStore: Send + Sync {
    async fn put(&self, bytes: Vec<u8>) -> Result<ContentHash>;
    async fn get(&self, hash: &ContentHash) -> Result<Option<Vec<u8>>>;
    async fn exists(&self, hash: &ContentHash) -> Result<bool>;
}
