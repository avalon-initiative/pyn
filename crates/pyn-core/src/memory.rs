//! In-memory stores for tests and the dev server; one `Mutex` makes each primitive atomic.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use sha2::{Digest, Sha256};

use crate::access::{
    AccountKind, AccountRecord, AccountStatus, InviteId, InviteRecord, NewAccount, OrgRole,
    Permission, Role, RoleDefinitions, ServerSettings, ServiceCredentialRecord, SessionRecord,
    SignupStage, SshKeyRecord, TeamRecord, TokenId, TokenRecord, VerificationRecord,
};
use crate::access_service::{AccessStore, AdminRevoke, OrgDeleteMark, OrgMemberChange};
use crate::audit::{AuditEvent, AuditQuery, AuditScope, AuditStore, NewAuditEvent};
use crate::error::{PynError, Result};
use crate::history::HistoryCursor;
use crate::object::ObjectStore;
use crate::ratelimit::{RateLimitStore, RateState};
use crate::repo::{RepoRecord, RepoUpdate};
use crate::repo_policy::{
    CreationEffect, CreationRule, CreationScope, CreationSubject, MemberCreation, RepoPolicy,
};
use crate::rules::PathFilter;
use crate::store::MetadataStore;
use crate::types::{
    ContentHash, Lock, NewRevision, RepoId, RepoPath, Revision, RevisionId, UserId,
};

type Key = (RepoId, RepoPath);

