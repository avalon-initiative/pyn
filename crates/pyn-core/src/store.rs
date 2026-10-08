use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::error::Result;
use crate::types::{Lock, NewRevision, RepoId, RepoPath, Revision, RevisionId, UserId};

/// Persistence for locks and revisions.
///
/// Each method is an **atomic primitive**: an implementation must make the check-and-write
/// indivisible (one SQL statement or transaction), because two clients racing for the same
/// lock or the same head is the central correctness case of this system. Policy (which paths
/// are exclusive, lease length, who may do what) lives in `RepoService`, never here.
///
/// Expired locks are invisible: every method takes `now` and treats a lock with
/// `expires_at <= now` as absent.
#[async_trait]
pub trait MetadataStore: Send + Sync {
    /// Take the lock, or renew it if `owner` already holds it. A live lock held by someone
    /// else fails with `LockHeld`. An expired lock is replaced.
    async fn acquire_lock(
        &self,
        repo: &RepoId,
        path: &RepoPath,
        owner: &UserId,
        now: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> Result<Lock>;

    /// Release a lock held by `owner`. Fails with `NotLockHolder` if there is no live lock or
    /// someone else holds it.
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

    /// Append a revision iff the path's current head is `expected_head` (`None` = no revision
    /// yet), otherwise `StaleBase`. If `lock_holder` is `Some`, also require in the same
    /// atomic step that this user holds the live lock (`LockRequired` if no live lock,
    /// `LockHeld` if another user holds it), and release that lock on success.
    async fn commit_revision(
        &self,
        repo: &RepoId,
        revision: NewRevision,
        expected_head: Option<RevisionId>,
        lock_holder: Option<&UserId>,
        now: DateTime<Utc>,
    ) -> Result<Revision>;

    /// All revisions of a path, oldest first.
    async fn history(&self, repo: &RepoId, path: &RepoPath) -> Result<Vec<Revision>>;
}
