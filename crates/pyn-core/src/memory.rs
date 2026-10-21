//! In-memory stores for tests and the dev server; one `Mutex` makes each primitive atomic.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use pyn_object::ObjectId;

use crate::access::{
    AccountKind, AccountRecord, AccountStatus, InviteId, InviteRecord, NewAccount, OrgRole,
    Permission, Role, RoleDefinitions, ServerSettings, ServiceCredentialRecord, SessionRecord,
    SignupStage, SshKeyRecord, TeamRecord, TokenId, TokenRecord, VerificationRecord,
};
use crate::access_service::{
    AccessStore, AccountDisable, AdminRevoke, OrgDeleteMark, OrgMemberChange,
};
use crate::audit::{AuditEvent, AuditQuery, AuditScope, AuditStore, NewAuditEvent, brief};
use crate::branch::{Branch, DEFAULT_BRANCH, conflicts};
use crate::commit_record::{CommitDraft, TreeChange, TreeEdit, draft_commit};
use crate::error::{PynError, Result};
use crate::history::{HistoryCursor, HistoryScan};
use crate::limits::{Limits, LimitsChange, RepoUsage, StorageCap, Usage};
use crate::merge::{MergeWrite, MergeWritten};
use crate::object::{KeyHasher, Keying, ObjectRead, ObjectReader, ObjectStore};
use crate::oidc::{ExternalCreate, ExternalIdentity, IdentityUnlink, NewExternalAccount, OidcFlow};
use crate::ratelimit::{RateLimitStore, RateState};
use crate::repo::{RepoRecord, RepoUpdate, Visibility};
use crate::repo_policy::{
    CreationEffect, CreationRule, CreationScope, CreationSubject, MemberCreation, RepoPolicy,
};
use crate::rules::{Mode, PathFilter};
use crate::store::{MetadataStore, UploadReservation};
use crate::tree::HeadCounts;
use crate::types::{
    ChangesetItem, Commit, CommitFilter, Lock, Person, PushedCommit, RepoId, RepoPath, Revision,
    RevisionId, UserId,
};
use crate::webhook::{
    Delivery, DeliveryQuery, DeliveryStatus, DueDelivery, Jitter, MAX_ACTIVE_SECRETS, NewSecret,
    NewSubscription, OutboxEvent, Subscription, SubscriptionUpdate, WebhookSecret, WebhookStore,
    retry_delay,
};

type Key = (RepoId, RepoPath);

#[derive(Default, Clone)]
struct State {
    locks: HashMap<Key, Lock>,
    revisions: HashMap<Key, Vec<Revision>>,
    repos: HashMap<RepoId, RepoRecord>,
    /// Length of each distinct content in each repository.
    sizes: HashMap<(RepoId, ObjectId), u64>,
    /// Every commit of each repository in the order written; the records carry no revisions.
    commits: HashMap<RepoId, Vec<Commit>>,
    trees: HashMap<(RepoId, ObjectId), Vec<u8>>,
    branches: HashMap<RepoId, Vec<Branch>>,
    /// Per commit, the paths whose revision differs from its first parent's: the revisions the commit made.
    pointers: HashMap<(RepoId, ObjectId), Vec<(RepoPath, RevisionId)>>,
    /// Uploads each repository may reference: size, and whether an assembled object already holds the chunk.
    uploads: HashMap<(RepoId, ObjectId), (u64, bool)>,
    /// Holds on the unreferenced-upload budget of in-flight uploads: repository, bytes, expiry.
    reservations: HashMap<i64, (RepoId, u64, DateTime<Utc>)>,
    next_reservation: i64,
}

impl State {
    fn stored_by(&self, owner: &UserId) -> u64 {
        self.sizes
            .iter()
            .filter(|((r, _), _)| self.repos.get(r).is_some_and(|rec| &rec.owner == owner))
            .map(|(_, size)| size)
            .sum()
    }

    fn names_content(&self, repo: &RepoId, id: &ObjectId) -> bool {
        self.revisions
            .iter()
            .any(|((r, _), revs)| r == repo && revs.iter().any(|v| &v.content == id))
    }

    fn knows(&self, repo: &RepoId, id: &ObjectId) -> bool {
        self.uploads.contains_key(&(repo.clone(), *id)) || self.names_content(repo, id)
    }

    fn unreferenced(&self, repo: &RepoId) -> u64 {
        self.uploads
            .iter()
            .filter(|((r, id), (_, assembled))| {
                r == repo && !assembled && !self.names_content(repo, id)
            })
            .map(|(_, (size, _))| size)
            .sum()
    }

    fn record(&mut self, repo: &RepoId, id: &ObjectId, size: u64, cap: u64) -> Result<()> {
        let key = (repo.clone(), *id);
        if self.uploads.contains_key(&key) {
            return Ok(());
        }
        self.uploads.insert(key.clone(), (size, false));
        let used = self.unreferenced(repo);
        if used > cap {
            self.uploads.remove(&key);
            return Err(PynError::StorageLimitReached {
                owner: repo.to_string(),
                limit: cap,
                used: used - size,
            });
        }
        Ok(())
    }

    fn commit_record(&self, repo: &RepoId, id: &ObjectId) -> Option<&Commit> {
        self.commits.get(repo)?.iter().find(|c| &c.id == id)
    }

    /// `tip` and everything reachable from it through any parent.
    fn ancestors(&self, repo: &RepoId, tip: &ObjectId) -> HashSet<ObjectId> {
        let mut seen = HashSet::new();
        let mut stack = vec![*tip];
        while let Some(id) = stack.pop() {
            if !seen.insert(id) {
                continue;
            }
            if let Some(c) = self.commit_record(repo, &id) {
                stack.extend(c.parents.iter().copied());
            }
        }
        seen
    }

    /// Every path's revision number at `commit`: the nearest pointer along the first-parent chain.
    fn pointers_at(&self, repo: &RepoId, commit: &ObjectId) -> BTreeMap<RepoPath, RevisionId> {
        let mut out = BTreeMap::new();
        let mut next = Some(*commit);
        while let Some(id) = next {
            for (path, rev) in self.pointers.get(&(repo.clone(), id)).into_iter().flatten() {
                out.entry(path.clone()).or_insert(*rev);
            }
            next = self
                .commit_record(repo, &id)
                .and_then(|c| c.parents.first().copied());
        }
        out
    }

    fn revision(&self, repo: &RepoId, path: &RepoPath, id: RevisionId) -> Option<Revision> {
        let revs = self.revisions.get(&(repo.clone(), path.clone()))?;
        revs.iter().find(|r| r.id == id).cloned()
    }

    /// A repository written to without being registered still has its default branch.
    fn ensure_default_branch(&mut self, repo: &RepoId, by: &UserId, now: DateTime<Utc>) {
        let all = self.branches.entry(repo.clone()).or_default();
        if !all.iter().any(|b| b.default) {
            all.push(Branch {
                name: DEFAULT_BRANCH.to_string(),
                head: None,
                default: true,
                created_at: now,
                created_by: by.clone(),
            });
        }
    }

    fn branch(&self, repo: &RepoId, name: &str) -> Result<&Branch> {
        self.branches
            .get(repo)
            .and_then(|v| v.iter().find(|b| b.name == name))
            .ok_or_else(|| PynError::BranchNotFound(name.to_string()))
    }

    /// The commit that appending `items` after `parent` would make, with the trees it adds.
    fn draft(
        &self,
        repo: &RepoId,
        parent: Option<&Commit>,
        items: &[ChangesetItem],
    ) -> Result<CommitDraft> {
        let changes = items.iter().map(|i| i.revision.tree_change()).collect();
        let first = &items[0].revision;
        self.draft_with(
            repo,
            parent,
            parent.map(|c| c.id).into_iter().collect(),
            changes,
            (&first.person, &first.message, first.created_at),
            first.changeset.clone(),
        )
    }

