use async_trait::async_trait;

use crate::error::Result;
use crate::types::ContentHash;

/// Content-addressed blob storage keyed by SHA-256, so `put` is idempotent.
#[async_trait]
pub trait ObjectStore: Send + Sync {
    async fn put(&self, bytes: Vec<u8>) -> Result<ContentHash>;
    async fn get(&self, hash: &ContentHash) -> Result<Option<Vec<u8>>>;
    async fn exists(&self, hash: &ContentHash) -> Result<bool>;
}
