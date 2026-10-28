//! A frozen owner keeps every byte and stays readable; every write under it is refused.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;

use crate::{PynError, Result, UserId};

/// The longest reason a freeze records, in bytes.
pub const FREEZE_REASON_BYTES: usize = 512;

/// Answers whether writes under an owner are allowed right now.
#[async_trait]
pub trait OwnerWrites: Send + Sync {
    /// `OwnerFrozen` when `owner` is frozen.
    async fn ensure_writable(&self, owner: &UserId) -> Result<()>;
}

/// Keeps frozen owners in memory, for tests.
#[derive(Debug, Default)]
pub struct MemoryOwnerWrites {
    frozen: Mutex<HashMap<UserId, String>>,
}

impl MemoryOwnerWrites {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn freeze(&self, owner: &UserId, reason: &str) {
        self.frozen
            .lock()
            .unwrap()
            .insert(owner.clone(), reason.to_string());
    }

    pub fn unfreeze(&self, owner: &UserId) {
        self.frozen.lock().unwrap().remove(owner);
    }
}

#[async_trait]
impl OwnerWrites for MemoryOwnerWrites {
    async fn ensure_writable(&self, owner: &UserId) -> Result<()> {
        match self.frozen.lock().unwrap().get(owner) {
            Some(reason) => Err(PynError::OwnerFrozen {
                owner: owner.to_string(),
                reason: Some(reason.clone()),
            }),
            None => Ok(()),
        }
    }
}