#[derive(Default)]
struct State {
    locks: HashMap<Key, Lock>,
    revisions: HashMap<Key, Vec<Revision>>,
    repos: HashMap<RepoId, RepoRecord>,
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

fn history_key(r: &Revision) -> (DateTime<Utc>, &str, RevisionId) {
    (r.created_at, r.path.as_str(), r.id)
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
        max_locks: u32,
    ) -> Result<Lock> {
        let mut st = self.state.lock().unwrap();
        let key = (repo.clone(), path.clone());
        if !st.locks.get(&key).is_some_and(|l| live(l, now)) {
            let held = st
                .locks
                .iter()
                .filter(|((r, _), l)| r == repo && &l.owner == owner && live(l, now))
                .count();
            if held >= max_locks as usize {
                return Err(PynError::LockLimitReached { limit: max_locks });
            }
        }
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

    async fn list_locks_of(
        &self,
        owner: &UserId,
        now: DateTime<Utc>,
    ) -> Result<Vec<(RepoId, Lock)>> {
        let st = self.state.lock().unwrap();
        let mut out: Vec<(RepoId, Lock)> = st
            .locks
            .iter()
            .filter(|(_, l)| &l.owner == owner && live(l, now))
            .map(|((r, _), l)| (r.clone(), l.clone()))
            .collect();
        out.sort_by(|a, b| (&a.0, &a.1.path).cmp(&(&b.0, &b.1.path)));
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
            mode: Some(revision.mode),
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

    async fn repo_history(
        &self,
        repo: &RepoId,
        filter: Option<&PathFilter>,
        before: Option<&HistoryCursor>,
        limit: usize,
    ) -> Result<Vec<Revision>> {
        let st = self.state.lock().unwrap();
        let mut revs: Vec<&Revision> = st
            .revisions
            .iter()
            .filter(|((r, _), _)| r == repo)
            .flat_map(|(_, revs)| revs)
            .filter(|r| filter.is_none_or(|f| f.is_match(&r.path)))
            .filter(|r| before.is_none_or(|b| history_key(r) < b.sort_key()))
            .collect();
        revs.sort_by_key(|r| std::cmp::Reverse(history_key(r)));
        Ok(revs.into_iter().take(limit).cloned().collect())
    }

    async fn create_repo(&self, repo: RepoRecord) -> Result<RepoRecord> {
        let mut st = self.state.lock().unwrap();
        if st
            .repos
            .values()
            .any(|r| r.owner == repo.owner && r.name == repo.name)
        {
            return Err(PynError::RepoExists(repo.address()));
        }
        st.repos.insert(repo.id.clone(), repo.clone());
        Ok(repo)
    }

    async fn find_repo(&self, owner: &UserId, name: &str) -> Result<Option<RepoRecord>> {
        let st = self.state.lock().unwrap();
        Ok(st
            .repos
            .values()
            .find(|r| &r.owner == owner && r.name == name)
            .cloned())
    }

    async fn get_repo(&self, id: &RepoId) -> Result<Option<RepoRecord>> {
        Ok(self.state.lock().unwrap().repos.get(id).cloned())
    }

    async fn list_repos(&self, owner: Option<&UserId>) -> Result<Vec<RepoRecord>> {
        let st = self.state.lock().unwrap();
        let mut out: Vec<_> = st
            .repos
            .values()
            .filter(|r| owner.is_none_or(|o| &r.owner == o))
            .cloned()
            .collect();
        out.sort_by(|a, b| (&a.owner, &a.name).cmp(&(&b.owner, &b.name)));
        Ok(out)
    }

    async fn update_repo(&self, id: &RepoId, update: RepoUpdate) -> Result<RepoRecord> {
        let mut st = self.state.lock().unwrap();
        let mut repo = st
            .repos
            .get(id)
            .cloned()
            .ok_or_else(|| PynError::RepoNotFound(id.to_string()))?;
        if let Some(name) = update.name {
            repo.name = name;
        }
        if let Some(visibility) = update.visibility {
            repo.visibility = visibility;
        }
        if let Some(settings) = update.settings {
            repo.settings = settings;
        }
        if st
            .repos
            .values()
            .any(|r| &r.id != id && r.owner == repo.owner && r.name == repo.name)
        {
            return Err(PynError::RepoExists(repo.address()));
        }
        st.repos.insert(id.clone(), repo.clone());
        Ok(repo)
    }

    async fn delete_repo(&self, id: &RepoId) -> Result<bool> {
        let mut st = self.state.lock().unwrap();
        st.locks.retain(|(r, _), _| r != id);
        st.revisions.retain(|(r, _), _| r != id);
        Ok(st.repos.remove(id).is_some())
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
    accounts: BTreeMap<UserId, AccountRecord>,
    verifications: HashMap<String, VerificationRecord>,
    passwords: HashMap<UserId, String>,
    invites: HashMap<InviteId, InviteRecord>,
    ssh_keys: Vec<SshKeyRecord>,
    roles: HashMap<(RepoId, UserId), Role>,
    org_roles: HashMap<(UserId, UserId), OrgRole>,
    teams: BTreeMap<(UserId, String), TeamRecord>,
    team_members: BTreeSet<(UserId, String, UserId)>,
    team_roles: BTreeMap<(RepoId, UserId, String), Role>,
    member_creation: HashMap<UserId, MemberCreation>,
    creation_rules: BTreeMap<(UserId, CreationSubject, CreationEffect), CreationScope>,
    definitions: HashMap<RepoId, RoleDefinitions>,
    tokens: HashMap<TokenId, TokenRecord>,
    service_credentials: HashMap<TokenId, ServiceCredentialRecord>,
    sessions: HashMap<String, SessionRecord>,
    settings: Option<ServerSettings>,
}

impl AccessState {
    fn owner_count(&self, org: &UserId) -> usize {
        self.org_roles
            .iter()
            .filter(|((o, _), r)| o == org && **r == OrgRole::Owner)
            .count()
    }
}

fn plain_account(user: &UserId, now: DateTime<Utc>) -> AccountRecord {
    AccountRecord {
        user: user.clone(),
        kind: AccountKind::User,
        email: None,
        email_verified_at: None,
        signup: SignupStage::Complete,
        disabled_at: None,
        disabled_reason: None,
        is_admin: false,
        created_at: now,
        deleting_since: None,
    }
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
    async fn ensure_user(&self, user: &UserId, now: DateTime<Utc>) -> Result<()> {
        self.state
            .lock()
            .unwrap()
            .accounts
            .entry(user.clone())
            .or_insert_with(|| plain_account(user, now));
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

    async fn repos_of(&self, user: &UserId) -> Result<Vec<RepoId>> {
        let st = self.state.lock().unwrap();
        let mut out: Vec<_> = st
            .roles
            .keys()
            .filter(|(_, u)| u == user)
            .map(|(r, _)| r.clone())
            .collect();
        out.sort();
        Ok(out)
    }

    async fn delete_repo_access(&self, repo: &RepoId) -> Result<()> {
        let mut st = self.state.lock().unwrap();
        st.roles.retain(|(r, _), _| r != repo);
        st.team_roles.retain(|(r, _, _), _| r != repo);
        st.definitions.remove(repo);
        st.invites.retain(|_, i| &i.repo != repo);
        Ok(())
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

    async fn create_user(&self, user: &UserId, now: DateTime<Utc>) -> Result<bool> {
        let mut st = self.state.lock().unwrap();
        if st.accounts.contains_key(user) {
            return Ok(false);
        }
        st.accounts.insert(user.clone(), plain_account(user, now));
        Ok(true)
    }

    async fn user_exists(&self, user: &UserId) -> Result<bool> {
        Ok(self.state.lock().unwrap().accounts.contains_key(user))
    }

    async fn create_org(&self, org: &UserId, owner: &UserId, now: DateTime<Utc>) -> Result<bool> {
        let mut st = self.state.lock().unwrap();
        if st.accounts.contains_key(org) {
            return Ok(false);
        }
        st.accounts
            .entry(owner.clone())
            .or_insert_with(|| plain_account(owner, now));
        let record = AccountRecord {
            kind: AccountKind::Org,
            ..plain_account(org, now)
        };
        st.accounts.insert(org.clone(), record);
        st.org_roles
            .insert((org.clone(), owner.clone()), OrgRole::Owner);
        Ok(true)
    }

    async fn org_role(&self, org: &UserId, user: &UserId) -> Result<Option<OrgRole>> {
        let st = self.state.lock().unwrap();
        Ok(st.org_roles.get(&(org.clone(), user.clone())).copied())
    }

    async fn orgs_of(&self, user: &UserId) -> Result<Vec<(UserId, OrgRole)>> {
        let st = self.state.lock().unwrap();
        let mut out: Vec<_> = st
            .org_roles
            .iter()
            .filter(|((_, u), _)| u == user)
            .map(|((o, _), r)| (o.clone(), *r))
            .collect();
        out.sort();
        Ok(out)
    }

    async fn delete_org(&self, org: &UserId) -> Result<bool> {
        let mut st = self.state.lock().unwrap();
        if st
            .accounts
            .get(org)
            .is_none_or(|a| a.kind != AccountKind::Org)
        {
            return Ok(false);
        }
        st.accounts.remove(org);
        st.org_roles.retain(|(o, _), _| o != org);
        st.teams.retain(|(o, _), _| o != org);
        st.team_members.retain(|(o, _, _)| o != org);
        st.team_roles.retain(|(_, o, _), _| o != org);
        st.member_creation.remove(org);
        st.creation_rules.retain(|(o, _, _), _| o != org);
        Ok(true)
    }

    async fn mark_org_deleting(
        &self,
        org: &UserId,
        now: DateTime<Utc>,
        stale_before: DateTime<Utc>,
    ) -> Result<OrgDeleteMark> {
        let mut st = self.state.lock().unwrap();
        let Some(account) = st
            .accounts
            .get_mut(org)
            .filter(|a| a.kind == AccountKind::Org)
        else {
            return Ok(OrgDeleteMark::NotFound);
        };
        if account.deleting_since.is_some_and(|s| s > stale_before) {
            return Ok(OrgDeleteMark::Held);
        }
        account.deleting_since = Some(now);
        Ok(OrgDeleteMark::Set)
    }

    async fn clear_org_deleting(&self, org: &UserId) -> Result<()> {
        let mut st = self.state.lock().unwrap();
        if let Some(account) = st.accounts.get_mut(org) {
            account.deleting_since = None;
        }
        Ok(())
    }

    async fn org_members(&self, org: &UserId) -> Result<Vec<(UserId, OrgRole)>> {
        let st = self.state.lock().unwrap();
        let mut out: Vec<_> = st
            .org_roles
            .iter()
            .filter(|((o, _), _)| o == org)
            .map(|((_, u), r)| (u.clone(), *r))
            .collect();
        out.sort();
        Ok(out)
    }

    async fn add_org_member(&self, org: &UserId, user: &UserId, role: OrgRole) -> Result<bool> {
        let mut st = self.state.lock().unwrap();
        let key = (org.clone(), user.clone());
        if st.org_roles.contains_key(&key) {
            return Ok(false);
        }
        st.org_roles.insert(key, role);
        Ok(true)
    }

    async fn set_org_role(
        &self,
        org: &UserId,
        user: &UserId,
        role: OrgRole,
    ) -> Result<OrgMemberChange> {
        let mut st = self.state.lock().unwrap();
        let key = (org.clone(), user.clone());
        let Some(&old) = st.org_roles.get(&key) else {
            return Ok(OrgMemberChange::NotMember);
        };
        if old == OrgRole::Owner && role != OrgRole::Owner && st.owner_count(org) == 1 {
            return Ok(OrgMemberChange::LastOwner);
        }
        st.org_roles.insert(key, role);
        Ok(OrgMemberChange::Done(old))
    }

    async fn remove_org_member(
        &self,
        org: &UserId,
        user: &UserId,
        repos: &[RepoId],
    ) -> Result<OrgMemberChange> {
        let mut st = self.state.lock().unwrap();
        let key = (org.clone(), user.clone());
        let Some(&old) = st.org_roles.get(&key) else {
            return Ok(OrgMemberChange::NotMember);
        };
        if old == OrgRole::Owner && st.owner_count(org) == 1 {
            return Ok(OrgMemberChange::LastOwner);
        }
        st.org_roles.remove(&key);
        st.team_members.retain(|(o, _, u)| !(o == org && u == user));
        st.creation_rules
            .retain(|(o, s, _), _| !(o == org && *s == CreationSubject::User(user.clone())));
        for repo in repos {
            st.roles.remove(&(repo.clone(), user.clone()));
        }
        Ok(OrgMemberChange::Done(old))
    }

    async fn create_team(&self, team: TeamRecord) -> Result<bool> {
        let mut st = self.state.lock().unwrap();
        let key = (team.org.clone(), team.slug.clone());
        if st.teams.contains_key(&key) {
            return Ok(false);
        }
        st.teams.insert(key, team);
        Ok(true)
    }

    async fn team(&self, org: &UserId, slug: &str) -> Result<Option<TeamRecord>> {
        let st = self.state.lock().unwrap();
        Ok(st.teams.get(&(org.clone(), slug.to_string())).cloned())
    }

    async fn teams(&self, org: &UserId) -> Result<Vec<TeamRecord>> {
        let st = self.state.lock().unwrap();
        Ok(st
            .teams
            .values()
            .filter(|t| &t.org == org)
            .cloned()
            .collect())
    }

    async fn update_team(&self, team: &TeamRecord) -> Result<bool> {
        let mut st = self.state.lock().unwrap();
        match st.teams.get_mut(&(team.org.clone(), team.slug.clone())) {
            Some(existing) => {
                existing.name = team.name.clone();
                existing.description = team.description.clone();
                Ok(true)
            }
            None => Ok(false),
        }
    }

    async fn delete_team(&self, org: &UserId, slug: &str) -> Result<bool> {
        let mut st = self.state.lock().unwrap();
        if st.teams.remove(&(org.clone(), slug.to_string())).is_none() {
            return Ok(false);
        }
        st.team_members.retain(|(o, t, _)| !(o == org && t == slug));
        st.team_roles
            .retain(|(_, o, t), _| !(o == org && t == slug));
        st.creation_rules
            .retain(|(o, s, _), _| !(o == org && *s == CreationSubject::Team(slug.to_string())));
        Ok(true)
    }

    async fn team_members(&self, org: &UserId, slug: &str) -> Result<Vec<UserId>> {
        let st = self.state.lock().unwrap();
        Ok(st
            .team_members
            .iter()
            .filter(|(o, t, _)| o == org && t == slug)
            .map(|(_, _, u)| u.clone())
            .collect())
    }

    async fn add_team_member(&self, org: &UserId, slug: &str, user: &UserId) -> Result<bool> {
        let mut st = self.state.lock().unwrap();
        if !st.teams.contains_key(&(org.clone(), slug.to_string()))
            || !st.org_roles.contains_key(&(org.clone(), user.clone()))
        {
            return Err(PynError::Storage(format!(
                "{user} cannot join team {slug} of {org}"
            )));
        }
        Ok(st
            .team_members
            .insert((org.clone(), slug.to_string(), user.clone())))
    }

    async fn remove_team_member(&self, org: &UserId, slug: &str, user: &UserId) -> Result<bool> {
        let mut st = self.state.lock().unwrap();
        Ok(st
            .team_members
            .remove(&(org.clone(), slug.to_string(), user.clone())))
    }

    async fn teams_of(&self, org: &UserId, user: &UserId) -> Result<Vec<String>> {
        let st = self.state.lock().unwrap();
        Ok(st
            .team_members
            .iter()
            .filter(|(o, _, u)| o == org && u == user)
            .map(|(_, t, _)| t.clone())
            .collect())
    }

    async fn set_team_role(
        &self,
        repo: &RepoId,
        org: &UserId,
        slug: &str,
        role: Role,
    ) -> Result<Option<Role>> {
        let mut st = self.state.lock().unwrap();
        if !st.teams.contains_key(&(org.clone(), slug.to_string())) {
            return Err(PynError::Storage(format!("no team {slug} in {org}")));
        }
        Ok(st
            .team_roles
            .insert((repo.clone(), org.clone(), slug.to_string()), role))
    }

    async fn remove_team_role(
        &self,
        repo: &RepoId,
        org: &UserId,
        slug: &str,
    ) -> Result<Option<Role>> {
        let mut st = self.state.lock().unwrap();
        Ok(st
            .team_roles
            .remove(&(repo.clone(), org.clone(), slug.to_string())))
    }

    async fn team_grants(&self, repo: &RepoId) -> Result<Vec<(String, Role)>> {
        let st = self.state.lock().unwrap();
        let mut out: Vec<_> = st
            .team_roles
            .iter()
            .filter(|((r, _, _), _)| r == repo)
            .map(|((_, _, t), role)| (t.clone(), *role))
            .collect();
        out.sort();
        Ok(out)
    }

    async fn team_repos(&self, org: &UserId, slug: &str) -> Result<Vec<(RepoId, Role)>> {
        let st = self.state.lock().unwrap();
        Ok(st
            .team_roles
            .iter()
            .filter(|((_, o, t), _)| o == org && t == slug)
            .map(|((r, _, _), role)| (r.clone(), *role))
            .collect())
    }

    async fn team_role_of(&self, repo: &RepoId, user: &UserId) -> Result<Option<Role>> {
        let st = self.state.lock().unwrap();
        Ok(st
            .team_roles
            .iter()
            .filter(|((r, o, t), _)| {
                r == repo
                    && st
                        .team_members
                        .contains(&(o.clone(), t.clone(), user.clone()))
            })
            .map(|(_, role)| *role)
            .max())
    }

    async fn team_repos_of(&self, user: &UserId) -> Result<Vec<RepoId>> {
        let st = self.state.lock().unwrap();
        let mut out: Vec<_> = st
            .team_roles
            .keys()
            .filter(|(_, o, t)| {
                st.team_members
                    .contains(&(o.clone(), t.clone(), user.clone()))
            })
            .map(|(r, _, _)| r.clone())
            .collect();
        out.sort();
        out.dedup();
        Ok(out)
    }

    async fn repo_policy(&self, org: &UserId) -> Result<RepoPolicy> {
        let st = self.state.lock().unwrap();
        let mut rules: Vec<_> = st
            .creation_rules
            .iter()
            .filter(|((o, _, _), _)| o == org)
            .map(|((_, subject, effect), scope)| CreationRule {
                subject: subject.clone(),
                effect: *effect,
                scope: *scope,
            })
            .collect();
        rules.sort_by_key(|r| (r.subject.kind().as_str(), r.subject.name(), r.effect));
        Ok(RepoPolicy {
            base: st
                .member_creation
                .get(org)
                .copied()
                .unwrap_or(MemberCreation::None),
            rules,
        })
    }

    async fn set_member_creation(
        &self,
        org: &UserId,
        base: MemberCreation,
    ) -> Result<MemberCreation> {
        let mut st = self.state.lock().unwrap();
        Ok(st
            .member_creation
            .insert(org.clone(), base)
            .unwrap_or(MemberCreation::None))
    }

    async fn set_creation_rule(&self, org: &UserId, rule: &CreationRule) -> Result<bool> {
        let mut st = self.state.lock().unwrap();
        let present = match &rule.subject {
            CreationSubject::Team(slug) => st.teams.contains_key(&(org.clone(), slug.clone())),
            CreationSubject::User(user) => st.org_roles.contains_key(&(org.clone(), user.clone())),
            CreationSubject::Role(_) => true,
        };
        if present {
            st.creation_rules
                .insert((org.clone(), rule.subject.clone(), rule.effect), rule.scope);
        }
        Ok(present)
    }

    async fn remove_creation_rule(
        &self,
        org: &UserId,
        subject: &CreationSubject,
        effect: CreationEffect,
    ) -> Result<Option<CreationScope>> {
        let mut st = self.state.lock().unwrap();
        Ok(st
            .creation_rules
            .remove(&(org.clone(), subject.clone(), effect)))
    }

    async fn create_account(&self, new: NewAccount) -> Result<bool> {
        let mut st = self.state.lock().unwrap();
        if st.accounts.contains_key(&new.user) {
            return Ok(false);
        }
        st.passwords.insert(new.user.clone(), new.password_hash);
        let account = AccountRecord {
            email: new.email,
            signup: new.signup,
            ..plain_account(&new.user, new.created_at)
        };
        st.accounts.insert(new.user, account);
        Ok(true)
    }

    async fn account(&self, user: &UserId) -> Result<Option<AccountRecord>> {
        Ok(self.state.lock().unwrap().accounts.get(user).cloned())
    }

    async fn verified_email_owner(&self, email: &str) -> Result<Option<UserId>> {
        let st = self.state.lock().unwrap();
        Ok(st
            .accounts
            .values()
            .find(|a| a.email_verified_at.is_some() && a.email.as_deref() == Some(email))
            .map(|a| a.user.clone()))
    }

    async fn pending_by_email(&self, email: &str) -> Result<Vec<AccountRecord>> {
        let st = self.state.lock().unwrap();
        Ok(st
            .accounts
            .values()
            .filter(|a| {
                a.signup == SignupStage::PendingVerification && a.email.as_deref() == Some(email)
            })
            .cloned()
            .collect())
    }

    async fn complete_verification(
        &self,
        user: &UserId,
        signup: SignupStage,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        let mut st = self.state.lock().unwrap();
        let Some(email) = st.accounts.get(user).and_then(|a| a.email.clone()) else {
            return Ok(false);
        };
        let taken = st.accounts.values().any(|a| {
            &a.user != user
                && a.email_verified_at.is_some()
                && a.email.as_deref() == Some(email.as_str())
        });
        if taken {
            return Ok(false);
        }
        let account = st.accounts.get_mut(user).expect("checked above");
        account.email_verified_at = Some(now);
        account.signup = signup;
        Ok(true)
    }

    async fn set_signup(&self, user: &UserId, signup: SignupStage) -> Result<bool> {
        let mut st = self.state.lock().unwrap();
        let Some(account) = st.accounts.get_mut(user) else {
            return Ok(false);
        };
        account.signup = signup;
        Ok(true)
    }

    async fn set_disabled(
        &self,
        user: &UserId,
        disabled: Option<(DateTime<Utc>, Option<String>)>,
    ) -> Result<bool> {
        let mut st = self.state.lock().unwrap();
        let Some(account) = st.accounts.get_mut(user) else {
            return Ok(false);
        };
        (account.disabled_at, account.disabled_reason) = match disabled {
            Some((at, reason)) => (Some(at), reason),
            None => (None, None),
        };
        Ok(true)
    }

    async fn set_admin(&self, user: &UserId, admin: bool) -> Result<()> {
        if let Some(account) = self.state.lock().unwrap().accounts.get_mut(user) {
            account.is_admin = admin;
        }
        Ok(())
    }

    async fn revoke_admin(&self, user: &UserId) -> Result<AdminRevoke> {
        let mut st = self.state.lock().unwrap();
        if !st.accounts.get(user).is_some_and(|a| a.is_admin) {
            return Ok(AdminRevoke::NotAdmin);
        }
        let others = st.accounts.values().any(|a| {
            &a.user != user && a.kind == AccountKind::User && a.is_admin && a.disabled_at.is_none()
        });
        if !others {
            return Ok(AdminRevoke::LastAdmin);
        }
        st.accounts.get_mut(user).unwrap().is_admin = false;
        Ok(AdminRevoke::Revoked)
    }

    async fn list_accounts(
        &self,
        status: Option<AccountStatus>,
        limit: usize,
    ) -> Result<Vec<AccountRecord>> {
        let st = self.state.lock().unwrap();
        let mut out: Vec<_> = st
            .accounts
            .values()
            .filter(|a| a.kind == AccountKind::User)
            .filter(|a| status.is_none_or(|s| a.status() == s))
            .cloned()
            .collect();
        out.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.user.cmp(&b.user))
        });
        out.truncate(limit);
        Ok(out)
    }

    async fn delete_stale_pending(
        &self,
        now: DateTime<Utc>,
        created_before: DateTime<Utc>,
    ) -> Result<()> {
        let mut st = self.state.lock().unwrap();
        st.verifications.retain(|_, v| v.expires_at > now);
        let live: BTreeSet<UserId> = st.verifications.values().map(|v| v.user.clone()).collect();
        st.accounts.retain(|u, a| {
            a.signup != SignupStage::PendingVerification
                || a.created_at >= created_before
                || a.disabled_at.is_some()
                || live.contains(u)
        });
        let AccessState {
            accounts,
            passwords,
            ..
        } = &mut *st;
        passwords.retain(|u, _| accounts.contains_key(u));
        Ok(())
    }

    async fn put_verification(&self, record: VerificationRecord) -> Result<()> {
        let mut st = self.state.lock().unwrap();
        st.verifications.retain(|_, v| v.user != record.user);
        st.verifications.insert(record.token_hash.clone(), record);
        Ok(())
    }

    async fn take_verification(&self, token_hash: &str) -> Result<Option<VerificationRecord>> {
        Ok(self.state.lock().unwrap().verifications.remove(token_hash))
    }

    async fn delete_sessions_of(&self, user: &UserId) -> Result<()> {
        self.state
            .lock()
            .unwrap()
            .sessions
            .retain(|_, s| &s.user != user);
        Ok(())
    }

    async fn server_settings(&self) -> Result<Option<ServerSettings>> {
        Ok(self.state.lock().unwrap().settings.clone())
    }

    async fn complete_setup(&self, admin: NewAccount, settings: ServerSettings) -> Result<bool> {
        let mut st = self.state.lock().unwrap();
        if st.settings.is_some() {
            return Ok(false);
        }
        if st.accounts.contains_key(&admin.user) {
            return Err(PynError::UserExists(admin.user));
        }
        st.passwords.insert(admin.user.clone(), admin.password_hash);
        let account = AccountRecord {
            email: admin.email,
            signup: admin.signup,
            is_admin: true,
            ..plain_account(&admin.user, admin.created_at)
        };
        st.accounts.insert(admin.user, account);
        st.settings = Some(settings);
        Ok(true)
    }

    async fn set_password_hash(&self, user: &UserId, hash: &str) -> Result<()> {
        self.state
            .lock()
            .unwrap()
            .passwords
            .insert(user.clone(), hash.to_string());
        Ok(())
    }

    async fn password_hash(&self, user: &UserId) -> Result<Option<String>> {
        Ok(self.state.lock().unwrap().passwords.get(user).cloned())
    }

    async fn create_invite(&self, invite: InviteRecord) -> Result<()> {
        self.state
            .lock()
            .unwrap()
            .invites
            .insert(invite.id.clone(), invite);
        Ok(())
    }

    async fn get_invite(&self, id: &InviteId) -> Result<Option<InviteRecord>> {
        Ok(self.state.lock().unwrap().invites.get(id).cloned())
    }

    async fn list_invites(&self, repo: &RepoId) -> Result<Vec<InviteRecord>> {
        let st = self.state.lock().unwrap();
        let mut out: Vec<_> = st
            .invites
            .values()
            .filter(|i| &i.repo == repo)
            .cloned()
            .collect();
        out.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(out)
    }

    async fn revoke_invite(&self, id: &InviteId, now: DateTime<Utc>) -> Result<bool> {
        let mut st = self.state.lock().unwrap();
        let Some(invite) = st.invites.get_mut(id) else {
            return Ok(false);
        };
        invite.revoked_at.get_or_insert(now);
        Ok(true)
    }

    async fn use_invite(&self, id: &InviteId, user: &UserId, now: DateTime<Utc>) -> Result<bool> {
        let mut st = self.state.lock().unwrap();
        let Some(invite) = st.invites.get_mut(id) else {
            return Ok(false);
        };
        if invite.used_at.is_some() || invite.revoked_at.is_some() || invite.expires_at <= now {
            return Ok(false);
        }
        invite.used_at = Some(now);
        invite.used_by = Some(user.clone());
        Ok(true)
    }

    async fn add_ssh_key(&self, key: SshKeyRecord) -> Result<bool> {
        let mut st = self.state.lock().unwrap();
        if st.ssh_keys.iter().any(|k| k.fingerprint == key.fingerprint) {
            return Ok(false);
        }
        st.ssh_keys.push(key);
        Ok(true)
    }

    async fn create_session(&self, session: SessionRecord) -> Result<()> {
        self.state
            .lock()
            .unwrap()
            .sessions
            .insert(session.id_hash.clone(), session);
        Ok(())
    }

    async fn get_session(&self, id_hash: &str) -> Result<Option<SessionRecord>> {
        Ok(self.state.lock().unwrap().sessions.get(id_hash).cloned())
    }

    async fn delete_session(&self, id_hash: &str) -> Result<bool> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .sessions
            .remove(id_hash)
            .is_some())
    }

