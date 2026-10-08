use std::collections::BTreeMap;
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

/// One path in a repository listing: its policy, head revision and live lock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    pub path: RepoPath,
    pub mode: Mode,
    pub revision: Option<RevisionId>,
    pub lock: Option<Lock>,
}

#[derive(Debug, Clone)]
pub struct ServiceConfig {
    /// Lock lifetime; checking out again renews it.
    pub lease: Duration,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self {
            lease: Duration::hours(8),
        }
    }
}

/// Server-side policy for one repository. Enforcement lives here and in the `MetadataStore` primitives.
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

    /// Take the lock on an exclusive path. `base` must equal the head so nobody edits a stale copy;
    /// the holder renews by checking out again.
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

    pub async fn release(&self, path: &RepoPath, user: &UserId) -> Result<()> {
        self.meta
            .release_lock(&self.repo, path, user, self.clock.now())
            .await
    }

    /// Record a revision of `content` (already in the object store). Exclusive paths need the caller's
    /// live lock; every path needs `base` == head.
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

    /// Paths with a revision or a live lock, ordered by path, starting after `after`.
    pub async fn files(&self, after: Option<&RepoPath>, limit: usize) -> Result<Vec<FileEntry>> {
        let now = self.clock.now();
        let mut entries: BTreeMap<RepoPath, FileEntry> = BTreeMap::new();
        for rev in self.meta.list_head_revisions(&self.repo).await? {
            let mode = self.mode_for(&rev.path);
            entries.insert(
                rev.path.clone(),
                FileEntry {
                    path: rev.path,
                    mode,
                    revision: Some(rev.id),
                    lock: None,
                },
            );
        }
        for lock in self.meta.list_locks(&self.repo, now).await? {
            let path = lock.path.clone();
            let mode = self.mode_for(&path);
            entries
                .entry(path.clone())
                .or_insert_with(|| FileEntry {
                    path,
                    mode,
                    revision: None,
                    lock: None,
                })
                .lock = Some(lock);
        }
        Ok(entries
            .into_values()
            .filter(|e| after.is_none_or(|a| &e.path > a))
            .take(limit)
            .collect())
    }

    /// A revision and its content; the head when `revision` is `None`.
    pub async fn read(
        &self,
        path: &RepoPath,
        revision: Option<RevisionId>,
    ) -> Result<(Revision, Vec<u8>)> {
        let found = match revision {
            Some(id) => self.meta.get_revision(&self.repo, path, id).await?,
            None => self.meta.head_revision(&self.repo, path).await?,
        };
        let rev = found.ok_or_else(|| PynError::RevisionNotFound {
            path: path.clone(),
            revision: revision.map_or("(head)".to_string(), |r| r.to_string()),
        })?;
        let bytes = self
            .objects
            .get(&rev.content)
            .await?
            .ok_or_else(|| PynError::ObjectMissing(rev.content.to_string()))?;
        Ok((rev, bytes))
    }

    pub async fn head(&self, path: &RepoPath) -> Result<Option<Revision>> {
        self.meta.head_revision(&self.repo, path).await
    }

    pub async fn history(&self, path: &RepoPath) -> Result<Vec<Revision>> {
        self.meta.history(&self.repo, path).await
    }
}
