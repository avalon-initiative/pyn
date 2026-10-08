use std::sync::Arc;

use chrono::Duration;

use crate::clock::Clock;
use crate::error::{PynError, Result};
use crate::object::ObjectStore;
use crate::rules::{Mode, Rules};
use crate::store::MetadataStore;
use crate::types::{
    ContentHash, Lock, NewRevision, RepoId, RepoPath, Revision, RevisionId, UserId,
};

#[derive(Debug, Clone)]
pub struct ServiceConfig {
    /// How long a checkout lasts before it expires on its own. Renewed by checking out again.
    pub lease: Duration,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self {
            lease: Duration::hours(8),
        }
    }
}

/// The server-side policy for one repository: which paths are exclusive (from `Rules`), who
/// may check out and check in, and what a checkin must prove. All enforcement lives here and in
/// the `MetadataStore` primitives; clients are never trusted.
///
/// Phase 1: one repo per service, a single mainline, rules fixed at construction.
pub struct RepoService {
    repo: RepoId,
    rules: Rules,
    meta: Arc<dyn MetadataStore>,
    objects: Arc<dyn ObjectStore>,
    clock: Arc<dyn Clock>,
    config: ServiceConfig,
}

impl RepoService {
    pub fn new(
        repo: RepoId,
        rules: Rules,
        meta: Arc<dyn MetadataStore>,
        objects: Arc<dyn ObjectStore>,
        clock: Arc<dyn Clock>,
        config: ServiceConfig,
    ) -> Self {
        Self {
            repo,
            rules,
            meta,
            objects,
            clock,
            config,
        }
    }

    pub fn repo(&self) -> &RepoId {
        &self.repo
    }

    pub fn mode_for(&self, path: &RepoPath) -> Mode {
        self.rules.mode_for(path)
    }

    /// Request the lock on an exclusive path. `base` is the revision the caller's copy is at
    /// (`None` = they have no copy). If it is not the head, the caller must update first, so
    /// nobody starts editing a stale copy. Checking out again as the holder renews the lease.
    pub async fn checkout(
        &self,
        path: &RepoPath,
        user: &UserId,
        base: Option<RevisionId>,
    ) -> Result<Lock> {
        if self.mode_for(path) != Mode::Exclusive {
            return Err(PynError::NotExclusive(path.clone()));
        }
        let head = self
            .meta
            .head_revision(&self.repo, path)
            .await?
            .map(|r| r.id);
        if head != base {
            return Err(PynError::StaleBase {
                path: path.clone(),
                expected: base,
                actual: head,
            });
        }
        let now = self.clock.now();
        self.meta
            .acquire_lock(&self.repo, path, user, now, now + self.config.lease)
            .await
    }

    /// Give up a lock without checking in.
    pub async fn release(&self, path: &RepoPath, user: &UserId) -> Result<()> {
        self.meta
            .release_lock(&self.repo, path, user, self.clock.now())
            .await
    }

    /// Record a new revision. Content must already be in the object store. Exclusive paths
    /// additionally require the caller's live lock, which is released by a successful checkin.
    /// Every path requires `base` to equal the current head, so a stale shared edit is
    /// rejected rather than silently overwriting someone's work (merge arrives in Phase 2).
    pub async fn checkin(
        &self,
        path: &RepoPath,
        user: &UserId,
        content: ContentHash,
        base: Option<RevisionId>,
        message: String,
    ) -> Result<Revision> {
        if !self.objects.exists(&content).await? {
            return Err(PynError::ObjectMissing(content.to_string()));
        }
        let now = self.clock.now();
        let lock_holder = (self.mode_for(path) == Mode::Exclusive).then_some(user);
        let revision = NewRevision {
            path: path.clone(),
            content,
            author: user.clone(),
            message,
            created_at: now,
        };
        self.meta
            .commit_revision(&self.repo, revision, base, lock_holder, now)
            .await
    }

    pub async fn locks(&self) -> Result<Vec<Lock>> {
        self.meta.list_locks(&self.repo, self.clock.now()).await
    }

    pub async fn head(&self, path: &RepoPath) -> Result<Option<Revision>> {
        self.meta.head_revision(&self.repo, path).await
    }

    pub async fn history(&self, path: &RepoPath) -> Result<Vec<Revision>> {
        self.meta.history(&self.repo, path).await
    }
}
