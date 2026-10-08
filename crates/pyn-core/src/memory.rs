//! In-memory stores for tests and the dev server; one `Mutex` makes each primitive atomic.

use std::collections::{BTreeSet, HashMap};
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};

use crate::access::{Permission, Role, RoleDefinitions, TokenId, TokenRecord};
use crate::access_service::AccessStore;
use crate::audit::{AuditEvent, AuditQuery, AuditStore, NewAuditEvent};
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

    async fn force_release_lock(
        &self,
        repo: &RepoId,
        path: &RepoPath,
        now: DateTime<Utc>,
    ) -> Result<Option<Lock>> {
        let mut st = self.state.lock().unwrap();
        let key = (repo.clone(), path.clone());
        if st.locks.get(&key).is_some_and(|l| live(l, now)) {
            return Ok(st.locks.remove(&key));
        }
        Ok(None)
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
            restored_from: revision.restored_from,
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

    async fn get_revision(
        &self,
        repo: &RepoId,
        path: &RepoPath,
        id: RevisionId,
    ) -> Result<Option<Revision>> {
        let st = self.state.lock().unwrap();
        let revs = st.revisions.get(&(repo.clone(), path.clone()));
        Ok(revs.and_then(|v| v.iter().find(|r| r.id == id).cloned()))
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

#[derive(Default)]
struct AccessState {
    users: BTreeSet<UserId>,
    roles: HashMap<(RepoId, UserId), Role>,
    definitions: HashMap<RepoId, RoleDefinitions>,
    tokens: HashMap<TokenId, TokenRecord>,
}

#[derive(Default)]
pub struct MemoryAccessStore {
    state: Mutex<AccessState>,
}

impl MemoryAccessStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl AccessStore for MemoryAccessStore {
    async fn ensure_user(&self, user: &UserId, _now: DateTime<Utc>) -> Result<()> {
        self.state.lock().unwrap().users.insert(user.clone());
        Ok(())
    }

    async fn set_role(&self, repo: &RepoId, user: &UserId, role: Role) -> Result<()> {
        self.state
            .lock()
            .unwrap()
            .roles
            .insert((repo.clone(), user.clone()), role);
        Ok(())
    }

    async fn role_of(&self, repo: &RepoId, user: &UserId) -> Result<Option<Role>> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .roles
            .get(&(repo.clone(), user.clone()))
            .copied())
    }

    async fn members(&self, repo: &RepoId) -> Result<Vec<(UserId, Role)>> {
        let st = self.state.lock().unwrap();
        let mut out: Vec<_> = st
            .roles
            .iter()
            .filter(|((r, _), _)| r == repo)
            .map(|((_, u), role)| (u.clone(), *role))
            .collect();
        out.sort();
        Ok(out)
    }

    async fn role_definitions(&self, repo: &RepoId) -> Result<RoleDefinitions> {
        let st = self.state.lock().unwrap();
        Ok(st
            .definitions
            .get(repo)
            .cloned()
            .unwrap_or_else(RoleDefinitions::defaults))
    }

    async fn set_role_permissions(
        &self,
        repo: &RepoId,
        role: Role,
        permissions: BTreeSet<Permission>,
    ) -> Result<()> {
        let mut st = self.state.lock().unwrap();
        st.definitions
            .entry(repo.clone())
            .or_insert_with(RoleDefinitions::defaults)
            .set(role, permissions);
        Ok(())
    }

    async fn create_token(&self, token: TokenRecord) -> Result<()> {
        self.state
            .lock()
            .unwrap()
            .tokens
            .insert(token.id.clone(), token);
        Ok(())
    }

    async fn get_token(&self, id: &TokenId) -> Result<Option<TokenRecord>> {
        Ok(self.state.lock().unwrap().tokens.get(id).cloned())
    }

    async fn list_tokens(&self, user: &UserId) -> Result<Vec<TokenRecord>> {
        let st = self.state.lock().unwrap();
        let mut out: Vec<_> = st
            .tokens
            .values()
            .filter(|t| &t.user == user)
            .cloned()
            .collect();
        out.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(out)
    }

    async fn revoke_token(&self, id: &TokenId, now: DateTime<Utc>) -> Result<bool> {
        let mut st = self.state.lock().unwrap();
        let Some(token) = st.tokens.get_mut(id) else {
            return Ok(false);
        };
        token.revoked_at.get_or_insert(now);
        Ok(true)
    }

    async fn touch_token(&self, id: &TokenId, now: DateTime<Utc>) -> Result<()> {
        if let Some(token) = self.state.lock().unwrap().tokens.get_mut(id) {
            token.last_used_at = Some(now);
        }
        Ok(())
    }
}

#[derive(Default)]
pub struct MemoryAuditStore {
    events: Mutex<Vec<(RepoId, AuditEvent)>>,
}

impl MemoryAuditStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl AuditStore for MemoryAuditStore {
    async fn record(&self, repo: &RepoId, event: NewAuditEvent) -> Result<()> {
        let mut events = self.events.lock().unwrap();
        let id = events.len() as i64 + 1;
        events.push((
            repo.clone(),
            AuditEvent {
                id,
                at: event.at,
                actor: event.actor,
                action: event.action,
                path: event.path,
                detail: event.detail,
            },
        ));
        Ok(())
    }

    async fn list(&self, repo: &RepoId, query: &AuditQuery) -> Result<Vec<AuditEvent>> {
        let events = self.events.lock().unwrap();
        Ok(events
            .iter()
            .rev()
            .filter(|(r, e)| {
                r == repo
                    && query.before.is_none_or(|b| e.id < b)
                    && query
                        .path
                        .as_ref()
                        .is_none_or(|p| e.path.as_ref() == Some(p))
                    && query.actor.as_ref().is_none_or(|a| &e.actor == a)
                    && query.action.is_none_or(|a| e.action == a)
            })
            .map(|(_, e)| e.clone())
            .take(query.limit)
            .collect())
    }
}