    /// The commit with `parents` over `parent`'s tree with `changes` put in.
    fn draft_with(
        &self,
        repo: &RepoId,
        parent: Option<&Commit>,
        parents: Vec<ObjectId>,
        changes: Vec<TreeChange>,
        (author, message, at): (&Person, &str, DateTime<Utc>),
        changeset: Option<String>,
    ) -> Result<CommitDraft> {
        let mut edit = TreeEdit::new(parent.map(|c| c.tree), changes);
        while let Some(id) = edit.wanted() {
            let body = self
                .trees
                .get(&(repo.clone(), id))
                .ok_or_else(|| PynError::Storage(format!("tree {id} is missing")))?;
            edit.supply(id, body)?;
        }
        draft_commit(parents, edit.finish()?, author, message, at, changeset)
    }

    fn full_commit(&self, repo: &RepoId, id: &ObjectId) -> Option<Commit> {
        let mut commit = self
            .commits
            .get(repo)?
            .iter()
            .find(|c| &c.id == id)?
            .clone();
        commit.revisions = self
            .revisions
            .iter()
            .filter(|((r, _), _)| r == repo)
            .flat_map(|(_, revs)| revs)
            .filter(|r| &r.commit == id)
            .cloned()
            .collect();
        commit.revisions.sort_by(|a, b| a.path.cmp(&b.path));
        Some(commit)
    }

    /// Stores the commit with its trees and pointers, and moves `branch` to it.
    fn append_commit(
        &mut self,
        repo: &RepoId,
        branch: &str,
        draft: CommitDraft,
        revs: &[Revision],
    ) {
        let id = draft.record.id;
        for (tree, body) in draft.trees {
            self.trees.insert((repo.clone(), tree), body);
        }
        self.pointers.insert(
            (repo.clone(), id),
            revs.iter().map(|r| (r.path.clone(), r.id)).collect(),
        );
        self.commits
            .entry(repo.clone())
            .or_default()
            .push(draft.record);
        self.set_head(repo, branch, id);
    }

    fn set_head(&mut self, repo: &RepoId, branch: &str, id: ObjectId) {
        if let Some(b) = self
            .branches
            .get_mut(repo)
            .and_then(|v| v.iter_mut().find(|b| b.name == branch))
        {
            b.head = Some(id);
        }
    }

    /// Appends every item's revision as revision of `commit`, or none: a failing item undoes the ones before it.
    fn apply_items(
        &mut self,
        repo: &RepoId,
        items: Vec<ChangesetItem>,
        now: DateTime<Utc>,
        cap: Option<&StorageCap>,
        commit: &ObjectId,
        at_branch: &BTreeMap<RepoPath, RevisionId>,
    ) -> Result<Vec<Revision>> {
        let undo: Vec<_> = items
            .iter()
            .map(|i| {
                let key = (repo.clone(), i.revision.path.clone());
                let size = (repo.clone(), i.revision.content);
                let prior = (
                    self.locks.get(&key).cloned(),
                    self.revisions.get(&key).map_or(0, Vec::len),
                    self.sizes.get(&size).copied(),
                );
                (key, size, prior)
            })
            .collect();
        let mut out = Vec::with_capacity(items.len());
        for item in items {
            match self.commit(repo, item, now, cap, commit, at_branch) {
                Ok(rev) => out.push(rev),
                Err(e) => {
                    for (key, size, (lock, len, stored)) in undo {
                        if let Some(v) = self.revisions.get_mut(&key) {
                            v.truncate(len);
                        }
                        match lock {
                            Some(l) => self.locks.insert(key, l),
                            None => self.locks.remove(&key),
                        };
                        match stored {
                            Some(n) => self.sizes.insert(size, n),
                            None => self.sizes.remove(&size),
                        };
                    }
                    return Err(e);
                }
            }
        }
        Ok(out)
    }

    /// The whole push on `self`; the caller discards `self` when this fails.
    fn push(
        &mut self,
        repo: &RepoId,
        branch: &str,
        commits: Vec<PushedCommit>,
        pusher: &UserId,
        now: DateTime<Utc>,
        cap: Option<&StorageCap>,
    ) -> Result<Vec<Revision>> {
        self.ensure_default_branch(repo, pusher, now);
        let head = self.branch(repo, branch)?.head;
        let mut tip = head;
        for (i, c) in commits.iter().enumerate() {
            let first = c.record.parents.first().copied();
            let linked = match (i, head) {
                (0, Some(h)) => {
                    first.is_some_and(|p| p == h || self.ancestors(repo, &h).contains(&p))
                }
                (0, None) => first.is_none(),
                _ => first == tip,
            };
            if !linked {
                return Err(PynError::NotFastForward(branch.to_string()));
            }
            for other in c.record.parents.iter().skip(1) {
                if self.commit_record(repo, other).is_none() {
                    return Err(PynError::PushInvalid(format!(
                        "commit {} merges {other}, which is not in the repository",
                        c.record.id
                    )));
                }
            }
            if c.known {
                if head.is_some_and(|h| self.ancestors(repo, &h).contains(&c.record.id)) {
                    return Err(PynError::NotFastForward(branch.to_string()));
                }
            } else if self.commit_record(repo, &c.record.id).is_some() {
                return Err(PynError::PushInvalid(format!(
                    "commit {} is already in the repository",
                    c.record.id
                )));
            }
            tip = Some(c.record.id);
        }

        let mut last_use = BTreeMap::new();
        for (i, c) in commits.iter().enumerate() {
            for item in c
                .items
                .iter()
                .filter(|i| i.mode == Mode::Exclusive && !c.unlocked.contains(&i.path))
            {
                last_use.insert(item.path.clone(), i);
            }
        }
        for path in last_use.keys() {
            match self.locks.get(&(repo.clone(), path.clone())) {
                Some(l) if live(l, now) && &l.owner == pusher => {}
                Some(l) if live(l, now) => {
                    return Err(PynError::LockHeld {
                        path: path.clone(),
                        owner: l.owner.clone(),
                        expires_at: l.expires_at,
                    });
                }
                _ => return Err(PynError::LockRequired(path.clone())),
            }
        }

        let known = commits.iter().take_while(|c| c.known).count();
        let start = match known {
            0 => commits[0].record.parents.first().copied(),
            n => Some(commits[n - 1].record.id),
        };
        let mut pointers = start.map_or_else(BTreeMap::new, |p| self.pointers_at(repo, &p));
        let mut out = Vec::new();
        if known > 0 {
            self.set_head(repo, branch, commits[known - 1].record.id);
        }
        for (i, c) in commits.into_iter().enumerate().skip(known) {
            let items = c
                .items
                .into_iter()
                .map(|revision| ChangesetItem {
                    expected_head: pointers.get(&revision.path).copied(),
                    revision,
                    lock_holder: None,
                })
                .collect();
            let revs = self.apply_items(repo, items, now, cap, &c.record.id, &pointers)?;
            for r in &revs {
                pointers.insert(r.path.clone(), r.id);
                if last_use.get(&r.path) == Some(&i) {
                    self.locks.remove(&(repo.clone(), r.path.clone()));
                }
            }
            for r in &c.adopted {
                pointers.insert(r.path.clone(), r.id);
            }
            let pointed: Vec<Revision> = c.adopted.iter().chain(&revs).cloned().collect();
            let draft = CommitDraft {
                record: c.record,
                trees: c.trees,
            };
            self.append_commit(repo, branch, draft, &pointed);
            out.extend(revs);
        }
        if let Some(h) = head
            && !tip.is_some_and(|t| self.ancestors(repo, &t).contains(&h))
        {
            return Err(PynError::NotFastForward(branch.to_string()));
        }
        Ok(out)
    }

