use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::branch::{Branch, DEFAULT_BRANCH};
use crate::error::Result;
use crate::history::{HistoryCursor, HistoryScan};
use crate::limits::{RepoUsage, StorageCap, Usage};
use crate::merge::{MergeWrite, MergeWritten};
use crate::repo::{RepoRecord, RepoUpdate};
use crate::rules::PathFilter;
use crate::tree::HeadCounts;
use crate::webhook::EventDraft;
use pyn_object::ObjectId;

use crate::types::{
    ChangesetItem, Commit, CommitFilter, Lock, NewRevision, PushedCommit, RepoId, RepoPath,
    Revision, RevisionId, UserId,
};

/// A hold on part of a repository's unreferenced-upload budget while an upload streams in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UploadReservation(pub i64);

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

    /// The path's newest revision on any branch: the end of its single line of history.
    async fn head_revision(&self, repo: &RepoId, path: &RepoPath) -> Result<Option<Revision>>;

    /// Append a revision on the default branch; see `commit_changeset_on`.
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
    ) -> Result<Revision> {
        let item = ChangesetItem {
            revision,
            expected_head,
            lock_holder: lock_holder.cloned(),
        };
        let mut revs = self
            .commit_changeset_on(repo, DEFAULT_BRANCH, vec![item], now, cap)
            .await?;
        Ok(revs.remove(0))
    }

    /// Appends every item's revision as one commit on the default branch, or none.
    async fn commit_changeset_capped(
        &self,
        repo: &RepoId,
        items: Vec<ChangesetItem>,
        now: DateTime<Utc>,
        cap: Option<StorageCap>,
    ) -> Result<Vec<Revision>> {
        self.commit_changeset_on(repo, DEFAULT_BRANCH, items, now, cap)
            .await
    }

    /// Appends every item's revision as one commit on `branch` and moves the branch to it, or writes nothing: the
    /// first item that fails its stale-base, lock or storage check is the error. Paths are distinct; each exclusive
    /// item's lock is released with its append. A shared item's base is its revision at the branch head; an
    /// exclusive item's is the path's newest revision on any branch, so its history stays one line. `BranchNotFound`
    /// for an unknown branch.
    async fn commit_changeset_on(
        &self,
        repo: &RepoId,
        branch: &str,
        items: Vec<ChangesetItem>,
        now: DateTime<Utc>,
        cap: Option<StorageCap>,
    ) -> Result<Vec<Revision>>;

    /// Appends the commits, oldest first, to `branch` and moves it to the last, or writes nothing. The first commit's
    /// parent must be the branch head or an ancestor of it, each next one's the one before, and the last commit must
    /// contain the head (`NotFastForward`); a second parent must exist (`PushInvalid`). No commit may already be in
    /// the repository (`PushInvalid`), except a leading run of `known` ones from another branch: they only move the
    /// branch and must not be in its history (`NotFastForward`). `pusher` must hold a live lock on each exclusive
    /// path (`LockRequired`, `LockHeld`), consumed with the path's last revision. A commit's `unlocked` paths need no
    /// lock; its `adopted` revisions become the paths' pointers. Each revision's base is the pointer at the commit's
    /// first parent, as the commits before it leave it (decision #299).
    async fn push_commits_on(
        &self,
        repo: &RepoId,
        branch: &str,
        commits: Vec<PushedCommit>,
        pusher: &UserId,
        now: DateTime<Utc>,
        cap: Option<StorageCap>,
    ) -> Result<Vec<Revision>>;

    /// Writes a merge commit on `branch` or nothing: its parents are the head, which must be `expected_head`
    /// (`MergeStale` otherwise), and `other`. Adopted and new revisions go in its tree and become the paths' pointers.
    async fn commit_merge_on(
        &self,
        repo: &RepoId,
        branch: &str,
        merge: MergeWrite,
        now: DateTime<Utc>,
        cap: Option<StorageCap>,
    ) -> Result<MergeWritten>;

    /// Moves `branch` from `from` to `to`, a descendant of it: no new commit. `MergeStale` if the branch is no
    /// longer at `from`.
    async fn fast_forward_branch(
        &self,
        repo: &RepoId,
        branch: &str,
        from: Option<&ObjectId>,
        to: &ObjectId,
    ) -> Result<()>;

    /// The lowest common ancestors of `a` and `b` through every parent: their common ancestors (each commit counts
    /// as its own ancestor) that no other common ancestor descends from, ordered by id.
    async fn merge_bases(&self, repo: &RepoId, a: &ObjectId, b: &ObjectId)
    -> Result<Vec<ObjectId>>;

    /// The branches of the repository, the default one first, then by name.
    async fn list_branches(&self, repo: &RepoId) -> Result<Vec<Branch>>;

    async fn get_branch(&self, repo: &RepoId, name: &str) -> Result<Option<Branch>>;

    /// A branch at commit `at`; `BranchExists` if the name is taken, `CommitNotFound` if `at` is not a commit.
    async fn create_branch(
        &self,
        repo: &RepoId,
        name: &str,
        at: &ObjectId,
        by: &UserId,
        now: DateTime<Utc>,
    ) -> Result<Branch>;

    /// Removes the name only. `BranchNotFound`, or `InvalidRequest` for the default branch or when `expected` is given
    /// and the branch has moved off it.
    async fn delete_branch(
        &self,
        repo: &RepoId,
        name: &str,
        expected: Option<&ObjectId>,
    ) -> Result<()>;

    /// `BranchNotFound`, `BranchExists`, or `InvalidRequest` for the default branch.
    async fn rename_branch(&self, repo: &RepoId, from: &str, to: &str) -> Result<Branch>;

    async fn get_revision(
        &self,
        repo: &RepoId,
        path: &RepoPath,
        id: RevisionId,
    ) -> Result<Option<Revision>>;

    async fn get_commit(&self, repo: &RepoId, id: &ObjectId) -> Result<Option<Commit>>;

    /// Ids of at most `limit` commits whose hex starts with `prefix` (lowercase).
    async fn commits_by_prefix(
        &self,
        repo: &RepoId,
        prefix: &str,
        limit: usize,
    ) -> Result<Vec<ObjectId>>;

    /// At most `limit` commits, newest first, strictly older than commit `before` when given: those reachable from
    /// `tip` (itself included), or every commit of the repository when `tip` is `None`.
    async fn commits(
        &self,
        repo: &RepoId,
        tip: Option<&ObjectId>,
        before: Option<&ObjectId>,
        limit: usize,
    ) -> Result<Vec<Commit>>;

    /// Like `commits`, keeping only those `filter` matches; `limit` counts matches, so a page is full while more exist.
    async fn commits_matching(
        &self,
        repo: &RepoId,
        tip: Option<&ObjectId>,
        before: Option<&ObjectId>,
        filter: &CommitFilter,
        limit: usize,
    ) -> Result<Vec<Commit>>;

    /// Commits reachable from `tips` and not from `haves` (unknown ids are ignored), parents before children in the
    /// order stored, only those stored after commit `after`, at most `limit`. `InvalidRequest` when either walk would
    /// pass `max_examined` commits.
    async fn missing_commits(
        &self,
        repo: &RepoId,
        tips: &[ObjectId],
        haves: &[ObjectId],
        after: Option<&ObjectId>,
        limit: usize,
        max_examined: usize,
    ) -> Result<Vec<Commit>>;

    /// Whether `ancestor` is `descendant` or one of its ancestors.
    async fn is_ancestor(
        &self,
        repo: &RepoId,
        ancestor: &ObjectId,
        descendant: &ObjectId,
    ) -> Result<bool>;

    /// The path's revision at `commit`: its highest-numbered revision made in the commit or an ancestor. A deletion
    /// revision counts: it is the pointer a later write is based on.
    async fn revision_at(
        &self,
        repo: &RepoId,
        commit: &ObjectId,
        path: &RepoPath,
    ) -> Result<Option<Revision>>;

    /// The revisions the commit's pointer cache records, ordered by path: the paths whose revision differs from its
    /// first parent's, including those a merge adopted from the other parent.
    async fn commit_pointers(&self, repo: &RepoId, commit: &ObjectId) -> Result<Vec<Revision>>;

    /// Whether a revision of the repository, on any branch, has `content`.
    async fn has_content(&self, repo: &RepoId, content: &ObjectId) -> Result<bool>;

    /// Records that `repo` may reference the uploaded object `id` of `size` bytes. A new record that takes the
    /// repository's unreferenced uploads past `cap` bytes is `StorageLimitReached` and is not kept. On success the
    /// upload's `reservation` is consumed and its bytes stop counting as held; any newly counted unreferenced bytes
    /// are added in the same step. On failure the reservation survives and stays releasable.
    async fn record_upload(
        &self,
        repo: &RepoId,
        id: &ObjectId,
        size: u64,
        cap: u64,
        reservation: UploadReservation,
    ) -> Result<()>;

    /// Whether any repository, this one included, has recorded the object `id`.
    async fn object_is_recorded(&self, id: &ObjectId) -> Result<bool>;

    /// Like `record_upload` for the object assembled from `chunks`, which must all be known to `repo` (else
    /// `ObjectMissing`); the chunks stop counting as unreferenced uploads.
    async fn record_assembled(
        &self,
        repo: &RepoId,
        id: &ObjectId,
        size: u64,
        chunks: &[ObjectId],
        cap: u64,
    ) -> Result<()>;

    /// Holds `bytes` against `repo`'s unreferenced-upload budget, counting the uploads still in flight, until released
    /// or `ttl_secs` after `now`. Over `cap` is `StorageLimitReached`; the check and the hold are one atomic step.
    async fn reserve_upload(
        &self,
        repo: &RepoId,
        bytes: u64,
        cap: u64,
        now: DateTime<Utc>,
        ttl_secs: i64,
    ) -> Result<UploadReservation>;

    /// Drops a hold; releasing one that is gone is not an error.
    async fn release_upload(&self, repo: &RepoId, reservation: UploadReservation) -> Result<()>;

    /// The ids among `ids` that `repo` may not reference: neither uploaded through it nor named by one of its
    /// revisions. Says nothing about what the object store holds.
    async fn unknown_objects(&self, repo: &RepoId, ids: &[ObjectId]) -> Result<Vec<ObjectId>>;

    /// Bytes `repo` holds in uploads that no revision of it names yet, assembled chunks excluded.
    async fn unreferenced_bytes(&self, repo: &RepoId) -> Result<u64>;

    /// The body of a stored git tree, as `commit.tree` and the entries inside it name them.
    async fn get_tree(&self, repo: &RepoId, id: &ObjectId) -> Result<Option<Vec<u8>>>;

    /// The repository's state as of `commit`: for every path it holds, its highest-numbered revision made in `commit`
    /// or an ancestor, ordered by path. A path whose revision deletes it is left out. `commit` must exist.
    async fn snapshot(&self, repo: &RepoId, commit: &ObjectId) -> Result<Vec<Revision>>;

    /// `snapshot` limited to paths that start with `prefix` and sort after `after`, at most `limit` of them.
    async fn snapshot_page(
        &self,
        repo: &RepoId,
        commit: &ObjectId,
        prefix: Option<&str>,
        after: Option<&RepoPath>,
        limit: usize,
    ) -> Result<Vec<Revision>>;

    /// The counts of `commit`'s snapshot; the cost does not grow with the repository. `CommitNotFound` for an unknown commit.
    async fn head_counts(&self, repo: &RepoId, commit: &ObjectId) -> Result<HeadCounts>;

    /// The newest revision of each path that has one, on any branch, ordered by path, for paths after `after`; at most `limit`.
    async fn list_head_revisions(
        &self,
        repo: &RepoId,
        after: Option<&RepoPath>,
        limit: usize,
    ) -> Result<Vec<Revision>>;

    /// One path's revisions, oldest first; with `within`, only those made in that commit or an ancestor.
    async fn history(
        &self,
        repo: &RepoId,
        path: &RepoPath,
        within: Option<&ObjectId>,
    ) -> Result<Vec<Revision>>;

    /// One path's revisions numbered above `after`, oldest first, at most `limit`; with `within`, only those made in
    /// that commit or an ancestor.
    async fn history_page(
        &self,
        repo: &RepoId,
        path: &RepoPath,
        within: Option<&ObjectId>,
        after: Option<RevisionId>,
        limit: usize,
    ) -> Result<Vec<Revision>>;

    /// At most `limit` revisions of the repository matching `filter`, newest first by (created_at, path, id) with paths
    /// compared bytewise, strictly before `before` when given; with `within`, only those made in that commit or an
    /// ancestor. The scan examines at most `budget` revisions; if that ends it before `limit` matches, `resume`
    /// is the position after the last one examined.
    async fn repo_history(
        &self,
        repo: &RepoId,
        within: Option<&ObjectId>,
        filter: Option<&PathFilter>,
        before: Option<&HistoryCursor>,
        limit: usize,
        budget: usize,
    ) -> Result<HistoryScan>;

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
    ) -> Result<RepoRecord> {
        self.create_repo_capped_with_events(repo, max_repos, &[])
            .await
    }

    /// Like `create_repo_capped`, queueing `events` in the same transaction; a refused create queues nothing.
    async fn create_repo_capped_with_events(
        &self,
        repo: RepoRecord,
        max_repos: Option<u64>,
        events: &[EventDraft],
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
    async fn update_repo(&self, id: &RepoId, update: RepoUpdate) -> Result<RepoRecord> {
        self.update_repo_with_events(id, update, &[]).await
    }

    /// Like `update_repo`, queueing `events` in the same transaction; a rejected update queues nothing.
    async fn update_repo_with_events(
        &self,
        id: &RepoId,
        update: RepoUpdate,
        events: &[EventDraft],
    ) -> Result<RepoRecord>;

    /// Removes the repository with its locks and revisions; false if it is not registered.
    async fn delete_repo(&self, id: &RepoId) -> Result<bool> {
        self.delete_repo_with_events(id, &[]).await
    }

    /// Like `delete_repo`, queueing `events` in the same transaction when a repository was removed.
    async fn delete_repo_with_events(&self, id: &RepoId, events: &[EventDraft]) -> Result<bool>;

    /// Repositories and stored bytes of everything the owner owns; `members` is left unset. Stored bytes count
    /// each distinct content once per repository.
    async fn owner_usage(&self, owner: &UserId) -> Result<Usage>;

    async fn repo_usage(&self, repo: &RepoId) -> Result<RepoUsage>;
}
