use async_trait::async_trait;

use crate::access::account;
use crate::error::Result;

/// Runs password hashing and checking, which takes tens of milliseconds. A server swaps in an
/// implementation that keeps it off the request thread.
#[async_trait]
pub trait PasswordWorker: Send + Sync {
    async fn hash(&self, password: &str) -> Result<String>;

    async fn verify(&self, password: &str, hash: &str) -> bool;
}

/// Hashes on the calling task.
#[derive(Debug, Default, Clone, Copy)]
pub struct InlinePasswords;

#[async_trait]
impl PasswordWorker for InlinePasswords {
    async fn hash(&self, password: &str) -> Result<String> {
        account::hash_password(password)
    }

    async fn verify(&self, password: &str, hash: &str) -> bool {
        account::verify_password(password, hash)
    }
}
