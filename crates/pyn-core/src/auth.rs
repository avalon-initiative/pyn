use async_trait::async_trait;

use crate::access::Identity;
use crate::error::Result;

/// Turns a presented credential into an `Identity`.
#[async_trait]
pub trait AuthProvider: Send + Sync {
    async fn authenticate(&self, credential: &str) -> Result<Identity>;
}
