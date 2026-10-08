use std::sync::Arc;

use async_trait::async_trait;
use pyn_core::{AccessService, AuthProvider, Principal, PynError, RepoId, UserId};

/// Authenticates bearer tokens against the access store.
pub struct BearerAuth {
    pub access: Arc<AccessService>,
    pub repo: RepoId,
}

#[async_trait]
impl AuthProvider for BearerAuth {
    async fn authenticate(&self, credential: &str) -> pyn_core::Result<Principal> {
        self.access.authenticate(&self.repo, credential).await
    }
}

/// Dev only: trusts the `X-Pyn-User` header and grants every permission. Never expose to an untrusted network.
pub struct DevHeaderAuth;

#[async_trait]
impl AuthProvider for DevHeaderAuth {
    async fn authenticate(&self, credential: &str) -> pyn_core::Result<Principal> {
        if credential.trim().is_empty() {
            return Err(PynError::Unauthenticated("missing X-Pyn-User".into()));
        }
        Ok(Principal::unrestricted(UserId::new(credential.trim())))
    }
}