    async fn delete_expired_sessions(&self, now: DateTime<Utc>) -> Result<()> {
        self.state
            .lock()
            .unwrap()
            .sessions
            .retain(|_, s| s.expires_at > now);
        Ok(())
    }

    async fn list_ssh_keys(&self, user: &UserId) -> Result<Vec<SshKeyRecord>> {
        let st = self.state.lock().unwrap();
        let mut out: Vec<_> = st
            .ssh_keys
            .iter()
            .filter(|k| &k.user == user)
            .cloned()
            .collect();
        out.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(out)
    }

    async fn delete_ssh_key(&self, user: &UserId, id: &str) -> Result<bool> {
        let mut st = self.state.lock().unwrap();
        let before = st.ssh_keys.len();
        st.ssh_keys.retain(|k| !(&k.user == user && k.id == id));
        Ok(st.ssh_keys.len() < before)
    }

    async fn find_ssh_key(&self, fingerprint: &str) -> Result<Option<SshKeyRecord>> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .ssh_keys
            .iter()
            .find(|k| k.fingerprint == fingerprint)
            .cloned())
    }

    async fn touch_ssh_key(&self, fingerprint: &str, now: DateTime<Utc>) -> Result<()> {
        if let Some(k) = self
            .state
            .lock()
            .unwrap()
            .ssh_keys
            .iter_mut()
            .find(|k| k.fingerprint == fingerprint)
        {
            k.last_used_at = Some(now);
        }
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

    async fn create_service_credential(&self, record: ServiceCredentialRecord) -> Result<bool> {
        let mut st = self.state.lock().unwrap();
        if st
            .service_credentials
            .values()
            .any(|c| c.name == record.name)
        {
            return Ok(false);
        }
        st.service_credentials.insert(record.id.clone(), record);
        Ok(true)
    }

    async fn get_service_credential(
        &self,
        id: &TokenId,
    ) -> Result<Option<ServiceCredentialRecord>> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .service_credentials
            .get(id)
            .cloned())
    }

    async fn list_service_credentials(&self) -> Result<Vec<ServiceCredentialRecord>> {
        let mut out: Vec<_> = self
            .state
            .lock()
            .unwrap()
            .service_credentials
            .values()
            .cloned()
            .collect();
        out.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(out)
    }

    async fn revoke_service_credential(&self, name: &str, now: DateTime<Utc>) -> Result<bool> {
        let mut st = self.state.lock().unwrap();
        let Some(c) = st.service_credentials.values_mut().find(|c| c.name == name) else {
            return Ok(false);
        };
        c.revoked_at.get_or_insert(now);
        Ok(true)
    }

    async fn touch_service_credential(&self, id: &TokenId, now: DateTime<Utc>) -> Result<()> {
        if let Some(c) = self.state.lock().unwrap().service_credentials.get_mut(id) {
            c.last_used_at = Some(now);
        }
        Ok(())
    }
}

