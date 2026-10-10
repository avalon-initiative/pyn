use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::error::Result;
use crate::history::HistoryCursor;
use crate::limits::{RepoUsage, StorageCap, Usage};
use crate::repo::{RepoRecord, RepoUpdate};
use crate::rules::PathFilter;
use crate::types::{Lock, NewRevision, RepoId, RepoPath, Revision, RevisionId, UserId};

/// Persistence for the repository registry, locks and revisions. Each method is one atomic check-and-write; policy lives in
/// `RepoService`. A lock with `expires_at <= now` counts as absent.
#[async_trait]
pub trait MetadataStore: Send + Sync {
    /// Take or renew the lock; `LockHeld` if someone else holds a live one. Taking a new lock while `owner` already holds
    /// `max_locks` live ones in the repository is `LockLimitReached`; renewing never is.
    async fn acquire_lock(
        &self,
        repo: &RepoId,
        path: &RepoPath,
        owner: &UserId,
        now: DateTime<Utc>,
        expires_at: DateTime<Utc>,
        max_locks: u32,
    ) -> Result<Lock>;

    /// `NotLockHolder` if there is no live lock or another user holds it.
    async fn release_lock(
        &self,
        repo: &RepoId,
        path: &RepoPath,
        owner: &UserId,
        now: DateTime<Utc>,
    ) -> Result<()>;

    /// Removes a live lock whatever its owner and returns it; `None` if the path is not locked.
    async fn force_release_lock(
        &self,
        repo: &RepoId,
        path: &RepoPath,
        now: DateTime<Utc>,
    ) -> Result<Option<Lock>>;

    async fn get_lock(
        &self,
        repo: &RepoId,
        path: &RepoPath,
        now: DateTime<Utc>,
    ) -> Result<Option<Lock>>;

    async fn list_locks(&self, repo: &RepoId, now: DateTime<Utc>) -> Result<Vec<Lock>>;

    /// The live locks `owner` holds in every repository, ordered by repository id then path.
    async fn list_locks_of(
        &self,
        owner: &UserId,
        now: DateTime<Utc>,
    ) -> Result<Vec<(RepoId, Lock)>>;

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
    ) -> Result<Revision> {
        self.commit_revision_capped(repo, revision, expected_head, lock_holder, now, None)
            .await
    }

    /// Like `commit_revision`, but content new to the repository that would take the owner's stored bytes past
    /// `cap` is `StorageLimitReached`; the check and the append are one atomic step.
    async fn commit_revision_capped(
        &self,
        repo: &RepoId,
        revision: NewRevision,
        expected_head: Option<RevisionId>,
        lock_holder: Option<&UserId>,
        now: DateTime<Utc>,
        cap: Option<StorageCap>,
    ) -> Result<Revision>;

    async fn get_revision(
        &self,
        repo: &RepoId,
        path: &RepoPath,
        id: RevisionId,
    ) -> Result<Option<Revision>>;

    /// The head revision of every path that has one, ordered by path.
    async fn list_head_revisions(&self, repo: &RepoId) -> Result<Vec<Revision>>;

    /// One path's revisions, oldest first.
    async fn history(&self, repo: &RepoId, path: &RepoPath) -> Result<Vec<Revision>>;

    /// At most `limit` revisions of the repository matching `filter`, newest first by (created_at, path, id) with paths
    /// compared bytewise, strictly before `before` when given.
    async fn repo_history(
        &self,
        repo: &RepoId,
        filter: Option<&PathFilter>,
        before: Option<&HistoryCursor>,
        limit: usize,
    ) -> Result<Vec<Revision>>;

    /// Registers the repository; `RepoExists` if the owner already has one with that name.
    async fn create_repo(&self, repo: RepoRecord) -> Result<RepoRecord> {
        self.create_repo_capped(repo, None).await
    }

    /// Like `create_repo`, but an owner that already has `max_repos` repositories gets `RepoLimitReached`; the
    /// count and the insert are one atomic step.
    async fn create_repo_capped(
        &self,
        repo: RepoRecord,
        max_repos: Option<u64>,
    ) -> Result<RepoRecord>;

    async fn find_repo(&self, owner: &UserId, name: &str) -> Result<Option<RepoRecord>>;

    async fn get_repo(&self, id: &RepoId) -> Result<Option<RepoRecord>>;

    /// Ordered by owner, then name.
    async fn list_repos(&self, owner: Option<&UserId>) -> Result<Vec<RepoRecord>>;

    /// At most `limit` public repositories ordered by owner, then name, strictly after `after` (owner, name) when given.
    async fn list_public_repos(
        &self,
        after: Option<(&UserId, &str)>,
        limit: usize,
    ) -> Result<Vec<RepoRecord>>;

    /// `RepoNotFound` for an unknown id, `RepoExists` if the new name is taken; a rejected update changes nothing.
    async fn update_repo(&self, id: &RepoId, update: RepoUpdate) -> Result<RepoRecord>;

    /// Removes the repository with its locks and revisions; false if it is not registered.
    async fn delete_repo(&self, id: &RepoId) -> Result<bool>;

    /// Repositories and stored bytes of everything the owner owns; `members` is left unset. Stored bytes count
    /// each distinct content once per repository.
    async fn owner_usage(&self, owner: &UserId) -> Result<Usage>;

    async fn repo_usage(&self, repo: &RepoId) -> Result<RepoUsage>;
}
