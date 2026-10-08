//! In-memory stores for tests and the dev server; one `Mutex` makes each primitive atomic.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};

use crate::error::{PynError, Result};
use crate::object::ObjectStore;
use crate::store::MetadataStore;
use crate::types::{
    ContentHash, Lock, NewRevision, RepoId, RepoPath, Revision, RevisionId, UserId,
};

type Key = (RepoId, RepoPath);

#[derive(Default)]
struct State {
    locks: HashMap<Key, Lock>,
    revisions: HashMap<Key, Vec<Revision>>,
}

#[derive(Default)]
pub struct MemoryMetadataStore {
    state: Mutex<State>,
}

impl MemoryMetadataStore {
    pub fn new() -> Self {
        Self::default()
    }
}

fn live(lock: &Lock, now: DateTime<Utc>) -> bool {
    lock.expires_at > now
}

#[async_trait]
impl MetadataStore for MemoryMetadataStore {
    async fn acquire_lock(
        &self,
        repo: &RepoId,
        path: &RepoPath,
        owner: &UserId,
        now: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> Result<Lock> {
        let mut st = self.state.lock().unwrap();
        let key = (repo.clone(), path.clone());
        let acquired_at = match st.locks.get(&key) {
            Some(l) if live(l, now) && &l.owner != owner => {
                return Err(PynError::LockHeld {
                    path: path.clone(),
                    owner: l.owner.clone(),
                    expires_at: l.expires_at,
                });
            }
            // Renewal keeps the original acquisition time.
            Some(l) if live(l, now) => l.acquired_at,
            _ => now,
        };
        let lock = Lock {
            path: path.clone(),
            owner: owner.clone(),
            acquired_at,
            expires_at,
        };
        st.locks.insert(key, lock.clone());
        Ok(lock)
    }

    async fn release_lock(
        &self,
        repo: &RepoId,
        path: &RepoPath,
        owner: &UserId,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let mut st = self.state.lock().unwrap();
        let key = (repo.clone(), path.clone());
        match st.locks.get(&key) {
            Some(l) if live(l, now) && &l.owner == owner => {
                st.locks.remove(&key);
                Ok(())
            }
            _ => Err(PynError::NotLockHolder(path.clone())),
        }
    }

    async fn get_lock(
        &self,
        repo: &RepoId,
        path: &RepoPath,
        now: DateTime<Utc>,
    ) -> Result<Option<Lock>> {
        let st = self.state.lock().unwrap();
        Ok(st
            .locks
            .get(&(repo.clone(), path.clone()))
            .filter(|l| live(l, now))
            .cloned())
    }

    async fn list_locks(&self, repo: &RepoId, now: DateTime<Utc>) -> Result<Vec<Lock>> {
        let st = self.state.lock().unwrap();
        let mut out: Vec<Lock> = st
            .locks
            .iter()
            .filter(|((r, _), l)| r == repo && live(l, now))
            .map(|(_, l)| l.clone())
            .collect();
        out.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(out)
    }

    async fn head_revision(&self, repo: &RepoId, path: &RepoPath) -> Result<Option<Revision>> {
        let st = self.state.lock().unwrap();
        Ok(st
            .revisions
            .get(&(repo.clone(), path.clone()))
            .and_then(|v| v.last().cloned()))
    }

    async fn commit_revision(
        &self,
        repo: &RepoId,
        revision: NewRevision,
        expected_head: Option<RevisionId>,
        lock_holder: Option<&UserId>,
        now: DateTime<Utc>,
    ) -> Result<Revision> {
        let mut st = self.state.lock().unwrap();
        let key = (repo.clone(), revision.path.clone());

        if let Some(user) = lock_holder {
            match st.locks.get(&key) {
                Some(l) if live(l, now) && &l.owner == user => {}
                Some(l) if live(l, now) => {
                    return Err(PynError::LockHeld {
                        path: revision.path,
                        owner: l.owner.clone(),
                        expires_at: l.expires_at,
                    });
                }
                _ => return Err(PynError::LockRequired(revision.path)),
            }
        }

        let head = st.revisions.get(&key).and_then(|v| v.last()).map(|r| r.id);
        if head != expected_head {
            return Err(PynError::StaleBase {
                path: revision.path,
                expected: expected_head,
                actual: head,
            });
        }

        let created = Revision {
            id: RevisionId(head.map_or(1, |h| h.0 + 1)),
            path: revision.path,
            content: revision.content,
            author: revision.author,
            message: revision.message,
            created_at: revision.created_at,
        };
        st.revisions
            .entry(key.clone())
            .or_default()
            .push(created.clone());
        if lock_holder.is_some() {
            st.locks.remove(&key);
        }
        Ok(created)
    }

    async fn list_head_revisions(&self, repo: &RepoId) -> Result<Vec<Revision>> {
        let st = self.state.lock().unwrap();
        let mut heads: Vec<Revision> = st
            .revisions
            .iter()
            .filter(|((r, _), _)| r == repo)
            .filter_map(|(_, revs)| revs.last().cloned())
            .collect();
        heads.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(heads)
    }

    async fn history(&self, repo: &RepoId, path: &RepoPath) -> Result<Vec<Revision>> {
        let st = self.state.lock().unwrap();
        Ok(st
            .revisions
            .get(&(repo.clone(), path.clone()))
            .cloned()
            .unwrap_or_default())
    }
}

#[derive(Default)]
pub struct MemoryObjectStore {
    objects: Mutex<HashMap<ContentHash, Vec<u8>>>,
}

impl MemoryObjectStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl ObjectStore for MemoryObjectStore {
    async fn put(&self, bytes: Vec<u8>) -> Result<ContentHash> {
        let hash = ContentHash::new(hex::encode(Sha256::digest(&bytes)));
        self.objects.lock().unwrap().insert(hash.clone(), bytes);
        Ok(hash)
    }

    async fn get(&self, hash: &ContentHash) -> Result<Option<Vec<u8>>> {
        Ok(self.objects.lock().unwrap().get(hash).cloned())
    }

    async fn exists(&self, hash: &ContentHash) -> Result<bool> {
        Ok(self.objects.lock().unwrap().contains_key(hash))
    }
}