#[derive(Default)]
pub struct MemoryAuditStore {
    events: Mutex<Vec<(String, AuditEvent)>>,
}

impl MemoryAuditStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl AuditStore for MemoryAuditStore {
    async fn record(&self, scope: &AuditScope, event: NewAuditEvent) -> Result<()> {
        let mut events = self.events.lock().unwrap();
        let id = events.len() as i64 + 1;
        events.push((
            scope.key(),
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

    async fn list(&self, scope: &AuditScope, query: &AuditQuery) -> Result<Vec<AuditEvent>> {
        let key = scope.key();
        let events = self.events.lock().unwrap();
        Ok(events
            .iter()
            .rev()
            .filter(|(r, e)| {
                *r == key
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

#[derive(Default)]
pub struct MemoryRateLimitStore {
    windows: Mutex<HashMap<String, RateState>>,
}

impl MemoryRateLimitStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl RateLimitStore for MemoryRateLimitStore {
    async fn hit(&self, key: &str, window: Duration, now: DateTime<Utc>) -> Result<RateState> {
        let mut windows = self.windows.lock().unwrap();
        let live = windows.get(key).filter(|s| s.resets_at > now);
        let state = RateState {
            count: live.map_or(0, |s| s.count) + 1,
            resets_at: live.map_or(now + window, |s| s.resets_at),
        };
        windows.insert(key.to_string(), state);
        Ok(state)
    }

    async fn state(&self, key: &str, now: DateTime<Utc>) -> Result<Option<RateState>> {
        let windows = self.windows.lock().unwrap();
        Ok(windows.get(key).filter(|s| s.resets_at > now).copied())
    }

    async fn reset(&self, key: &str) -> Result<()> {
        self.windows.lock().unwrap().remove(key);
        Ok(())
    }

    async fn sweep(&self, now: DateTime<Utc>) -> Result<()> {
        self.windows
            .lock()
            .unwrap()
            .retain(|_, s| s.resets_at > now);
        Ok(())
    }
}