    fn commit(
        &mut self,
        repo: &RepoId,
        item: ChangesetItem,
        now: DateTime<Utc>,
        cap: Option<&StorageCap>,
        commit: &ObjectId,
        at_branch: &BTreeMap<RepoPath, RevisionId>,
    ) -> Result<Revision> {
        let ChangesetItem {
            revision,
            expected_head,
            lock_holder,
        } = item;
        let lock_holder = lock_holder.as_ref();
        let st = self;
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
        let base = at_branch.get(&revision.path).copied();
        if base != expected_head {
            return Err(PynError::StaleBase {
                path: revision.path,
                expected: expected_head,
                actual: base,
            });
        }

        let size_key = (repo.clone(), revision.content);
        if let Some(cap) = cap
            && !st.sizes.contains_key(&size_key)
        {
            let used = st.stored_by(&cap.owner);
            if used + revision.size > cap.max_bytes {
                return Err(PynError::StorageLimitReached {
                    owner: cap.owner.to_string(),
                    limit: cap.max_bytes,
                    used,
                });
            }
        }
        st.sizes.insert(size_key, revision.size);
        let previous = st
            .revisions
            .get(&key)
            .and_then(|v| v.last())
            .map(|r| r.hash.clone());
        let created = revision.create(
            RevisionId(head.map_or(1, |h| h.0 + 1)),
            previous,
            base,
            *commit,
        );
        st.revisions
            .entry(key.clone())
            .or_default()
            .push(created.clone());
        if lock_holder.is_some() {
            st.locks.remove(&key);
        }
        Ok(created)
    }
}

#[derive(Default)]
pub struct MemoryMetadataStore {
    state: Mutex<State>,
}

impl MemoryMetadataStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Rewrites a stored revision in place, the way an attacker with database access would.
    #[cfg(feature = "test-support")]
    pub fn tamper(
        &self,
        repo: &RepoId,
        path: &RepoPath,
        id: RevisionId,
        f: impl FnOnce(&mut Revision),
    ) {
        let mut st = self.state.lock().unwrap();
        let rev = st
            .revisions
            .get_mut(&(repo.clone(), path.clone()))
            .and_then(|v| v.iter_mut().find(|r| r.id == id))
            .expect("revision to tamper with");
        f(rev);
    }
}

