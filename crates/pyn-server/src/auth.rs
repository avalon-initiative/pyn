use async_trait::async_trait;
use pyn_core::{AuthProvider, Principal, PynError, UserId};

/// Dev only: trusts the `X-Pyn-User` header. Never expose to an untrusted network.
pub struct DevHeaderAuth;

#[async_trait]
impl AuthProvider for DevHeaderAuth {
    async fn authenticate(&self, credential: &str) -> pyn_core::Result<Principal> {
        if credential.trim().is_empty() {
            return Err(PynError::InvalidPath("missing X-Pyn-User".into()));
        }
        Ok(Principal::unrestricted(UserId::new(credential.trim())))
    }
}
