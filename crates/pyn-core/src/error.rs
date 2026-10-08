use chrono::{DateTime, Utc};

use crate::access::Permission;
use crate::types::{RepoPath, RevisionId, UserId};

pub type Result<T, E = PynError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum PynError {
    #[error("invalid path: {0:?}")]
    InvalidPath(String),
    #[error("invalid rules: {0}")]
    InvalidRules(String),
    #[error("{path} is locked by {owner} until {expires_at}")]
    LockHeld {
        path: RepoPath,
        owner: UserId,
        expires_at: DateTime<Utc>,
    },
    #[error("{0} is not locked by you")]
    NotLockHolder(RepoPath),
    #[error("{0} is exclusive: check it out before checking in")]
    LockRequired(RepoPath),
    #[error("{0} is not an exclusive path; there is nothing to check out")]
    NotExclusive(RepoPath),
    #[error("stale base for {path}: you have {expected:?}, head is {actual:?}")]
    StaleBase {
        path: RepoPath,
        expected: Option<RevisionId>,
        actual: Option<RevisionId>,
    },
    #[error("content {0} is not in the object store")]
    ObjectMissing(String),
    #[error("{path} has no revision {revision}")]
    RevisionNotFound { path: RepoPath, revision: String },
    #[error("authentication failed: {0}")]
    Unauthenticated(String),
    #[error("permission {0} is required")]
    Forbidden(Permission),
    #[error("no token {0}")]
    TokenNotFound(String),
    #[error("{0}")]
    InvalidRequest(String),
    #[error("storage error: {0}")]
    Storage(String),
}

impl PynError {
    /// Stable machine-readable code sent to API clients.
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidPath(_) => "invalid_path",
            Self::InvalidRules(_) => "invalid_rules",
            Self::LockHeld { .. } => "lock_held",
            Self::NotLockHolder(_) => "not_lock_holder",
            Self::LockRequired(_) => "lock_required",
            Self::NotExclusive(_) => "not_exclusive",
            Self::StaleBase { .. } => "stale_base",
            Self::ObjectMissing(_) => "object_missing",
            Self::RevisionNotFound { .. } => "revision_not_found",
            Self::Unauthenticated(_) => "unauthenticated",
            Self::Forbidden(_) => "forbidden",
            Self::TokenNotFound(_) => "token_not_found",
            Self::InvalidRequest(_) => "invalid_request",
            Self::Storage(_) => "storage",
        }
    }
}
