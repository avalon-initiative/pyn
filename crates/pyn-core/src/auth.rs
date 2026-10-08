use async_trait::async_trait;

use crate::access::Principal;
use crate::error::Result;

/// Turns a presented credential into a `Principal`.
#[async_trait]
pub trait AuthProvider: Send + Sync {
    async fn authenticate(&self, credential: &str) -> Result<Principal>;
}
