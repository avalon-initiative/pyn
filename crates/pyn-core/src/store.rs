use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::branch::{Branch, DEFAULT_BRANCH};
use crate::error::Result;
use crate::history::HistoryCursor;
use crate::limits::{RepoUsage, StorageCap, Usage};
use crate::merge::{MergeWrite, MergeWritten};
use crate::repo::{RepoRecord, RepoUpdate};
use crate::rules::PathFilter;
use pyn_object::ObjectId;

use crate::types::{
    ChangesetItem, Commit, Lock, NewRevision, PushedCommit, RepoId, RepoPath, Revision, RevisionId,
    UserId,
};

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
    /// contain the head (`NotFastForward`); a second parent must exist and no commit may (`PushInvalid`). `pusher`
    /// must hold a live lock on each exclusive path (`LockRequired`, `LockHeld`), consumed with the path's last
    /// revision. A commit's `unlocked` paths need no lock; its `adopted` revisions become the paths' pointers. Each
    /// revision's base is the pointer at the commit's first parent, as the commits before it leave it (decision #299).
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
    /// repository's unreferenced uploads past `cap` bytes is `StorageLimitReached` and is not kept.
    async fn record_upload(&self, repo: &RepoId, id: &ObjectId, size: u64, cap: u64) -> Result<()>;

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

    /// The newest revision of every path that has one, on any branch, ordered by path.
    async fn list_head_revisions(&self, repo: &RepoId) -> Result<Vec<Revision>>;

    /// One path's revisions, oldest first; with `within`, only those made in that commit or an ancestor.
    async fn history(
        &self,
        repo: &RepoId,
        path: &RepoPath,
        within: Option<&ObjectId>,
    ) -> Result<Vec<Revision>>;

    /// At most `limit` revisions of the repository matching `filter`, newest first by (created_at, path, id) with paths
    /// compared bytewise, strictly before `before` when given; with `within`, only those made in that commit or an
    /// ancestor.
    async fn repo_history(
        &self,
        repo: &RepoId,
        within: Option<&ObjectId>,
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
