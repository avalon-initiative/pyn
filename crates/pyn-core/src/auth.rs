use async_trait::async_trait;

use crate::error::Result;
use crate::types::UserId;

/// Turns a presented credential into a principal. Implementations: dev header (server crate),
/// API tokens, OIDC later. The server authenticates; `pyn-core` only ever sees a `UserId`.
#[async_trait]
pub trait AuthProvider: Send + Sync {
    async fn authenticate(&self, credential: &str) -> Result<UserId>;
}
