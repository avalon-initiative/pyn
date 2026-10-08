use async_trait::async_trait;

use crate::error::Result;
use crate::types::UserId;

/// Turns a presented credential into a `UserId`.
#[async_trait]
pub trait AuthProvider: Send + Sync {
    async fn authenticate(&self, credential: &str) -> Result<UserId>;
}
