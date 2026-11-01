//! A store wrapper that counts the calls made through it, to show a request's cost does not grow with history.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use chrono::Utc;

use super::Store;
use crate::RevisionId;
use crate::{MetadataStore, ObjectId, RepoId, RepoPath, RepoRecord, RepoUpdate, UserId};

pub struct CountingStore {
    inner: Store,
    calls: AtomicUsize,
}

impl CountingStore {
    pub fn new(inner: Store) -> Arc<Self> {
        Arc::new(Self {
            inner,
            calls: AtomicUsize::new(0),
        })
    }

    fn count(&self) {
        self.calls.fetch_add(1, Ordering::Relaxed);
    }

    /// The calls since the last reading.
    pub fn take(&self) -> usize {
        self.calls.swap(0, Ordering::Relaxed)
    }
}

#[async_trait::async_trait]
impl MetadataStore for CountingStore {
    async fn acquire_lock_with_events(
        &self,
        repo: &RepoId,
        path: &RepoPath,
        owner: &UserId,
        now: chrono::DateTime<Utc>,
        expires_at: chrono::DateTime<Utc>,
        max_locks: u32,
        events: Option<crate::LockEventBuilder<'_>>,
    ) -> crate::Result<crate::Lock> {
        self.count();
        self.inner
            .acquire_lock_with_events(repo, path, owner, now, expires_at, max_locks, events)
            .await
    }

    async fn release_lock_with_events(
        &self,
        repo: &RepoId,
        path: &RepoPath,
        owner: &UserId,
        now: chrono::DateTime<Utc>,
        events: Option<crate::LockEventBuilder<'_>>,
    ) -> crate::Result<()> {
        self.count();
        self.inner
            .release_lock_with_events(repo, path, owner, now, events)
            .await
    }

    async fn force_release_lock_with_events(
        &self,
        repo: &RepoId,
        path: &RepoPath,
        now: chrono::DateTime<Utc>,
        events: Option<crate::LockEventBuilder<'_>>,
    ) -> crate::Result<Option<crate::Lock>> {
        self.count();
        self.inner
            .force_release_lock_with_events(repo, path, now, events)
            .await
    }

    async fn commit_changeset_on_with_events(
        &self,
        repo: &RepoId,
        branch: &str,
        items: Vec<crate::ChangesetItem>,
        now: chrono::DateTime<Utc>,
        cap: Option<crate::StorageCap>,
        events: Option<crate::LockEventBuilder<'_>>,
    ) -> crate::Result<Vec<crate::Revision>> {
        self.count();
        self.inner
            .commit_changeset_on_with_events(repo, branch, items, now, cap, events)
            .await
    }

    async fn push_commits_on_with_events(
        &self,
        repo: &RepoId,
        branch: &str,
        commits: Vec<crate::PushedCommit>,
        pusher: &UserId,
        now: chrono::DateTime<Utc>,
        cap: Option<crate::StorageCap>,
        events: Option<crate::LockEventBuilder<'_>>,
    ) -> crate::Result<Vec<crate::Revision>> {
        self.count();
        self.inner
            .push_commits_on_with_events(repo, branch, commits, pusher, now, cap, events)
            .await
    }

    async fn acquire_lock(
        &self,
        repo: &RepoId,
        path: &RepoPath,
        owner: &UserId,
        now: chrono::DateTime<Utc>,
        expires_at: chrono::DateTime<Utc>,
        max_locks: u32,
    ) -> crate::Result<crate::Lock> {
        self.count();
        self.inner
            .acquire_lock(repo, path, owner, now, expires_at, max_locks)
            .await
    }

    async fn release_lock(
        &self,
        repo: &RepoId,
        path: &RepoPath,
        owner: &UserId,
        now: chrono::DateTime<Utc>,
    ) -> crate::Result<()> {
        self.count();
        self.inner.release_lock(repo, path, owner, now).await
    }

    async fn force_release_lock(
        &self,
        repo: &RepoId,
        path: &RepoPath,
        now: chrono::DateTime<Utc>,
    ) -> crate::Result<Option<crate::Lock>> {
        self.count();
        self.inner.force_release_lock(repo, path, now).await
    }

    async fn get_lock(
        &self,
        repo: &RepoId,
        path: &RepoPath,
        now: chrono::DateTime<Utc>,
    ) -> crate::Result<Option<crate::Lock>> {
        self.count();
        self.inner.get_lock(repo, path, now).await
    }