#[cfg(feature = "test-support")]
impl MemoryMetadataStore {
    /// Rewrites a stored commit record in place, the way an attacker with database access would.
    pub fn tamper_commit(&self, repo: &RepoId, id: &ObjectId, f: impl FnOnce(&mut Commit)) {
        let mut st = self.state.lock().unwrap();
        let commit = st
            .commits
            .get_mut(repo)
            .and_then(|v| v.iter_mut().find(|c| &c.id == id))
            .expect("commit to tamper with");
        f(commit);
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

    async fn commit_changeset_on(
        &self,
        repo: &RepoId,
        branch: &str,
        items: Vec<ChangesetItem>,
        now: DateTime<Utc>,
        cap: Option<StorageCap>,
    ) -> Result<Vec<Revision>> {
        if items.is_empty() {
            return Ok(Vec::new());
        }
        let mut st = self.state.lock().unwrap();
        let author = &items[0].revision.author;
        st.ensure_default_branch(repo, author, now);
        let head = st.branch(repo, branch)?.head;
        let parent = head.and_then(|h| st.commit_record(repo, &h).cloned());
        let at_branch = head.map_or_else(BTreeMap::new, |h| st.pointers_at(repo, &h));
        let draft = st.draft(repo, parent.as_ref(), &items)?;
        let out = st.apply_items(repo, items, now, cap.as_ref(), &draft.record.id, &at_branch)?;
        st.append_commit(repo, branch, draft, &out);
        Ok(out)
    }

    async fn push_commits_on(
        &self,
        repo: &RepoId,
        branch: &str,
        commits: Vec<PushedCommit>,
        pusher: &UserId,
        now: DateTime<Utc>,
        cap: Option<StorageCap>,
    ) -> Result<Vec<Revision>> {
        if commits.is_empty() {
            return Ok(Vec::new());
        }
        let mut st = self.state.lock().unwrap();
        let mut work = st.clone();
        let out = work.push(repo, branch, commits, pusher, now, cap.as_ref())?;
        *st = work;
        Ok(out)
    }

    async fn commit_merge_on(
        &self,
        repo: &RepoId,
        branch: &str,
        merge: MergeWrite,
        now: DateTime<Utc>,
        cap: Option<StorageCap>,
    ) -> Result<MergeWritten> {
        let mut st = self.state.lock().unwrap();
        let head = st.branch(repo, branch)?.head;
        if head != Some(merge.expected_head) {
            return Err(PynError::MergeStale(branch.to_string()));
        }
        if st.commit_record(repo, &merge.other).is_none() {
            return Err(PynError::CommitNotFound(merge.other.to_string()));
        }
        let parent = st
            .commit_record(repo, &merge.expected_head)
            .cloned()
            .ok_or_else(|| PynError::CommitNotFound(merge.expected_head.to_string()))?;
        let at_branch = st.pointers_at(repo, &merge.expected_head);
        let changes = merge
            .adopted
            .iter()
            .map(|r| TreeChange {
                path: r.path.clone(),
                mode: r.file_mode,
                id: r.content,
                removed: false,
            })
            .chain(merge.items.iter().map(|i| i.revision.tree_change()))
            .collect();
        let draft = st.draft_with(
            repo,
            Some(&parent),
            vec![merge.expected_head, merge.other],
            changes,
            (&merge.author, &merge.message, now),
            None,
        )?;
        let made = st.apply_items(
            repo,
            merge.items,
            now,
            cap.as_ref(),
            &draft.record.id,
            &at_branch,
        )?;
        let commit = draft.record.id;
        let pointed: Vec<Revision> = merge
            .adopted
            .into_iter()
            .chain(made.iter().cloned())
            .collect();
        st.append_commit(repo, branch, draft, &pointed);
        Ok(MergeWritten {
            commit,
            revisions: made,
        })
    }

    async fn fast_forward_branch(
        &self,
        repo: &RepoId,
        branch: &str,
        from: Option<&ObjectId>,
        to: &ObjectId,
    ) -> Result<()> {
        let mut st = self.state.lock().unwrap();
        if st.commit_record(repo, to).is_none() {
            return Err(PynError::CommitNotFound(to.to_string()));
        }
        if st.branch(repo, branch)?.head.as_ref() != from {
            return Err(PynError::MergeStale(branch.to_string()));
        }
        if let Some(b) = st
            .branches
            .get_mut(repo)
            .and_then(|v| v.iter_mut().find(|b| b.name == branch))
        {
            b.head = Some(*to);
        }
        Ok(())
    }

    async fn merge_bases(
        &self,
        repo: &RepoId,
        a: &ObjectId,
        b: &ObjectId,
    ) -> Result<Vec<ObjectId>> {
        let st = self.state.lock().unwrap();
        let from_b = st.ancestors(repo, b);
        let common: Vec<ObjectId> = st
            .ancestors(repo, a)
            .into_iter()
            .filter(|c| from_b.contains(c))
            .collect();
        let mut shadowed = HashSet::new();
        for c in &common {
            let parents = st
                .commit_record(repo, c)
                .map(|r| r.parents.clone())
                .unwrap_or_default();
            for p in parents {
                shadowed.extend(st.ancestors(repo, &p));
            }
        }
        let mut bases: Vec<ObjectId> = common
            .into_iter()
            .filter(|c| !shadowed.contains(c))
            .collect();
        bases.sort_by_key(ObjectId::to_hex);
        Ok(bases)
    }

    async fn list_branches(&self, repo: &RepoId) -> Result<Vec<Branch>> {
        let st = self.state.lock().unwrap();
        let mut all = st.branches.get(repo).cloned().unwrap_or_default();
        all.sort_by(|a, b| b.default.cmp(&a.default).then_with(|| a.name.cmp(&b.name)));
        Ok(all)
    }

    async fn get_branch(&self, repo: &RepoId, name: &str) -> Result<Option<Branch>> {
        let st = self.state.lock().unwrap();
        Ok(st.branch(repo, name).ok().cloned())
    }

    async fn create_branch(
        &self,
        repo: &RepoId,
        name: &str,
        at: &ObjectId,
        by: &UserId,
        now: DateTime<Utc>,
    ) -> Result<Branch> {
        let mut st = self.state.lock().unwrap();
        if st.commit_record(repo, at).is_none() {
            return Err(PynError::CommitNotFound(at.to_string()));
        }
        let all = st.branches.entry(repo.clone()).or_default();
        if all.iter().any(|b| b.name == name) {
            return Err(PynError::BranchExists(name.to_string()));
        }
        let branch = Branch {
            name: name.to_string(),
            head: Some(*at),
            default: false,
            created_at: now,
            created_by: by.clone(),
        };
        all.push(branch.clone());
        Ok(branch)
    }

    async fn delete_branch(
        &self,
        repo: &RepoId,
        name: &str,
        expected: Option<&ObjectId>,
    ) -> Result<()> {
        let mut st = self.state.lock().unwrap();
        let current = st.branch(repo, name)?;
        if current.default {
            return Err(PynError::InvalidRequest(
                "the default branch cannot be deleted".into(),
            ));
        }
        if expected.is_some_and(|e| current.head.as_ref() != Some(e)) {
            return Err(PynError::InvalidRequest(format!(
                "{name} moved while it was being deleted"
            )));
        }
        if let Some(all) = st.branches.get_mut(repo) {
            all.retain(|b| b.name != name);
        }
        Ok(())
    }

    async fn rename_branch(&self, repo: &RepoId, from: &str, to: &str) -> Result<Branch> {
        let mut st = self.state.lock().unwrap();
        if st.branch(repo, from)?.default {
            return Err(PynError::InvalidRequest(
                "the default branch cannot be renamed".into(),
            ));
        }
        let all = st.branches.entry(repo.clone()).or_default();
        if all.iter().any(|b| b.name != from && conflicts(&b.name, to)) {
            return Err(PynError::BranchExists(to.to_string()));
        }
        let branch = all
            .iter_mut()
            .find(|b| b.name == from)
            .expect("checked above");
        branch.name = to.to_string();
        Ok(branch.clone())
    }

    async fn get_commit(&self, repo: &RepoId, id: &ObjectId) -> Result<Option<Commit>> {
        let st = self.state.lock().unwrap();
        Ok(st.full_commit(repo, id))
    }

    async fn commits_by_prefix(
        &self,
        repo: &RepoId,
        prefix: &str,
        limit: usize,
    ) -> Result<Vec<ObjectId>> {
        let st = self.state.lock().unwrap();
        Ok(st
            .commits
            .get(repo)
            .into_iter()
            .flatten()
            .filter(|c| c.id.to_hex().starts_with(prefix))
            .take(limit)
            .map(|c| c.id)
            .collect())
    }

    async fn commits(
        &self,
        repo: &RepoId,
        tip: Option<&ObjectId>,
        before: Option<&ObjectId>,
        limit: usize,
    ) -> Result<Vec<Commit>> {
        let st = self.state.lock().unwrap();
        let all = st.commits.get(repo).map_or(&[][..], Vec::as_slice);
        let reachable = tip.map(|t| st.ancestors(repo, t));
        let end = match before {
            Some(b) => all.iter().position(|c| &c.id == b).unwrap_or(0),
            None => all.len(),
        };
        Ok(all[..end]
            .iter()
            .rev()
            .filter(|c| reachable.as_ref().is_none_or(|r| r.contains(&c.id)))
            .take(limit)
            .filter_map(|c| st.full_commit(repo, &c.id))
            .collect())
    }

    async fn commits_matching(
        &self,
        repo: &RepoId,
        tip: Option<&ObjectId>,
        before: Option<&ObjectId>,
        filter: &CommitFilter,
        limit: usize,
    ) -> Result<Vec<Commit>> {
        let st = self.state.lock().unwrap();
        let all = st.commits.get(repo).map_or(&[][..], Vec::as_slice);
        let reachable = tip.map(|t| st.ancestors(repo, t));
        let excluded = filter.exclude.as_ref().map(|e| st.ancestors(repo, e));
        let end = match before {
            Some(b) => all.iter().position(|c| &c.id == b).unwrap_or(0),
            None => all.len(),
        };
        Ok(all[..end]
            .iter()
            .rev()
            .filter(|c| reachable.as_ref().is_none_or(|r| r.contains(&c.id)))
            .filter(|c| excluded.as_ref().is_none_or(|x| !x.contains(&c.id)))
            .filter(|c| filter.matches_record(c))
            .filter_map(|c| st.full_commit(repo, &c.id))
            .filter(|c| filter.matches_path(c))
            .take(limit)
            .collect())
    }

    async fn missing_commits(
        &self,
        repo: &RepoId,
        tips: &[ObjectId],
        haves: &[ObjectId],
        after: Option<&ObjectId>,
        limit: usize,
        max_examined: usize,
    ) -> Result<Vec<Commit>> {
        let st = self.state.lock().unwrap();
        let all = st.commits.get(repo).map_or(&[][..], Vec::as_slice);
        let index: HashMap<ObjectId, usize> =
            all.iter().enumerate().map(|(i, c)| (c.id, i)).collect();
        let floor = match after {
            Some(a) => {
                *index
                    .get(a)
                    .ok_or_else(|| PynError::CommitNotFound(a.to_string()))?
                    + 1
            }
            None => 0,
        };
        let reach = |from: &[ObjectId]| -> Result<HashSet<usize>> {
            let mut seen = HashSet::new();
            let mut stack: Vec<usize> = from.iter().filter_map(|i| index.get(i).copied()).collect();
            while let Some(at) = stack.pop() {
                if at < floor || !seen.insert(at) {
                    continue;
                }
                if seen.len() > max_examined {
                    return Err(PynError::InvalidRequest(format!(
                        "the request would examine more than {max_examined} commits"
                    )));
                }
                stack.extend(all[at].parents.iter().filter_map(|p| index.get(p).copied()));
            }
            Ok(seen)
        };
        let wanted = reach(tips)?;
        let owned = reach(haves)?;
        let mut out: Vec<usize> = wanted.difference(&owned).copied().collect();
        out.sort_unstable();
        Ok(out
            .into_iter()
            .take(limit)
            .map(|i| all[i].clone())
            .collect())
    }

    async fn is_ancestor(
        &self,
        repo: &RepoId,
        ancestor: &ObjectId,
        descendant: &ObjectId,
    ) -> Result<bool> {
        let st = self.state.lock().unwrap();
        Ok(st.ancestors(repo, descendant).contains(ancestor))
    }

    async fn revision_at(
        &self,
        repo: &RepoId,
        commit: &ObjectId,
        path: &RepoPath,
    ) -> Result<Option<Revision>> {
        let st = self.state.lock().unwrap();
        let at = st.pointers_at(repo, commit);
        Ok(at.get(path).and_then(|id| st.revision(repo, path, *id)))
    }

    async fn commit_pointers(&self, repo: &RepoId, commit: &ObjectId) -> Result<Vec<Revision>> {
        let st = self.state.lock().unwrap();
        let mut out: Vec<Revision> = st
            .pointers
            .get(&(repo.clone(), *commit))
            .into_iter()
            .flatten()
            .filter_map(|(path, id)| st.revision(repo, path, *id))
            .collect();
        out.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(out)
    }

    async fn has_content(&self, repo: &RepoId, content: &ObjectId) -> Result<bool> {
        let st = self.state.lock().unwrap();
        Ok(st
            .revisions
            .iter()
            .any(|((r, _), revs)| r == repo && revs.iter().any(|v| &v.content == content)))
    }

    async fn record_upload(&self, repo: &RepoId, id: &ObjectId, size: u64, cap: u64) -> Result<()> {
        self.state.lock().unwrap().record(repo, id, size, cap)
    }

    async fn object_is_recorded(&self, id: &ObjectId) -> Result<bool> {
        let st = self.state.lock().unwrap();
        Ok(st.uploads.keys().any(|(_, o)| o == id))
    }

    async fn record_assembled(
        &self,
        repo: &RepoId,
        id: &ObjectId,
        size: u64,
        chunks: &[ObjectId],
        cap: u64,
    ) -> Result<()> {
        let mut st = self.state.lock().unwrap();
        if let Some(c) = chunks.iter().find(|c| !st.knows(repo, c)) {
            return Err(PynError::ObjectMissing(c.to_string()));
        }
        let before = st.uploads.clone();
        for c in chunks {
            if let Some(u) = st.uploads.get_mut(&(repo.clone(), *c)) {
                u.1 = true;
            }
        }
        let recorded = st.record(repo, id, size, cap);
        if recorded.is_err() {
            st.uploads = before;
        }
        recorded
    }

    async fn reserve_upload(
        &self,
        repo: &RepoId,
        bytes: u64,
        cap: u64,
        now: DateTime<Utc>,
        ttl_secs: i64,
    ) -> Result<UploadReservation> {
        let mut st = self.state.lock().unwrap();
        st.reservations.retain(|_, (_, _, until)| *until > now);
        let held: u64 = st
            .reservations
            .values()
            .filter(|(r, _, _)| r == repo)
            .map(|(_, b, _)| b)
            .sum();
        let used = st.unreferenced(repo).saturating_add(held);
        if used.saturating_add(bytes) > cap {
            return Err(PynError::StorageLimitReached {
                owner: repo.to_string(),
                limit: cap,
                used,
            });
        }
        st.next_reservation += 1;
        let id = st.next_reservation;
        let until = now + Duration::seconds(ttl_secs);
        st.reservations.insert(id, (repo.clone(), bytes, until));
        Ok(UploadReservation(id))
    }

    async fn release_upload(&self, _repo: &RepoId, reservation: UploadReservation) -> Result<()> {
        self.state
            .lock()
            .unwrap()
            .reservations
            .remove(&reservation.0);
        Ok(())
    }

    async fn unknown_objects(&self, repo: &RepoId, ids: &[ObjectId]) -> Result<Vec<ObjectId>> {
        let st = self.state.lock().unwrap();
        Ok(ids
            .iter()
            .filter(|id| !st.knows(repo, id))
            .copied()
            .collect())
    }

    async fn unreferenced_bytes(&self, repo: &RepoId) -> Result<u64> {
        Ok(self.state.lock().unwrap().unreferenced(repo))
    }

    async fn get_tree(&self, repo: &RepoId, id: &ObjectId) -> Result<Option<Vec<u8>>> {
        let st = self.state.lock().unwrap();
        Ok(st.trees.get(&(repo.clone(), *id)).cloned())
    }

    async fn snapshot(&self, repo: &RepoId, commit: &ObjectId) -> Result<Vec<Revision>> {
        let st = self.state.lock().unwrap();
        Ok(st
            .pointers_at(repo, commit)
            .into_iter()
            .filter_map(|(path, id)| st.revision(repo, &path, id))
            .filter(|r| !r.deleted)
            .collect())
    }

    async fn snapshot_page(
        &self,
        repo: &RepoId,
        commit: &ObjectId,
        prefix: Option<&str>,
        after: Option<&RepoPath>,
        limit: usize,
    ) -> Result<Vec<Revision>> {
        let st = self.state.lock().unwrap();
        Ok(st
            .pointers_at(repo, commit)
            .into_iter()
            .filter(|(path, _)| {
                prefix.is_none_or(|p| path.as_str().starts_with(p))
                    && after.is_none_or(|a| path > a)
            })
            .filter_map(|(path, id)| st.revision(repo, &path, id))
            .filter(|r| !r.deleted)
            .take(limit)
            .collect())
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

    async fn head_counts(&self, repo: &RepoId, commit: &ObjectId) -> Result<HeadCounts> {
        let st = self.state.lock().unwrap();
        if st.commit_record(repo, commit).is_none() {
            return Err(PynError::CommitNotFound(commit.to_hex()));
        }
        let mut counts = HeadCounts::default();
        let mut next = Some(*commit);
        while let Some(id) = next {
            for (path, rev) in st.pointers.get(&(repo.clone(), id)).into_iter().flatten() {
                let at = st.revision(repo, path, *rev).map(|r| r.created_at);
                counts.updated_at = counts.updated_at.max(at);
            }
            next = st
                .commit_record(repo, &id)
                .and_then(|c| c.parents.first().copied());
        }
        for rev in st
            .pointers_at(repo, commit)
            .into_iter()
            .filter_map(|(path, id)| st.revision(repo, &path, id))
            .filter(|r| !r.deleted)
        {
            counts.files += 1;
            counts.exclusive_files += u64::from(rev.mode == Some(Mode::Exclusive));
        }
        Ok(counts)
    }

    async fn list_head_revisions(
        &self,
        repo: &RepoId,
        after: Option<&RepoPath>,
        limit: usize,
    ) -> Result<Vec<Revision>> {
        let st = self.state.lock().unwrap();
        let mut heads: Vec<Revision> = st
            .revisions
            .iter()
            .filter(|((r, p), _)| r == repo && after.is_none_or(|a| p > a))
            .filter_map(|(_, revs)| revs.last().cloned())
            .collect();
        heads.sort_by(|a, b| a.path.cmp(&b.path));
        heads.truncate(limit);
        Ok(heads)
    }

    async fn history(
        &self,
        repo: &RepoId,
        path: &RepoPath,
        within: Option<&ObjectId>,
    ) -> Result<Vec<Revision>> {
        let st = self.state.lock().unwrap();
        let reachable = within.map(|t| st.ancestors(repo, t));
        Ok(st
            .revisions
            .get(&(repo.clone(), path.clone()))
            .into_iter()
            .flatten()
            .filter(|r| reachable.as_ref().is_none_or(|a| a.contains(&r.commit)))
            .cloned()
            .collect())
    }

    async fn history_page(
        &self,
        repo: &RepoId,
        path: &RepoPath,
        within: Option<&ObjectId>,
        after: Option<RevisionId>,
        limit: usize,
    ) -> Result<Vec<Revision>> {
        let st = self.state.lock().unwrap();
        let reachable = within.map(|t| st.ancestors(repo, t));
        Ok(st
            .revisions
            .get(&(repo.clone(), path.clone()))
            .into_iter()
            .flatten()
            .filter(|r| after.is_none_or(|a| r.id > a))
            .filter(|r| reachable.as_ref().is_none_or(|a| a.contains(&r.commit)))
            .take(limit)
            .cloned()
            .collect())
    }

    async fn repo_history(
        &self,
        repo: &RepoId,
        within: Option<&ObjectId>,
        filter: Option<&PathFilter>,
        before: Option<&HistoryCursor>,
        limit: usize,
        budget: usize,
    ) -> Result<HistoryScan> {
        let budget = budget.max(1);
        let st = self.state.lock().unwrap();
        let reachable = within.map(|t| st.ancestors(repo, t));
        let mut revs: Vec<&Revision> = st
            .revisions
            .iter()
            .filter(|((r, _), _)| r == repo)
            .flat_map(|(_, revs)| revs)
            .filter(|r| reachable.as_ref().is_none_or(|a| a.contains(&r.commit)))
            .filter(|r| before.is_none_or(|b| history_key(r) < b.sort_key()))
            .filter(|r| {
                filter
                    .and_then(PathFilter::prefix)
                    .is_none_or(|p| r.path.as_str().starts_with(p))
            })
            .collect();
        revs.sort_by_key(|r| std::cmp::Reverse(history_key(r)));
        let mut revisions = Vec::new();
        for (i, r) in revs.iter().enumerate() {
            if i == budget {
                return Ok(HistoryScan {
                    revisions,
                    resume: Some(HistoryCursor::of(revs[i - 1])),
                });
            }
            if filter.is_none_or(|f| f.is_match(&r.path)) {
                revisions.push((*r).clone());
                if revisions.len() == limit {
                    break;
                }
            }
        }
        Ok(HistoryScan {
            revisions,
            resume: None,
        })
    }

    async fn create_repo_capped(
        &self,
        repo: RepoRecord,
        max_repos: Option<u64>,
    ) -> Result<RepoRecord> {
        let mut st = self.state.lock().unwrap();
        if st
            .repos
            .values()
            .any(|r| r.owner == repo.owner && r.name == repo.name)
        {
            return Err(PynError::RepoExists(repo.address()));
        }
        if let Some(limit) = max_repos
            && st.repos.values().filter(|r| r.owner == repo.owner).count() as u64 >= limit
        {
            return Err(PynError::RepoLimitReached {
                owner: repo.owner.to_string(),
                limit,
            });
        }
        st.ensure_default_branch(&repo.id, &repo.owner, repo.created_at);
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

    async fn list_public_repos(
        &self,
        after: Option<(&UserId, &str)>,
        limit: usize,
    ) -> Result<Vec<RepoRecord>> {
        let st = self.state.lock().unwrap();
        let mut out: Vec<_> = st
            .repos
            .values()
            .filter(|r| r.visibility == Visibility::Public)
            .filter(|r| {
                after.is_none_or(|(o, n)| (r.owner.as_str(), r.name.as_str()) > (o.as_str(), n))
            })
            .cloned()
            .collect();
        out.sort_by(|a, b| (&a.owner, &a.name).cmp(&(&b.owner, &b.name)));
        out.truncate(limit);
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
        st.sizes.retain(|(r, _), _| r != id);
        st.commits.remove(id);
        st.trees.retain(|(r, _), _| r != id);
        st.branches.remove(id);
        st.pointers.retain(|(r, _), _| r != id);
        st.uploads.retain(|(r, _), _| r != id);
        st.reservations.retain(|_, (r, _, _)| r != id);
        Ok(st.repos.remove(id).is_some())
    }

    async fn owner_usage(&self, owner: &UserId) -> Result<Usage> {
        let st = self.state.lock().unwrap();
        Ok(Usage {
            repositories: st.repos.values().filter(|r| &r.owner == owner).count() as u64,
            members: None,
            stored_bytes: st.stored_by(owner),
        })
    }

    async fn repo_usage(&self, repo: &RepoId) -> Result<RepoUsage> {
        let st = self.state.lock().unwrap();
        let in_repo = || st.revisions.iter().filter(|((r, _), _)| r == repo);
        Ok(RepoUsage {
            stored_bytes: st
                .sizes
                .iter()
                .filter(|((r, _), _)| r == repo)
                .map(|(_, size)| size)
                .sum(),
            files: in_repo().count() as u64,
            revisions: in_repo().map(|(_, revs)| revs.len() as u64).sum(),
        })
    }
}

#[derive(Default)]
pub struct MemoryObjectStore {
    objects: Mutex<HashMap<ObjectId, Vec<u8>>>,
    manifests: Mutex<HashMap<ObjectId, Vec<u8>>>,
}

impl MemoryObjectStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl ObjectStore for MemoryObjectStore {
    async fn put_stream(
        &self,
        reader: ObjectReader,
        keying: Keying,
        max: Option<u64>,
    ) -> Result<ObjectId> {
        Ok(self.put_stream_tracked(reader, keying, max).await?.id)
    }

    async fn put_stream_tracked(
        &self,
        reader: ObjectReader,
        keying: Keying,
        max: Option<u64>,
    ) -> Result<crate::Stored> {
        let bytes = crate::object::read_limited(reader, max).await?;
        let mut hasher = KeyHasher::new(keying);
        hasher.update(&bytes)?;
        let id = hasher.finish()?;
        let fresh = self.objects.lock().unwrap().insert(id, bytes).is_none();
        Ok(crate::Stored {
            id,
            created: if fresh { vec![id] } else { Vec::new() },
        })
    }

    async fn read(
        &self,
        hash: &ObjectId,
        range: Option<std::ops::Range<u64>>,
    ) -> Result<Option<ObjectRead>> {
        let Some(bytes) = self.objects.lock().unwrap().get(hash).cloned() else {
            return Ok(None);
        };
        let total = bytes.len() as u64;
        let range = crate::object::clamp_range(range, total);
        let part = bytes[range.start as usize..range.end as usize].to_vec();
        Ok(Some(ObjectRead {
            reader: Box::pin(std::io::Cursor::new(part)),
            total,
            range,
        }))
    }

    async fn delete(&self, hash: &ObjectId) -> Result<bool> {
        let blob = self.objects.lock().unwrap().remove(hash).is_some();
        let manifest = self.manifests.lock().unwrap().remove(hash).is_some();
        Ok(blob || manifest)
    }

    async fn put_manifest(&self, key: &ObjectId, bytes: Vec<u8>) -> Result<()> {
        self.manifests.lock().unwrap().insert(*key, bytes);
        Ok(())
    }

    async fn get_manifest(&self, key: &ObjectId) -> Result<Option<Vec<u8>>> {
        Ok(self.manifests.lock().unwrap().get(key).cloned())
    }

    async fn exists(&self, hash: &ObjectId) -> Result<bool> {
        Ok(self.objects.lock().unwrap().contains_key(hash)
            || self.manifests.lock().unwrap().contains_key(hash))
    }

    async fn size(&self, hash: &ObjectId) -> Result<Option<u64>> {
        Ok(self
            .objects
            .lock()
            .unwrap()
            .get(hash)
            .map(|b| b.len() as u64))
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
    identities: Vec<ExternalIdentity>,
    oidc_flows: HashMap<String, OidcFlow>,
    settings: Option<ServerSettings>,
    limits: BTreeMap<UserId, Limits>,
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
        st.limits.remove(org);
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

    async fn add_org_member_capped(
        &self,
        org: &UserId,
        user: &UserId,
        role: OrgRole,
        max_members: Option<u64>,
    ) -> Result<bool> {
        let mut st = self.state.lock().unwrap();
        let key = (org.clone(), user.clone());
        if st.org_roles.contains_key(&key) {
            return Ok(false);
        }
        if let Some(limit) = max_members
            && st.org_roles.keys().filter(|(o, _)| o == org).count() as u64 >= limit
        {
            return Err(PynError::MemberLimitReached {
                org: org.to_string(),
                limit,
            });
        }
        st.org_roles.insert(key, role);
        Ok(true)
    }

    async fn owner_limits(&self, owner: &UserId) -> Result<Limits> {
        let st = self.state.lock().unwrap();
        Ok(st.limits.get(owner).copied().unwrap_or_default())
    }

    async fn update_owner_limits(&self, owner: &UserId, change: LimitsChange) -> Result<Limits> {
        let mut st = self.state.lock().unwrap();
        let updated = change.apply(st.limits.get(owner).copied().unwrap_or_default());
        st.limits.insert(owner.clone(), updated);
        Ok(updated)
    }

    async fn list_owner_limits(&self) -> Result<Vec<(UserId, Limits)>> {
        let st = self.state.lock().unwrap();
        Ok(st
            .limits
            .iter()
            .filter(|(_, l)| !l.is_unset())
            .map(|(o, l)| (o.clone(), *l))
            .collect())
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

    async fn disable_account(
        &self,
        user: &UserId,
        at: DateTime<Utc>,
        reason: Option<String>,
    ) -> Result<AccountDisable> {
        let mut st = self.state.lock().unwrap();
        let Some(target) = st.accounts.get(user) else {
            return Ok(AccountDisable::NoAccount);
        };
        if target.is_admin && target.disabled_at.is_none() {
            let others = st.accounts.values().any(|a| {
                &a.user != user
                    && a.kind == AccountKind::User
                    && a.is_admin
                    && a.disabled_at.is_none()
            });
            if !others {
                return Ok(AccountDisable::LastAdmin);
            }
        }
        let account = st.accounts.get_mut(user).unwrap();
        (account.disabled_at, account.disabled_reason) = (Some(at), reason);
        Ok(AccountDisable::Disabled)
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

    async fn delete_other_sessions(&self, user: &UserId, keep_id_hash: &str) -> Result<()> {
        self.state
            .lock()
            .unwrap()
            .sessions
            .retain(|id, s| &s.user != user || id == keep_id_hash);
        Ok(())
    }

    async fn create_external_account(&self, new: NewExternalAccount) -> Result<ExternalCreate> {
        let mut st = self.state.lock().unwrap();
        if st.accounts.contains_key(&new.user) {
            return Ok(ExternalCreate::NameTaken);
        }
        if st
            .identities
            .iter()
            .any(|i| i.issuer == new.identity.issuer && i.subject == new.identity.subject)
        {
            return Ok(ExternalCreate::IdentityTaken);
        }
        if let Some(email) = &new.email
            && st
                .accounts
                .values()
                .any(|a| a.email_verified_at.is_some() && a.email.as_ref() == Some(email))
        {
            return Ok(ExternalCreate::EmailTaken);
        }
        let verified_at = new.email.is_some().then_some(new.identity.created_at);
        let account = AccountRecord {
            email: new.email,
            email_verified_at: verified_at,
            ..plain_account(&new.user, new.identity.created_at)
        };
        st.accounts.insert(new.user, account);
        st.identities.push(new.identity);
        Ok(ExternalCreate::Created)
    }

    async fn find_external_identity(
        &self,
        issuer: &str,
        subject: &str,
    ) -> Result<Option<ExternalIdentity>> {
        let st = self.state.lock().unwrap();
        Ok(st
            .identities
            .iter()
            .find(|i| i.issuer == issuer && i.subject == subject)
            .cloned())
    }

    async fn link_external_identity(&self, identity: ExternalIdentity) -> Result<bool> {
        let mut st = self.state.lock().unwrap();
        if st
            .identities
            .iter()
            .any(|i| i.issuer == identity.issuer && i.subject == identity.subject)
        {
            return Ok(false);
        }
        st.identities.push(identity);
        Ok(true)
    }

    async fn list_external_identities(&self, user: &UserId) -> Result<Vec<ExternalIdentity>> {
        let st = self.state.lock().unwrap();
        let mut out: Vec<_> = st
            .identities
            .iter()
            .filter(|i| &i.user == user)
            .cloned()
            .collect();
        out.sort_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)));
        Ok(out)
    }

    async fn unlink_external_identity(&self, user: &UserId, id: &str) -> Result<IdentityUnlink> {
        let mut st = self.state.lock().unwrap();
        let Some(pos) = st
            .identities
            .iter()
            .position(|i| i.id == id && &i.user == user)
        else {
            return Ok(IdentityUnlink::NotFound);
        };
        let links = st.identities.iter().filter(|i| &i.user == user).count();
        if links == 1 && !st.passwords.contains_key(user) {
            return Ok(IdentityUnlink::LastCredential);
        }
        st.identities.remove(pos);
        Ok(IdentityUnlink::Removed)
    }

    async fn touch_external_identity(
        &self,
        id: &str,
        email: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let mut st = self.state.lock().unwrap();
        if let Some(identity) = st.identities.iter_mut().find(|i| i.id == id) {
            identity.last_sign_in_at = Some(now);
            if let Some(email) = email {
                identity.email = Some(email.to_string());
            }
        }
        Ok(())
    }

    async fn put_oidc_flow(&self, flow: OidcFlow) -> Result<()> {
        self.state
            .lock()
            .unwrap()
            .oidc_flows
            .insert(flow.state_hash.clone(), flow);
        Ok(())
    }

    async fn take_oidc_flow(&self, state_hash: &str) -> Result<Option<OidcFlow>> {
        Ok(self.state.lock().unwrap().oidc_flows.remove(state_hash))
    }

    async fn delete_expired_oidc_flows(&self, now: DateTime<Utc>) -> Result<()> {
        self.state
            .lock()
            .unwrap()
            .oidc_flows
            .retain(|_, f| f.expires_at > now);
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

    async fn revoke_sign_in_tokens(&self, user: &UserId, now: DateTime<Utc>) -> Result<()> {
        for token in self.state.lock().unwrap().tokens.values_mut() {
            if token.sign_in && &token.user == user {
                token.revoked_at.get_or_insert(now);
            }
        }
        Ok(())
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

    /// Rewrites a stored event in place, the way an attacker with database access would.
    #[cfg(feature = "test-support")]
    pub fn tamper(&self, id: i64, f: impl FnOnce(&mut AuditEvent)) {
        let mut events = self.events.lock().unwrap();
        let (_, e) = events
            .iter_mut()
            .find(|(_, e)| e.id == id)
            .expect("event to tamper with");
        f(e);
    }

    /// Drops an event, as if deleted from the table.
    #[cfg(feature = "test-support")]
    pub fn remove(&self, id: i64) {
        self.events.lock().unwrap().retain(|(_, e)| e.id != id);
    }
}

#[async_trait]
impl AuditStore for MemoryAuditStore {
    async fn record(&self, scope: &AuditScope, event: NewAuditEvent) -> Result<()> {
        let mut events = self.events.lock().unwrap();
        let id = events.len() as i64 + 1;
        let key = scope.key();
        let previous = events
            .iter()
            .rev()
            .find(|(k, _)| *k == key)
            .map(|(_, e)| e.hash.clone());
        let hash = event.hash(scope, previous.as_deref());
        events.push((
            key,
            AuditEvent {
                id,
                at: event.at,
                actor: event.actor,
                action: event.action,
                path: event.path,
                detail: event.detail,
                previous,
                hash,
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
                    && (query.actions.is_empty() || query.actions.contains(&e.action))
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

    async fn refund(&self, key: &str, now: DateTime<Utc>) -> Result<()> {
        let mut windows = self.windows.lock().unwrap();
        match windows.get_mut(key) {
            Some(s) if s.resets_at > now && s.count > 1 => s.count -= 1,
            Some(s) if s.resets_at > now => {
                windows.remove(key);
            }
            _ => {}
        }
        Ok(())
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

#[derive(Default)]
struct WebhookState {
    next_subscription: i64,
    next_secret: i64,
    next_event: i64,
    subscriptions: BTreeMap<i64, Subscription>,
    secrets: Vec<WebhookSecret>,
    events: BTreeMap<i64, OutboxEvent>,
    deliveries: Vec<Delivery>,
}

/// In-memory webhook store; the passed `now` is its clock.
#[derive(Default)]
pub struct MemoryWebhookStore {
    state: Mutex<WebhookState>,
}

impl MemoryWebhookStore {
    pub fn new() -> Self {
        Self::default()
    }
}

fn delivery_id() -> Result<String> {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).map_err(|e| PynError::Storage(format!("no randomness: {e}")))?;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = hex::encode(b);
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    ))
}

#[async_trait]
impl WebhookStore for MemoryWebhookStore {
    async fn create_subscription(
        &self,
        new: NewSubscription,
        now: DateTime<Utc>,
    ) -> Result<Subscription> {
        let mut s = self.state.lock().unwrap();
        s.next_subscription += 1;
        let sub = Subscription {
            id: s.next_subscription,
            name: new.name,
            url: new.url,
            event_types: new.event_types,
            enabled: new.enabled,
            created_at: now,
            updated_at: now,
            created_by: new.created_by,
        };
        s.subscriptions.insert(sub.id, sub.clone());
        Ok(sub)
    }

    async fn get_subscription(&self, id: i64) -> Result<Option<Subscription>> {
        Ok(self.state.lock().unwrap().subscriptions.get(&id).cloned())
    }

    async fn list_subscriptions(&self) -> Result<Vec<Subscription>> {
        Ok(self
            .state
            .lock()
            .unwrap()
            .subscriptions
            .values()
            .cloned()
            .collect())
    }

    async fn update_subscription(
        &self,
        id: i64,
        update: SubscriptionUpdate,
        now: DateTime<Utc>,
    ) -> Result<Option<Subscription>> {
        let mut s = self.state.lock().unwrap();
        let Some(sub) = s.subscriptions.get_mut(&id) else {
            return Ok(None);
        };
        if let Some(v) = update.name {
            sub.name = v;
        }
        if let Some(v) = update.url {
            sub.url = v;
        }
        if let Some(v) = update.event_types {
            sub.event_types = v;
        }
        if let Some(v) = update.enabled {
            sub.enabled = v;
        }
        sub.updated_at = now;
        Ok(Some(sub.clone()))
    }

    async fn delete_subscription(&self, id: i64) -> Result<bool> {
        let mut s = self.state.lock().unwrap();
        s.secrets.retain(|x| x.subscription_id != id);
        s.deliveries.retain(|d| d.subscription_id != id);
        Ok(s.subscriptions.remove(&id).is_some())
    }

    async fn add_secret(
        &self,
        subscription_id: i64,
        secret: NewSecret,
        now: DateTime<Utc>,
    ) -> Result<Option<WebhookSecret>> {
        let mut s = self.state.lock().unwrap();
        if !s.subscriptions.contains_key(&subscription_id) {
            return Ok(None);
        }
        let active = s
            .secrets
            .iter()
            .filter(|x| x.subscription_id == subscription_id && x.retired_at.is_none())
            .count();
        if active >= MAX_ACTIVE_SECRETS {
            return Err(PynError::InvalidRequest(format!(
                "a webhook has at most {MAX_ACTIVE_SECRETS} active secrets; retire one first"
            )));
        }
        s.next_secret += 1;
        let row = WebhookSecret {
            id: s.next_secret,
            subscription_id,
            secret: secret.secret,
            encrypted: secret.encrypted,
            key_id: secret.key_id,
            created_at: now,
            retired_at: None,
        };
        s.secrets.push(row.clone());
        Ok(Some(row))
    }

    async fn retire_secret(
        &self,
        subscription_id: i64,
        secret_id: i64,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        let mut s = self.state.lock().unwrap();
        let found = s.secrets.iter_mut().find(|x| {
            x.id == secret_id && x.subscription_id == subscription_id && x.retired_at.is_none()
        });
        Ok(match found {
            Some(x) => {
                x.retired_at = Some(now);
                true
            }
            None => false,
        })
    }

    async fn list_active_secrets(&self, subscription_id: i64) -> Result<Vec<WebhookSecret>> {
        let s = self.state.lock().unwrap();
        Ok(s.secrets
            .iter()
            .filter(|x| x.subscription_id == subscription_id && x.retired_at.is_none())
            .cloned()
            .collect())
    }

    async fn append_event(
        &self,
        event_type: &str,
        payload: &str,
        now: DateTime<Utc>,
    ) -> Result<OutboxEvent> {
        let mut s = self.state.lock().unwrap();
        s.next_event += 1;
        let event = OutboxEvent {
            id: s.next_event,
            event_type: event_type.to_string(),
            payload: payload.to_string(),
            created_at: now,
        };
        let targets: Vec<i64> = s
            .subscriptions
            .values()
            .filter(|sub| sub.wants(event_type))
            .map(|sub| sub.id)
            .collect();
        for subscription_id in targets {
            s.deliveries.push(Delivery {
                id: delivery_id()?,
                subscription_id,
                event_id: event.id,
                attempt_count: 0,
                next_attempt_at: now,
                claimed_until: None,
                status: DeliveryStatus::Pending,
                last_status_code: None,
                last_error: None,
                created_at: now,
                delivered_at: None,
                failed_at: None,
            });
        }
        s.events.insert(event.id, event.clone());
        Ok(event)
    }

    async fn claim_due(
        &self,
        limit: u32,
        lease: Duration,
        now: DateTime<Utc>,
    ) -> Result<Vec<DueDelivery>> {
        let mut s = self.state.lock().unwrap();
        let s = &mut *s;
        let mut due: Vec<usize> = (0..s.deliveries.len())
            .filter(|&i| {
                let d = &s.deliveries[i];
                d.status == DeliveryStatus::Pending
                    && d.next_attempt_at <= now
                    && d.claimed_until.is_none_or(|c| c <= now)
                    && s.subscriptions
                        .get(&d.subscription_id)
                        .is_some_and(|sub| sub.enabled)
            })
            .collect();
        due.sort_by_key(|&i| (s.deliveries[i].next_attempt_at, s.deliveries[i].event_id));
        due.truncate(limit as usize);
        let mut out = Vec::new();
        for i in due {
            let d = &mut s.deliveries[i];
            d.claimed_until = Some(now + lease);
            out.push(DueDelivery {
                delivery: d.clone(),
                event: s.events[&d.event_id].clone(),
                subscription: s.subscriptions[&d.subscription_id].clone(),
            });
        }
        Ok(out)
    }

    async fn mark_delivered(
        &self,
        delivery_id: &str,
        status_code: Option<i32>,
        now: DateTime<Utc>,
    ) -> Result<Option<Delivery>> {
        let mut s = self.state.lock().unwrap();
        let Some(d) = s.deliveries.iter_mut().find(|d| d.id == delivery_id) else {
            return Ok(None);
        };
        if d.status == DeliveryStatus::Pending {
            d.attempt_count += 1;
            d.status = DeliveryStatus::Delivered;
            d.last_status_code = status_code;
            d.last_error = None;
            d.claimed_until = None;
            d.delivered_at = Some(now);
        }
        Ok(Some(d.clone()))
    }

    async fn record_failure(
        &self,
        delivery_id: &str,
        status_code: Option<i32>,
        error: &str,
        now: DateTime<Utc>,
        jitter: &dyn Jitter,
    ) -> Result<Option<Delivery>> {
        let mut s = self.state.lock().unwrap();
        let Some(d) = s.deliveries.iter_mut().find(|d| d.id == delivery_id) else {
            return Ok(None);
        };
        if d.status == DeliveryStatus::Pending {
            d.attempt_count += 1;
            d.last_status_code = status_code;
            d.last_error = Some(brief(error));
            d.claimed_until = None;
            match retry_delay(d.attempt_count, jitter.unit()) {
                Some(wait) => d.next_attempt_at = now + wait,
                None => {
                    d.status = DeliveryStatus::Failed;
                    d.failed_at = Some(now);
                }
            }
        }
        Ok(Some(d.clone()))
    }

    async fn list_deliveries(
        &self,
        subscription_id: i64,
        query: &DeliveryQuery,
    ) -> Result<Vec<Delivery>> {
        let s = self.state.lock().unwrap();
        let mut rows: Vec<Delivery> = s
            .deliveries
            .iter()
            .filter(|d| {
                d.subscription_id == subscription_id
                    && query.status.is_none_or(|st| d.status == st)
                    && query.before_event.is_none_or(|b| d.event_id < b)
            })
            .cloned()
            .collect();
        rows.sort_by_key(|d| std::cmp::Reverse(d.event_id));
        rows.truncate(query.limit as usize);
        Ok(rows)
    }

    async fn redeliver(&self, delivery_id: &str, now: DateTime<Utc>) -> Result<Option<Delivery>> {
        let mut s = self.state.lock().unwrap();
        let Some(d) = s.deliveries.iter_mut().find(|d| d.id == delivery_id) else {
            return Ok(None);
        };
        d.status = DeliveryStatus::Pending;
        d.attempt_count = 0;
        d.next_attempt_at = now;
        d.claimed_until = None;
        d.last_status_code = None;
        d.last_error = None;
        d.delivered_at = None;
        d.failed_at = None;
        Ok(Some(d.clone()))
    }

    async fn purge(&self, retention: Duration, now: DateTime<Utc>) -> Result<u64> {
        let mut s = self.state.lock().unwrap();
        let cutoff = now - retention;
        let old: Vec<i64> = s
            .events
            .values()
            .filter(|e| e.created_at < cutoff)
            .map(|e| e.id)
            .collect();
        for id in &old {
            s.events.remove(id);
        }
        s.deliveries.retain(|d| !old.contains(&d.event_id));
        Ok(old.len() as u64)
    }
}
