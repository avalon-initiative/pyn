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
    /// The owner's ceiling across its repositories.
    pub max_bytes: Option<u64>,
    /// The ceiling on the one repository written to.
    pub repo_max_bytes: Option<u64>,
    /// `owner/name` of that repository, for the refusal message.
    pub repository: String,
}

/// Stored bytes of one top-level folder (`""` is the files at the root), counting distinct content once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderUsage {
    pub folder: String,
    pub stored_bytes: u64,
    pub files: u64,
}

/// Distinct content stored for one path across its revisions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathUsage {
    pub path: String,
    pub stored_bytes: u64,
    pub revisions: u64,
}

/// Where a repository's stored bytes sit. Folders and paths may share content, so their sums can pass the total.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoBreakdown {
    pub folders: Vec<FolderUsage>,
    pub largest_paths: Vec<PathUsage>,
}

impl RepoBreakdown {
    /// Folders by size then name, and the `BREAKDOWN_PATHS` largest paths by size then name.
    pub fn new(mut folders: Vec<FolderUsage>, mut paths: Vec<PathUsage>) -> Self {
        folders.sort_by(|a, b| (b.stored_bytes, &a.folder).cmp(&(a.stored_bytes, &b.folder)));
        paths.sort_by(|a, b| (b.stored_bytes, &a.path).cmp(&(a.stored_bytes, &b.path)));
        paths.truncate(BREAKDOWN_PATHS);
        Self {
            folders,
            largest_paths: paths,
        }
    }
}

/// Paths a breakdown lists.
pub const BREAKDOWN_PATHS: usize = 20;

/// The top-level folder of `path`, `""` for a file at the root.
pub fn top_folder(path: &str) -> &str {
    path.split_once('/').map_or("", |(dir, _)| dir)
}

/// Where a repository's service finds the storage ceiling of its owner.
#[async_trait]
pub trait StorageCaps: Send + Sync {
    async fn storage_cap(&self, owner: &UserId) -> Result<Option<u64>>;
}
