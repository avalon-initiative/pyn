use async_trait::async_trait;
use pyn_core::{AuthProvider, PynError, UserId};

/// DEV ONLY. Trusts whatever user name the client sends in `X-Pyn-User`. Exists so the lock
/// flow can be exercised before real authentication (tokens, OIDC) is built; never expose a
/// server running this to a network you do not control.
pub struct DevHeaderAuth;

#[async_trait]
impl AuthProvider for DevHeaderAuth {
    async fn authenticate(&self, credential: &str) -> pyn_core::Result<UserId> {
        if credential.trim().is_empty() {
            return Err(PynError::InvalidPath("missing X-Pyn-User".into()));
        }
        Ok(UserId::new(credential.trim()))
    }
}
