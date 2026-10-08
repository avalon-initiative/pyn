use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::error::Result;
use crate::types::{Lock, NewRevision, RepoId, RepoPath, Revision, RevisionId, UserId};

/// Persistence for locks and revisions. Each method is one atomic check-and-write; policy lives in
/// `RepoService`. A lock with `expires_at <= now` counts as absent.
#[async_trait]
pub trait MetadataStore: Send + Sync {
    /// Take or renew the lock; `LockHeld` if someone else holds a live one.
    async fn acquire_lock(
        &self,
        repo: &RepoId,
        path: &RepoPath,
        owner: &UserId,
        now: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> Result<Lock>;

    /// `NotLockHolder` if there is no live lock or another user holds it.
    async fn release_lock(
        &self,
        repo: &RepoId,
        path: &RepoPath,
        owner: &UserId,
        now: DateTime<Utc>,
    ) -> Result<()>;

    async fn get_lock(
        &self,
        repo: &RepoId,
        path: &RepoPath,
        now: DateTime<Utc>,
    ) -> Result<Option<Lock>>;

    async fn list_locks(&self, repo: &RepoId, now: DateTime<Utc>) -> Result<Vec<Lock>>;

    async fn head_revision(&self, repo: &RepoId, path: &RepoPath) -> Result<Option<Revision>>;

    /// Append a revision iff the head is `expected_head`, else `StaleBase`. With `lock_holder`,
    /// also require that user's live lock in the same step and release it.
    async fn commit_revision(
        &self,
        repo: &RepoId,
        revision: NewRevision,
        expected_head: Option<RevisionId>,
        lock_holder: Option<&UserId>,
        now: DateTime<Utc>,
    ) -> Result<Revision>;

    /// The head revision of every path that has one, ordered by path.
    async fn list_head_revisions(&self, repo: &RepoId) -> Result<Vec<Revision>>;

    async fn history(&self, repo: &RepoId, path: &RepoPath) -> Result<Vec<Revision>>;
}
