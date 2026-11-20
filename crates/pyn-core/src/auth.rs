use std::net::IpAddr;

use async_trait::async_trait;

use crate::access::Identity;
use crate::error::Result;

/// Turns a presented credential into an `Identity`. `source` is the request's client address when the server knows it.
#[async_trait]
pub trait AuthProvider: Send + Sync {
    async fn authenticate(&self, credential: &str, source: Option<IpAddr>) -> Result<Identity>;
}