    async fn list_locks(
        &self,
        repo: &RepoId,
        now: chrono::DateTime<Utc>,
    ) -> crate::Result<Vec<crate::Lock>> {
        self.count();
        self.inner.list_locks(repo, now).await
    }

    async fn list_locks_of(
        &self,
        owner: &UserId,
        now: chrono::DateTime<Utc>,
    ) -> crate::Result<Vec<(RepoId, crate::Lock)>> {
        self.count();
        self.inner.list_locks_of(owner, now).await
    }

    async fn head_revision(
        &self,
        repo: &RepoId,
        path: &RepoPath,
    ) -> crate::Result<Option<crate::Revision>> {
        self.count();
        self.inner.head_revision(repo, path).await
    }

    async fn commit_changeset_on(
        &self,
        repo: &RepoId,
        branch: &str,
        items: Vec<crate::ChangesetItem>,
        now: chrono::DateTime<Utc>,
        cap: Option<crate::StorageCap>,
    ) -> crate::Result<Vec<crate::Revision>> {
        self.count();
        self.inner
            .commit_changeset_on(repo, branch, items, now, cap)
            .await
    }

    async fn push_commits_on(
        &self,
        repo: &RepoId,
        branch: &str,
        commits: Vec<crate::PushedCommit>,
        pusher: &UserId,
        now: chrono::DateTime<Utc>,
        cap: Option<crate::StorageCap>,
    ) -> crate::Result<Vec<crate::Revision>> {
        self.count();
        self.inner
            .push_commits_on(repo, branch, commits, pusher, now, cap)
            .await
    }

    async fn commit_merge_on(
        &self,
        repo: &RepoId,
        branch: &str,
        merge: crate::merge::MergeWrite,
        now: chrono::DateTime<Utc>,
        cap: Option<crate::StorageCap>,
    ) -> crate::Result<crate::merge::MergeWritten> {
        self.count();
        self.inner
            .commit_merge_on(repo, branch, merge, now, cap)
            .await
    }

    async fn fast_forward_branch(
        &self,
        repo: &RepoId,
        branch: &str,
        from: Option<&ObjectId>,
        to: &ObjectId,
    ) -> crate::Result<()> {
        self.count();
        self.inner.fast_forward_branch(repo, branch, from, to).await
    }

    async fn merge_bases(
        &self,
        repo: &RepoId,
        a: &ObjectId,
        b: &ObjectId,
    ) -> crate::Result<Vec<ObjectId>> {
        self.count();
        self.inner.merge_bases(repo, a, b).await
    }

    async fn list_branches(&self, repo: &RepoId) -> crate::Result<Vec<crate::Branch>> {
        self.count();
        self.inner.list_branches(repo).await
    }

    async fn get_branch(&self, repo: &RepoId, name: &str) -> crate::Result<Option<crate::Branch>> {
        self.count();
        self.inner.get_branch(repo, name).await
    }

    async fn create_branch(
        &self,
        repo: &RepoId,
        name: &str,
        at: &ObjectId,
        by: &UserId,
        now: chrono::DateTime<Utc>,
    ) -> crate::Result<crate::Branch> {
        self.count();
        self.inner.create_branch(repo, name, at, by, now).await
    }

    async fn delete_branch(
        &self,
        repo: &RepoId,
        name: &str,
        expected: Option<&ObjectId>,
    ) -> crate::Result<()> {
        self.count();
        self.inner.delete_branch(repo, name, expected).await
    }

    async fn rename_branch(
        &self,
        repo: &RepoId,
        from: &str,
        to: &str,
    ) -> crate::Result<crate::Branch> {
        self.count();
        self.inner.rename_branch(repo, from, to).await
    }

    async fn get_commit(
        &self,
        repo: &RepoId,
        id: &ObjectId,
    ) -> crate::Result<Option<crate::Commit>> {
        self.count();
        self.inner.get_commit(repo, id).await
    }

    async fn commits_by_prefix(
        &self,
        repo: &RepoId,
        prefix: &str,
        limit: usize,
    ) -> crate::Result<Vec<ObjectId>> {
        self.count();
        self.inner.commits_by_prefix(repo, prefix, limit).await
    }

    async fn commits(
        &self,
        repo: &RepoId,
        tip: Option<&ObjectId>,
        before: Option<&ObjectId>,
        limit: usize,
    ) -> crate::Result<Vec<crate::Commit>> {
        self.count();
        self.inner.commits(repo, tip, before, limit).await
    }

