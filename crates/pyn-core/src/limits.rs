//! Per-owner limits and usage. Every limit is opt-in: unset means unlimited, and nothing is enforced until
//! an operator sets one.

use async_trait::async_trait;

use crate::error::{PynError, Result};
use crate::types::UserId;

/// The largest value a limit can take; stores keep it in a signed 64-bit column.
pub const MAX_LIMIT_VALUE: u64 = i64::MAX as u64;

/// Limits on one owner (a user or an organization). `None` is unlimited, or "follow the server default"
/// when these are an owner's own settings.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Limits {
    pub repositories: Option<u64>,
    /// Organization members; users have no member limit.
    pub members: Option<u64>,
    /// Bytes of distinct content across the owner's repositories.
    pub storage_bytes: Option<u64>,
}

impl Limits {
    pub fn validate(self) -> Result<Self> {
        for value in [self.repositories, self.members, self.storage_bytes]
            .into_iter()
            .flatten()
        {
            if value > MAX_LIMIT_VALUE {
                return Err(PynError::InvalidRequest(format!(
                    "a limit is at most {MAX_LIMIT_VALUE}"
                )));
            }
        }
        Ok(self)
    }

    /// Each field of `self` where set, else the field of `fallback`.
    pub fn or(self, fallback: Limits) -> Limits {
        Limits {
            repositories: self.repositories.or(fallback.repositories),
            members: self.members.or(fallback.members),
            storage_bytes: self.storage_bytes.or(fallback.storage_bytes),
        }
    }

    pub fn is_unset(&self) -> bool {
        *self == Limits::default()
    }
}

/// A partial change to an owner's own limits: `None` leaves a field, `Some(None)` clears it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LimitsChange {
    pub repositories: Option<Option<u64>>,
    pub members: Option<Option<u64>>,
    pub storage_bytes: Option<Option<u64>>,
}

impl LimitsChange {
    pub fn validate(self) -> Result<Self> {
        Limits {
            repositories: self.repositories.flatten(),
            members: self.members.flatten(),
            storage_bytes: self.storage_bytes.flatten(),
        }
        .validate()?;
        Ok(self)
    }

    pub fn apply(self, current: Limits) -> Limits {
        Limits {
            repositories: self.repositories.unwrap_or(current.repositories),
            members: self.members.unwrap_or(current.members),
            storage_bytes: self.storage_bytes.unwrap_or(current.storage_bytes),
        }
    }
}

/// What an owner uses now. `members` is `None` for a user.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub repositories: u64,
    pub members: Option<u64>,
    pub stored_bytes: u64,
}

/// What one repository uses now.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RepoUsage {
    pub stored_bytes: u64,
    /// Paths with a head revision.
    pub files: u64,
    pub revisions: u64,
}

/// A storage ceiling a store enforces together with a revision write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageCap {
    pub owner: UserId,
    pub max_bytes: u64,
}

/// Where a repository's service finds the storage ceiling of its owner.
#[async_trait]
pub trait StorageCaps: Send + Sync {
    async fn storage_cap(&self, owner: &UserId) -> Result<Option<u64>>;
}