    async fn commits_matching(
        &self,
        repo: &RepoId,
        tip: Option<&ObjectId>,
        before: Option<&ObjectId>,
        filter: &crate::CommitFilter,
        limit: usize,
    ) -> crate::Result<Vec<crate::Commit>> {
        self.count();
        self.inner
            .commits_matching(repo, tip, before, filter, limit)
            .await
    }

    async fn missing_commits(
        &self,
        repo: &RepoId,
        tips: &[ObjectId],
        haves: &[ObjectId],
        after: Option<&ObjectId>,
        limit: usize,
        max_examined: usize,
    ) -> crate::Result<Vec<crate::Commit>> {
        self.count();
        self.inner
            .missing_commits(repo, tips, haves, after, limit, max_examined)
            .await
    }

    async fn is_ancestor(
        &self,
        repo: &RepoId,
        ancestor: &ObjectId,
        descendant: &ObjectId,
    ) -> crate::Result<bool> {
        self.count();
        self.inner.is_ancestor(repo, ancestor, descendant).await
    }

    async fn revision_at(
        &self,
        repo: &RepoId,
        commit: &ObjectId,
        path: &RepoPath,
    ) -> crate::Result<Option<crate::Revision>> {
        self.count();
        self.inner.revision_at(repo, commit, path).await
    }

    async fn commit_pointers(
        &self,
        repo: &RepoId,
        commit: &ObjectId,
    ) -> crate::Result<Vec<crate::Revision>> {
        self.count();
        self.inner.commit_pointers(repo, commit).await
    }

    async fn has_content(&self, repo: &RepoId, content: &ObjectId) -> crate::Result<bool> {
        self.count();
        self.inner.has_content(repo, content).await
    }

    async fn record_upload(
        &self,
        repo: &RepoId,
        id: &ObjectId,
        size: u64,
        cap: u64,
        reservation: crate::UploadReservation,
    ) -> crate::Result<()> {
        self.count();
        self.inner
            .record_upload(repo, id, size, cap, reservation)
            .await
    }

    async fn record_assembled(
        &self,
        repo: &RepoId,
        id: &ObjectId,
        size: u64,
        chunks: &[ObjectId],
        cap: u64,
    ) -> crate::Result<()> {
        self.count();
        self.inner
            .record_assembled(repo, id, size, chunks, cap)
            .await
    }

    async fn record_lfs_object(
        &self,
        repo: &RepoId,
        object: &crate::LfsObject,
    ) -> crate::Result<bool> {
        self.count();
        self.inner.record_lfs_object(repo, object).await
    }

    async fn lfs_object(
        &self,
        repo: &RepoId,
        oid: &str,
    ) -> crate::Result<Option<crate::LfsObject>> {
        self.count();
        self.inner.lfs_object(repo, oid).await
    }

    async fn lfs_bytes(&self, repo: &RepoId) -> crate::Result<u64> {
        self.count();
        self.inner.lfs_bytes(repo).await
    }

    async fn object_is_recorded(&self, id: &ObjectId) -> crate::Result<bool> {
        self.count();
        self.inner.object_is_recorded(id).await
    }

    async fn unknown_objects(
        &self,
        repo: &RepoId,
        ids: &[ObjectId],
    ) -> crate::Result<Vec<ObjectId>> {
        self.count();
        self.inner.unknown_objects(repo, ids).await
    }

    async fn reserve_upload(
        &self,
        repo: &RepoId,
        bytes: u64,
        cap: u64,
        now: chrono::DateTime<chrono::Utc>,
        ttl_secs: i64,
    ) -> crate::Result<crate::UploadReservation> {
        self.count();
        self.inner
            .reserve_upload(repo, bytes, cap, now, ttl_secs)
            .await
    }

    async fn release_upload(
        &self,
        repo: &RepoId,
        reservation: crate::UploadReservation,
    ) -> crate::Result<()> {
        self.count();
        self.inner.release_upload(repo, reservation).await
    }

    async fn unreferenced_bytes(&self, repo: &RepoId) -> crate::Result<u64> {
        self.count();
        self.inner.unreferenced_bytes(repo).await
    }

    async fn get_tree(&self, repo: &RepoId, id: &ObjectId) -> crate::Result<Option<Vec<u8>>> {
        self.count();
        self.inner.get_tree(repo, id).await
    }

    async fn snapshot(
        &self,
        repo: &RepoId,
        commit: &ObjectId,
    ) -> crate::Result<Vec<crate::Revision>> {
        self.count();
        self.inner.snapshot(repo, commit).await
    }

    async fn get_revision(
        &self,
        repo: &RepoId,
        path: &RepoPath,
        id: RevisionId,
    ) -> crate::Result<Option<crate::Revision>> {
        self.count();
        self.inner.get_revision(repo, path, id).await
    }

    async fn snapshot_page(
        &self,
        repo: &RepoId,
        commit: &ObjectId,
        prefix: Option<&str>,
        after: Option<&RepoPath>,
        limit: usize,
    ) -> crate::Result<Vec<crate::Revision>> {
        self.count();
        self.inner
            .snapshot_page(repo, commit, prefix, after, limit)
            .await
    }

    async fn head_counts(
        &self,
        repo: &RepoId,
        commit: &ObjectId,
    ) -> crate::Result<crate::HeadCounts> {
        self.count();
        self.inner.head_counts(repo, commit).await
    }

    async fn list_head_revisions(
        &self,
        repo: &RepoId,
        after: Option<&RepoPath>,
        limit: usize,
    ) -> crate::Result<Vec<crate::Revision>> {
        self.count();
        self.inner.list_head_revisions(repo, after, limit).await
    }

    async fn history(
        &self,
        repo: &RepoId,
        path: &RepoPath,
        within: Option<&ObjectId>,
    ) -> crate::Result<Vec<crate::Revision>> {
        self.count();
        self.inner.history(repo, path, within).await
    }

    async fn history_page(
        &self,
        repo: &RepoId,
        path: &RepoPath,
        within: Option<&ObjectId>,
        after: Option<RevisionId>,
        limit: usize,
    ) -> crate::Result<Vec<crate::Revision>> {
        self.count();
        self.inner
            .history_page(repo, path, within, after, limit)
            .await
    }

    async fn repo_history(
        &self,
        repo: &RepoId,
        within: Option<&ObjectId>,
        filter: Option<&crate::PathFilter>,
        before: Option<&crate::HistoryCursor>,
        limit: usize,
        budget: usize,
    ) -> crate::Result<crate::HistoryScan> {
        self.count();
        self.inner
            .repo_history(repo, within, filter, before, limit, budget)
            .await
    }

    async fn create_repo_capped_with_events(
        &self,
        repo: RepoRecord,
        max_repos: Option<u64>,
        events: &[crate::EventDraft],
    ) -> crate::Result<RepoRecord> {
        self.count();
        self.inner
            .create_repo_capped_with_events(repo, max_repos, events)
            .await
    }

    async fn find_repo(&self, owner: &UserId, name: &str) -> crate::Result<Option<RepoRecord>> {
        self.count();
        self.inner.find_repo(owner, name).await
    }

    async fn get_repo(&self, id: &RepoId) -> crate::Result<Option<RepoRecord>> {
        self.count();
        self.inner.get_repo(id).await
    }

    async fn list_repos(&self, owner: Option<&UserId>) -> crate::Result<Vec<RepoRecord>> {
        self.count();
        self.inner.list_repos(owner).await
    }

    async fn list_public_repos(
        &self,
        after: Option<(&UserId, &str)>,
        limit: usize,
    ) -> crate::Result<Vec<RepoRecord>> {
        self.count();
        self.inner.list_public_repos(after, limit).await
    }

    async fn update_repo_with_events(
        &self,
        id: &RepoId,
        update: RepoUpdate,
        events: &[crate::EventDraft],
    ) -> crate::Result<RepoRecord> {
        self.count();
        self.inner.update_repo_with_events(id, update, events).await
    }

    async fn delete_repo_with_events(
        &self,
        id: &RepoId,
        events: &[crate::EventDraft],
    ) -> crate::Result<bool> {
        self.count();
        self.inner.delete_repo_with_events(id, events).await
    }

    async fn owner_usage(&self, owner: &UserId) -> crate::Result<crate::Usage> {
        self.count();
        self.inner.owner_usage(owner).await
    }

    async fn repo_usage(&self, repo: &RepoId) -> crate::Result<crate::RepoUsage> {
        self.count();
        self.inner.repo_usage(repo).await
    }

    async fn repo_breakdown(&self, repo: &RepoId) -> crate::Result<crate::RepoBreakdown> {
        self.count();
        self.inner.repo_breakdown(repo).await
    }
}
