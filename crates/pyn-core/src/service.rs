mod merging;

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use chrono::Duration;

use crate::access::{Permission, Principal};
use crate::audit::{AuditAction, AuditEvent, AuditQuery, AuditScope, AuditStore, NewAuditEvent};
use crate::branch::{Branch, DEFAULT_BRANCH, conflicts, validate_branch_name};
use crate::clock::Clock;
use crate::commit_record::{TreeChange, tree_of};
use crate::error::{PynError, Result};
use crate::history::{HISTORY_DEFAULT_LIMIT, HISTORY_MAX_LIMIT, HistoryCursor, HistoryPage};
use crate::integrity::{
    BreakKind, Problem, VerifyReport, check_events, verify_audit, verify_revisions,
};
use crate::limits::{StorageCap, StorageCaps};
use crate::object::ObjectStore;
use crate::repo::DEFAULT_MAX_LOCKS_PER_USER;
use crate::rules::{Mode, POLICY_PATH, PathFilter, Rules};
use crate::store::MetadataStore;
use crate::tree::{ACTIVITY_ACTIONS, EntryKind, RepoSummary, TreeEntry};
use crate::types::{
    ChangesetItem, Commit, Lock, NewRevision, RepoId, RepoPath, Revision, RevisionId, UserId,
};
use pyn_object::{FileMode, ObjectId};

/// What one revision writes: the content, its file mode and the revision it restores, if any.
struct Write {
    content: ObjectId,
    file_mode: FileMode,
    restored_from: Option<RevisionId>,
}

fn check_file_mode(mode: FileMode) -> Result<()> {
    match mode {
        FileMode::Regular | FileMode::Executable | FileMode::Symlink => Ok(()),
        other => Err(PynError::InvalidRequest(format!(
            "file mode {} cannot be recorded for a file",
            other.as_str()
        ))),
    }
}

fn changeset_id() -> Result<String> {
    let mut buf = [0u8; 16];
    getrandom::fill(&mut buf).map_err(|e| PynError::Storage(format!("no randomness: {e}")))?;
    Ok(hex::encode(buf))
}

/// One path in a repository listing: its policy, head revision and live lock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    pub path: RepoPath,
    pub mode: Mode,
    pub revision: Option<RevisionId>,
    /// The head revision's file mode; regular when the path has no revision yet.
    pub file_mode: FileMode,
    pub lock: Option<Lock>,
}

/// One path of a changeset as the caller names it: the uploaded content and the revision it was based on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeEntry {
    pub path: RepoPath,
    pub content: ObjectId,
    pub file_mode: FileMode,
    pub base: Option<RevisionId>,
}

/// One path of a snapshot: the revision the commit points at, with the mode `.pyn/pyn.toml` gave it at that commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotEntry {
    pub path: RepoPath,
    pub mode: Mode,
    pub revision: Revision,
}

/// The whole repository as of one commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// The commit the entries are as of; `None` for a repository with no commits.
    pub commit: Option<ObjectId>,
    /// Ordered by path.
    pub entries: Vec<SnapshotEntry>,
}

/// One page of commits, newest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitPage {
    pub commits: Vec<Commit>,
    /// Pass back as `before` for the next, older page; `None` on the last page.
    pub next: Option<ObjectId>,
}

#[derive(Debug, Clone)]
pub struct ServiceConfig {
    /// Lock lifetime; checking out again renews it.
    pub lease: Duration,
    /// Locks one user may hold when the policy file sets no limit.
    pub max_locks: u32,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self {
            lease: Duration::hours(8),
            max_locks: DEFAULT_MAX_LOCKS_PER_USER,
        }
    }
}

/// The per-user lock limit in force and whether the policy file set it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LockLimit {
    pub max: u32,
    pub from_policy: bool,
}

/// The rules in force and the policy revision they came from (`None` for the fallback).
struct Applied {
    revision: Option<RevisionId>,
    rules: Arc<Rules>,
}

/// The effect a policy change would have, as `preview_policy` reports it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PolicyPreview {
    pub to_shared: Vec<RepoPath>,
    pub to_exclusive: Vec<RepoPath>,
    pub released_locks: Vec<Lock>,
}

/// Server-side policy for one repository. Enforcement lives here and in the `MetadataStore` primitives.
pub struct RepoService {
    repo: RepoId,
    /// The rules in force until the repository has a policy file.
    fallback: Arc<Rules>,
    applied: RwLock<Applied>,
    meta: Arc<dyn MetadataStore>,
    objects: Arc<dyn ObjectStore>,
    audit: Arc<dyn AuditStore>,
    clock: Arc<dyn Clock>,
    config: ServiceConfig,
    storage: Option<(UserId, Arc<dyn StorageCaps>)>,
}

impl RepoService {
    pub fn new(
        repo: RepoId,
        rules: Rules,
        meta: Arc<dyn MetadataStore>,
        objects: Arc<dyn ObjectStore>,
        audit: Arc<dyn AuditStore>,
        clock: Arc<dyn Clock>,
        config: ServiceConfig,
    ) -> Self {
        let rules = Arc::new(rules);
        Self {
            repo,
            fallback: rules.clone(),
            applied: RwLock::new(Applied {
                revision: None,
                rules,
            }),
            meta,
            objects,
            audit,
            clock,
            config,
            storage: None,
        }
    }

    /// Enforces the storage ceiling `caps` reports for the repository's `owner`, if one is set.
    pub fn with_storage_caps(mut self, owner: UserId, caps: Arc<dyn StorageCaps>) -> Self {
        self.storage = Some((owner, caps));
        self
    }

    async fn record(
        &self,
        actor: &UserId,
        action: AuditAction,
        path: &RepoPath,
        detail: String,
    ) -> Result<()> {
        let event = NewAuditEvent {
            at: self.clock.now(),
            actor: actor.clone(),
            action,
            path: Some(path.clone()),
            detail,
        };
        self.audit
            .record(&AuditScope::Repo(self.repo.clone()), event)
            .await
    }

    async fn record_repo(&self, actor: &UserId, action: AuditAction, detail: String) -> Result<()> {
        let event = NewAuditEvent {
            at: self.clock.now(),
            actor: actor.clone(),
            action,
            path: None,
            detail,
        };
        self.audit
            .record(&AuditScope::Repo(self.repo.clone()), event)
            .await
    }

    pub fn repo(&self) -> &RepoId {
        &self.repo
    }

    fn rules(&self) -> Arc<Rules> {
        self.applied.read().unwrap().rules.clone()
    }

    pub fn mode_for(&self, path: &RepoPath) -> Mode {
        self.rules().mode_for(path)
    }

    pub fn lock_limit(&self) -> LockLimit {
        let policy = self.rules().max_locks_per_user();
        LockLimit {
            max: policy.unwrap_or(self.config.max_locks),
            from_policy: policy.is_some(),
        }
    }

    /// Applies the head of `.pyn/pyn.toml` if it is newer than the applied one; otherwise the current rules stay.
    pub async fn load_policy(&self) -> Result<()> {
        let policy = RepoPath::new(POLICY_PATH)?;
        let Some(tip) = self.default_head().await? else {
            return Ok(());
        };
        if let Some(head) = self.meta.revision_at(&self.repo, &tip, &policy).await? {
            if self
                .applied
                .read()
                .unwrap()
                .revision
                .is_some_and(|r| r >= head.id)
            {
                return Ok(());
            }
            let rules = self.parse_policy(&head.content).await?;
            self.apply(head.id, rules);
        }
        Ok(())
    }

    async fn parse_policy(&self, content: &ObjectId) -> Result<Rules> {
        let bytes = self
            .objects
            .get(content)
            .await?
            .ok_or_else(|| PynError::ObjectMissing(content.to_string()))?;
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| PynError::InvalidRules(format!("{POLICY_PATH} is not UTF-8 text")))?;
        Rules::from_toml(text)
    }

    /// Swaps in `rules` unless a newer policy revision is already applied; returns the rules replaced.
    fn apply(&self, revision: RevisionId, rules: Rules) -> Option<Arc<Rules>> {
        let mut applied = self.applied.write().unwrap();
        if applied.revision.is_some_and(|current| current >= revision) {
            return None;
        }
        applied.revision = Some(revision);
        Some(std::mem::replace(&mut applied.rules, Arc::new(rules)))
    }

    /// Applies a recorded policy revision's conversions: releases locks on paths that became shared, gives paths that
    /// became exclusive a conversion revision of their current content, and audits each step.
    async fn policy_applied(
        &self,
        who: &Principal,
        rev: &Revision,
        old: &Rules,
        new: &Rules,
    ) -> Result<()> {
        let actor = &who.user;
        let now = self.clock.now();
        let found = self.conversions(old, new).await?;
        for lock in &found.released_locks {
            if self
                .meta
                .force_release_lock(&self.repo, &lock.path, now)
                .await?
                .is_some()
            {
                let detail = format!("released {}'s lock: now shared by policy", lock.owner);
                self.record(actor, AuditAction::ForceUnlock, &lock.path, detail)
                    .await?;
            }
        }
        for path in &found.to_shared {
            let detail = format!("exclusive -> shared by policy r{}", rev.id);
            self.record(actor, AuditAction::ModeConverted, path, detail)
                .await?;
        }
        for path in &found.to_exclusive {
            let detail = match self.convert_to_exclusive(who, path, rev, new).await? {
                Some(conv) => format!(
                    "shared -> exclusive by policy r{}; conversion revision r{}",
                    rev.id, conv.id
                ),
                None => format!("shared -> exclusive by policy r{}", rev.id),
            };
            self.record(actor, AuditAction::ModeConverted, path, detail)
                .await?;
        }
        let detail = format!(
            "r{} applied; {} lock(s) released, {} path(s) to shared, {} to exclusive",
            rev.id,
            found.released_locks.len(),
            found.to_shared.len(),
            found.to_exclusive.len()
        );
        self.record(actor, AuditAction::PolicyChanged, &rev.path, detail)
            .await
    }

    /// Records the path's content on the default branch as the next revision of its single line under the new mode;
    /// no lock is needed. Retries when a concurrent checkin moves the line; `None` if the default branch lacks the path.
    async fn convert_to_exclusive(
        &self,
        who: &Principal,
        path: &RepoPath,
        policy: &Revision,
        new: &Rules,
    ) -> Result<Option<Revision>> {
        for _ in 0..3 {
            let Some(current) = self.revision_on(DEFAULT_BRANCH, path).await? else {
                return Ok(None);
            };
            let line = self.head(path).await?.map(|r| r.id);
            let size = self
                .objects
                .size(&current.content)
                .await?
                .ok_or_else(|| PynError::ObjectMissing(current.content.to_string()))?;
            let revision = NewRevision {
                path: path.clone(),
                content: current.content,
                file_mode: current.file_mode,
                author: who.user.clone(),
                person: who.person(),
                message: format!("Convert to exclusive: policy r{}", policy.id),
                created_at: self.clock.now(),
                restored_from: None,
                mode: new.mode_for(path),
                size,
                changeset: None,
            };
            match self
                .meta
                .commit_revision_capped(&self.repo, revision, line, None, self.clock.now(), None)
                .await
            {
                Err(PynError::StaleBase { .. }) => continue,
                other => return other.map(Some),
            }
        }
        Err(PynError::InvalidRequest(format!(
            "{path} kept changing while converting it to exclusive"
        )))
    }

    /// Paths of the default branch whose resolved mode differs between `old` and `new`, and the live locks on those becoming shared.
    async fn conversions(&self, old: &Rules, new: &Rules) -> Result<PolicyPreview> {
        let mut preview = PolicyPreview::default();
        let mut paths: BTreeMap<RepoPath, ()> = BTreeMap::new();
        if let Some(tip) = self.default_head().await? {
            for rev in self.meta.snapshot(&self.repo, &tip).await? {
                paths.insert(rev.path, ());
            }
        }
        let locks = self.meta.list_locks(&self.repo, self.clock.now()).await?;
        for lock in &locks {
            paths.insert(lock.path.clone(), ());
        }
        for path in paths.into_keys() {
            match (old.mode_for(&path), new.mode_for(&path)) {
                (Mode::Exclusive, Mode::Shared) => preview.to_shared.push(path),
                (Mode::Shared, Mode::Exclusive) => preview.to_exclusive.push(path),
                _ => {}
            }
        }
        preview.released_locks = locks
            .into_iter()
            .filter(|l| preview.to_shared.contains(&l.path))
            .collect();
        Ok(preview)
    }

    /// Validates a policy revision before it is recorded: `who` needs the policy permission and the file must parse.
    async fn check_policy_edit(&self, who: &Principal, content: &ObjectId) -> Result<Rules> {
        who.require(Permission::EditPolicy)?;
        self.parse_policy(content).await
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
        let head = self.head(path).await?.map(|r| r.id);
        if head != base {
            return Err(PynError::StaleBase {
                path: path.clone(),
                expected: base,
                actual: head,
            });
        }
        let now = self.clock.now();
        let lock = self
            .meta
            .acquire_lock(
                &self.repo,
                path,
                user,
                now,
                now + self.config.lease,
                self.lock_limit().max,
            )
            .await?;
        self.record(
            user,
            AuditAction::Checkout,
            path,
            format!("until {}", lock.expires_at),
        )
        .await?;
        Ok(lock)
    }

    pub async fn release(&self, path: &RepoPath, user: &UserId) -> Result<()> {
        self.meta
            .release_lock(&self.repo, path, user, self.clock.now())
            .await?;
        self.record(user, AuditAction::Release, path, "released".into())
            .await
    }

    /// What applying the policy `text` would change: paths whose mode flips and live locks it would release.
    pub async fn preview_policy(&self, who: &Principal, text: &str) -> Result<PolicyPreview> {
        who.require(Permission::EditPolicy)?;
        let new = Rules::from_toml(text)?;
        self.conversions(&self.rules(), &new).await
    }

    /// Removes someone else's live lock. The reason is required because it goes into the audit log.
    pub async fn force_unlock(
        &self,
        path: &RepoPath,
        actor: &UserId,
        reason: &str,
    ) -> Result<Lock> {
        if reason.trim().is_empty() {
            return Err(PynError::InvalidRequest(
                "a reason is required to force an unlock".into(),
            ));
        }
        let lock = self
            .meta
            .force_release_lock(&self.repo, path, self.clock.now())
            .await?
            .ok_or_else(|| PynError::NotLocked(path.clone()))?;
        let detail = format!("removed {}'s lock: {}", lock.owner, reason.trim());
        self.record(actor, AuditAction::ForceUnlock, path, detail)
            .await?;
        Ok(lock)
    }

    /// Audit events, newest first.
    pub async fn audit(&self, query: &AuditQuery) -> Result<Vec<AuditEvent>> {
        let scope = AuditScope::Repo(self.repo.clone());
        check_events(&scope, self.audit.list(&scope, query).await?)
    }

    /// Appends the revision on `branch` under the rules in force; a policy revision is validated first and, on the
    /// default branch, applied once recorded. The first policy revision is the owner's bootstrap: no lock, and the mode
    /// it declares for itself applies.
    async fn commit(
        &self,
        branch: &str,
        who: &Principal,
        path: &RepoPath,
        write: Write,
        base: Option<RevisionId>,
        message: String,
    ) -> Result<Revision> {
        let Write {
            content,
            file_mode,
            restored_from,
        } = write;
        let policy = path.as_str() == POLICY_PATH;
        let parsed = if policy {
            Some(self.check_policy_edit(who, &content).await?)
        } else {
            None
        };
        let on_default = branch == DEFAULT_BRANCH;
        let bootstrap = policy && on_default && base.is_none();
        self.head(path).await?;
        let mode = match &parsed {
            Some(new) if bootstrap => new.mode_for(path),
            _ => self.mode_for(path),
        };
        let lock_holder = (mode == Mode::Exclusive && !bootstrap).then_some(&who.user);
        let now = self.clock.now();
        let size = self
            .objects
            .size(&content)
            .await?
            .ok_or_else(|| PynError::ObjectMissing(content.to_string()))?;
        let cap = self.storage_cap().await?;
        let revision = NewRevision {
            path: path.clone(),
            content,
            file_mode,
            author: who.user.clone(),
            person: who.person(),
            message,
            created_at: now,
            restored_from,
            mode,
            size,
            changeset: None,
        };
        let item = ChangesetItem {
            revision,
            expected_head: base,
            lock_holder: lock_holder.cloned(),
        };
        let mut revs = self
            .meta
            .commit_changeset_on(&self.repo, branch, vec![item], now, cap)
            .await?;
        let rev = revs.remove(0);
        if on_default
            && let Some(new) = parsed
            && let Some(old) = self.apply(rev.id, new.clone())
        {
            self.policy_applied(who, &rev, &old, &new).await?;
        }
        Ok(rev)
    }

    /// Record a revision of `content` (already in the object store). Exclusive paths need the caller's
    /// live lock; every path needs `base` == head. `.pyn/pyn.toml` also needs the policy permission.
    pub async fn checkin(
        &self,
        who: &Principal,
        path: &RepoPath,
        content: ObjectId,
        base: Option<RevisionId>,
        message: String,
    ) -> Result<Revision> {
        self.checkin_as(who, path, content, FileMode::Regular, base, message)
            .await
    }

    /// Like `checkin`, with the git file mode the content has: regular, executable or symlink.
    pub async fn checkin_as(
        &self,
        who: &Principal,
        path: &RepoPath,
        content: ObjectId,
        file_mode: FileMode,
        base: Option<RevisionId>,
        message: String,
    ) -> Result<Revision> {
        self.checkin_on(DEFAULT_BRANCH, who, path, content, file_mode, base, message)
            .await
    }

    /// Like `checkin_as`, as a commit on `branch`. A shared path's `base` is its revision on the branch; an exclusive
    /// path's is its newest revision on any branch.
    #[allow(clippy::too_many_arguments)]
    pub async fn checkin_on(
        &self,
        branch: &str,
        who: &Principal,
        path: &RepoPath,
        content: ObjectId,
        file_mode: FileMode,
        base: Option<RevisionId>,
        message: String,
    ) -> Result<Revision> {
        check_file_mode(file_mode)?;
        if !self.objects.exists(&content).await? {
            return Err(PynError::ObjectMissing(content.to_string()));
        }
        let rev = self
            .commit(
                branch,
                who,
                path,
                Write {
                    content,
                    file_mode,
                    restored_from: None,
                },
                base,
                message,
            )
            .await?;
        self.record(
            &who.user,
            AuditAction::Checkin,
            path,
            format!("r{}: {}", rev.id, rev.message),
        )
        .await?;
        Ok(rev)
    }

    /// Checks several paths in as one atomic commit with one message: every stale-base, lock and storage check
    /// passes or nothing is written. `.pyn/pyn.toml` is not allowed here; it has its own validated flow.
    pub async fn checkin_changeset(
        &self,
        who: &Principal,
        entries: Vec<ChangeEntry>,
        message: String,
    ) -> Result<Vec<Revision>> {
        self.checkin_changeset_on(DEFAULT_BRANCH, who, entries, message)
            .await
    }

    /// Like `checkin_changeset`, as one commit on `branch`.
    pub async fn checkin_changeset_on(
        &self,
        branch: &str,
        who: &Principal,
        entries: Vec<ChangeEntry>,
        message: String,
    ) -> Result<Vec<Revision>> {
        if entries.is_empty() {
            return Err(PynError::InvalidRequest(
                "a changeset needs at least one path".into(),
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        for e in &entries {
            if e.path.as_str() == POLICY_PATH {
                return Err(PynError::InvalidRequest(format!(
                    "{POLICY_PATH} cannot be part of a changeset"
                )));
            }
            if !seen.insert(&e.path) {
                return Err(PynError::InvalidRequest(format!(
                    "{} appears twice in the changeset",
                    e.path
                )));
            }
        }
        for e in &entries {
            check_file_mode(e.file_mode)?;
            self.head(&e.path).await?;
        }
        let now = self.clock.now();
        let changeset = changeset_id()?;
        let cap = self.storage_cap().await?;
        let mut items = Vec::with_capacity(entries.len());
        for e in entries {
            let size = self
                .objects
                .size(&e.content)
                .await?
                .ok_or_else(|| PynError::ObjectMissing(e.content.to_string()))?;
            let mode = self.mode_for(&e.path);
            items.push(ChangesetItem {
                revision: NewRevision {
                    path: e.path,
                    content: e.content,
                    file_mode: e.file_mode,
                    author: who.user.clone(),
                    person: who.person(),
                    message: message.clone(),
                    created_at: now,
                    restored_from: None,
                    mode,
                    size,
                    changeset: Some(changeset.clone()),
                },
                expected_head: e.base,
                lock_holder: (mode == Mode::Exclusive).then(|| who.user.clone()),
            });
        }
        let revs = self
            .meta
            .commit_changeset_on(&self.repo, branch, items, now, cap)
            .await?;
        for rev in &revs {
            self.record(
                &who.user,
                AuditAction::Checkin,
                &rev.path,
                format!("r{}: {} (changeset {changeset})", rev.id, rev.message),
            )
            .await?;
        }
        Ok(revs)
    }

    async fn storage_cap(&self) -> Result<Option<StorageCap>> {
        match &self.storage {
            Some((owner, caps)) => Ok(caps.storage_cap(owner).await?.map(|max_bytes| StorageCap {
                owner: owner.clone(),
                max_bytes,
            })),
            None => Ok(None),
        }
    }

    /// Makes an older revision's content the new head as a fresh revision marked `restored_from`; history is untouched.
    /// Needs the caller's live lock, `base` equal to the head, and `confirm` naming the head being replaced.
    pub async fn restore(
        &self,
        who: &Principal,
        path: &RepoPath,
        source: RevisionId,
        base: RevisionId,
        confirm: &str,
        message: Option<String>,
    ) -> Result<Revision> {
        self.restore_on(DEFAULT_BRANCH, who, path, source, base, confirm, message)
            .await
    }

    /// Like `restore`, as a commit on `branch`.
    #[allow(clippy::too_many_arguments)]
    pub async fn restore_on(
        &self,
        branch: &str,
        who: &Principal,
        path: &RepoPath,
        source: RevisionId,
        base: RevisionId,
        confirm: &str,
        message: Option<String>,
    ) -> Result<Revision> {
        if self.mode_for(path) != Mode::Exclusive {
            return Err(PynError::NotExclusive(path.clone()));
        }
        let expected = format!("{path}@r{base}");
        if confirm != expected {
            return Err(PynError::ConfirmationRequired { expected });
        }
        let old = self
            .meta
            .get_revision(&self.repo, path, source)
            .await?
            .ok_or_else(|| PynError::RevisionNotFound {
                path: path.clone(),
                revision: source.to_string(),
            })?;
        if source == base {
            return Err(PynError::InvalidRequest(format!(
                "r{source} is already the head"
            )));
        }
        if !self.objects.exists(&old.content).await? {
            return Err(PynError::ObjectMissing(old.content.to_string()));
        }
        let message = message.unwrap_or_else(|| format!("Restore r{source}"));
        let rev = self
            .commit(
                branch,
                who,
                path,
                Write {
                    content: old.content,
                    file_mode: old.file_mode,
                    restored_from: Some(source),
                },
                Some(base),
                message,
            )
            .await?;
        self.record(
            &who.user,
            AuditAction::Restore,
            path,
            format!("r{} restored from r{source}", rev.id),
        )
        .await?;
        Ok(rev)
    }

    pub async fn locks(&self) -> Result<Vec<Lock>> {
        self.meta.list_locks(&self.repo, self.clock.now()).await
    }

    /// Paths with a revision or a live lock, ordered by path, starting after `after`.
    pub async fn files(&self, after: Option<&RepoPath>, limit: usize) -> Result<Vec<FileEntry>> {
        self.files_on(DEFAULT_BRANCH, after, limit).await
    }

    /// Like `files`, with each path's revision as of the head of `branch`.
    pub async fn files_on(
        &self,
        branch: &str,
        after: Option<&RepoPath>,
        limit: usize,
    ) -> Result<Vec<FileEntry>> {
        let now = self.clock.now();
        let mut entries: BTreeMap<RepoPath, FileEntry> = BTreeMap::new();
        for rev in self.heads_on(branch).await? {
            let mode = self.mode_for(&rev.path);
            entries.insert(
                rev.path.clone(),
                FileEntry {
                    path: rev.path,
                    mode,
                    revision: Some(rev.id),
                    file_mode: rev.file_mode,
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
                    file_mode: FileMode::Regular,
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

    /// Revision on `branch` and live lock of every path that has either, ordered by path.
    async fn live_paths(
        &self,
        branch: &str,
    ) -> Result<BTreeMap<RepoPath, (Option<Revision>, Option<Lock>)>> {
        let mut paths: BTreeMap<RepoPath, (Option<Revision>, Option<Lock>)> = BTreeMap::new();
        for rev in self.heads_on(branch).await? {
            paths.insert(rev.path.clone(), (Some(rev), None));
        }
        for lock in self.meta.list_locks(&self.repo, self.clock.now()).await? {
            let key = lock.path.clone();
            paths.entry(key).or_default().1 = Some(lock);
        }
        Ok(paths)
    }

    /// The children of `dir` (the root when `None`): folders first, then files, each by name. A folder's mode is
    /// `Mixed` unless every file under it has the same mode.
    pub async fn tree(&self, dir: Option<&RepoPath>) -> Result<Vec<TreeEntry>> {
        self.tree_on(DEFAULT_BRANCH, dir).await
    }

    /// Like `tree`, as of the head of `branch`.
    pub async fn tree_on(&self, branch: &str, dir: Option<&RepoPath>) -> Result<Vec<TreeEntry>> {
        let paths = self.live_paths(branch).await?;
        let prefix = dir.map_or(String::new(), |d| format!("{d}/"));
        let mut children: BTreeMap<(EntryKind, String), TreeEntry> = BTreeMap::new();
        for (path, (rev, lock)) in &paths {
            let Some(rest) = path.as_str().strip_prefix(&prefix) else {
                continue;
            };
            let (kind, name) = match rest.split_once('/') {
                Some((first, _)) => (EntryKind::Folder, first),
                None => (EntryKind::File, rest),
            };
            let mode = self.mode_for(path);
            match children.entry((kind, name.to_string())) {
                std::collections::btree_map::Entry::Vacant(v) => {
                    let child = RepoPath::new(format!("{prefix}{name}"))?;
                    v.insert(TreeEntry {
                        name: name.to_string(),
                        path: child,
                        kind,
                        mode: mode.into(),
                        last_change: rev.clone(),
                        lock: if kind == EntryKind::File {
                            lock.clone()
                        } else {
                            None
                        },
                    });
                }
                std::collections::btree_map::Entry::Occupied(mut o) => {
                    let entry = o.get_mut();
                    entry.mode = entry.mode.merge(mode);
                    if let Some(rev) = rev
                        && entry
                            .last_change
                            .as_ref()
                            .is_none_or(|c| rev.created_at > c.created_at)
                    {
                        entry.last_change = Some(rev.clone());
                    }
                }
            }
        }
        if children.is_empty()
            && let Some(dir) = dir
        {
            return Err(if paths.contains_key(dir) {
                PynError::InvalidRequest(format!("{dir} is a file, not a folder"))
            } else {
                PynError::PathNotFound(dir.to_string())
            });
        }
        Ok(children.into_values().collect())
    }

    /// Counts, live locks and the newest `activity_limit` file and lock events.
    pub async fn summary(&self, activity_limit: usize) -> Result<RepoSummary> {
        let paths = self.live_paths(DEFAULT_BRANCH).await?;
        let (mut files, mut exclusive_files, mut updated_at) = (0, 0, None);
        for (path, (rev, _)) in &paths {
            let Some(rev) = rev else { continue };
            files += 1;
            if self.mode_for(path) == Mode::Exclusive {
                exclusive_files += 1;
            }
            updated_at = updated_at.max(Some(rev.created_at));
        }
        let mut activity = Vec::new();
        for action in ACTIVITY_ACTIONS {
            let query = AuditQuery {
                action: Some(action),
                limit: activity_limit,
                ..AuditQuery::default()
            };
            let scope = AuditScope::Repo(self.repo.clone());
            activity.extend(check_events(
                &scope,
                self.audit.list(&scope, &query).await?,
            )?);
        }
        activity.sort_by_key(|e| std::cmp::Reverse(e.id));
        activity.truncate(activity_limit);
        Ok(RepoSummary {
            default_branch: DEFAULT_BRANCH.to_string(),
            branch_count: self.branches().await?.len() as u32,
            files,
            exclusive_files,
            shared_files: files - exclusive_files,
            updated_at,
            locks: paths.into_values().filter_map(|(_, lock)| lock).collect(),
            activity,
        })
    }

    /// A revision and its content; the head of the default branch when `revision` is `None`.
    pub async fn read(
        &self,
        path: &RepoPath,
        revision: Option<RevisionId>,
    ) -> Result<(Revision, Vec<u8>)> {
        self.read_on(DEFAULT_BRANCH, path, revision).await
    }

    /// Like `read`, taking the path's revision at the head of `branch` when `revision` is `None`.
    pub async fn read_on(
        &self,
        branch: &str,
        path: &RepoPath,
        revision: Option<RevisionId>,
    ) -> Result<(Revision, Vec<u8>)> {
        let rev = self.find_revision(branch, path, revision).await?;
        rev.verify()?;
        let bytes = self
            .objects
            .get(&rev.content)
            .await?
            .ok_or_else(|| PynError::ObjectMissing(rev.content.to_string()))?;
        Ok((rev, bytes))
    }

    async fn find_revision(
        &self,
        branch: &str,
        path: &RepoPath,
        revision: Option<RevisionId>,
    ) -> Result<Revision> {
        let found = match revision {
            Some(id) => self.meta.get_revision(&self.repo, path, id).await?,
            None => self.revision_on(branch, path).await?,
        };
        found.ok_or_else(|| PynError::RevisionNotFound {
            path: path.clone(),
            revision: revision.map_or("(head)".to_string(), |r| r.to_string()),
        })
    }

    /// Like `read`, but streams the content, or the byte `range` of it.
    pub async fn read_stream(
        &self,
        path: &RepoPath,
        revision: Option<RevisionId>,
        range: Option<std::ops::Range<u64>>,
    ) -> Result<(Revision, crate::ObjectRead)> {
        self.read_stream_on(DEFAULT_BRANCH, path, revision, range)
            .await
    }

    /// Like `read_stream`, taking the path's revision at the head of `branch` when `revision` is `None`.
    pub async fn read_stream_on(
        &self,
        branch: &str,
        path: &RepoPath,
        revision: Option<RevisionId>,
        range: Option<std::ops::Range<u64>>,
    ) -> Result<(Revision, crate::ObjectRead)> {
        let rev = self.find_revision(branch, path, revision).await?;
        rev.verify()?;
        let read = self
            .objects
            .read(&rev.content, range)
            .await?
            .ok_or_else(|| PynError::ObjectMissing(rev.content.to_string()))?;
        Ok((rev, read))
    }

    /// The path's revision at the head of `branch`, checked against its hash.
    pub async fn revision_on(&self, branch: &str, path: &RepoPath) -> Result<Option<Revision>> {
        let Some(tip) = self.head_of(branch).await? else {
            return Ok(None);
        };
        let rev = self.meta.revision_at(&self.repo, &tip, path).await?;
        rev.iter().try_for_each(Revision::verify)?;
        Ok(rev)
    }

    /// Every path's revision at the head of `branch`, ordered by path.
    async fn heads_on(&self, branch: &str) -> Result<Vec<Revision>> {
        match self.head_of(branch).await? {
            Some(tip) => self.meta.snapshot(&self.repo, &tip).await,
            None => Ok(Vec::new()),
        }
    }

    pub async fn head(&self, path: &RepoPath) -> Result<Option<Revision>> {
        let head = self.meta.head_revision(&self.repo, path).await?;
        head.iter().try_for_each(Revision::verify)?;
        Ok(head)
    }

    /// A path's revisions on every branch, oldest first, after checking each hash and the links between them.
    pub async fn history(&self, path: &RepoPath) -> Result<Vec<Revision>> {
        let revisions = self.meta.history(&self.repo, path, None).await?;
        if let Some((id, kind)) = verify_revisions(&revisions).first() {
            return Err(PynError::HistoryCorrupt(format!(
                "{path}@r{id}: {}",
                kind.as_str()
            )));
        }
        Ok(revisions)
    }

    /// The part of a path's history that `branch` contains, oldest first; the whole line is checked first.
    pub async fn history_on(&self, branch: &str, path: &RepoPath) -> Result<Vec<Revision>> {
        let all = self.history(path).await?;
        let Some(tip) = self.head_of(branch).await? else {
            return Ok(Vec::new());
        };
        let kept = self.meta.history(&self.repo, path, Some(&tip)).await?;
        let ids: std::collections::BTreeSet<_> = kept.iter().map(|r| r.id).collect();
        Ok(all.into_iter().filter(|r| ids.contains(&r.id)).collect())
    }

    /// Checks every path's revision chain, and the audit log when `audit` is set, reporting all breaks instead of
    /// stopping at the first. With `path`, only that path.
    pub async fn verify(&self, path: Option<&RepoPath>, audit: bool) -> Result<VerifyReport> {
        let paths = match path {
            Some(p) => vec![p.clone()],
            None => self
                .meta
                .list_head_revisions(&self.repo)
                .await?
                .into_iter()
                .map(|r| r.path)
                .collect(),
        };
        let mut report = VerifyReport::default();
        for p in paths {
            let revisions = self.meta.history(&self.repo, &p, None).await?;
            if revisions.is_empty() {
                return Err(PynError::PathNotFound(p.to_string()));
            }
            report.paths += 1;
            report.revisions += revisions.len() as u64;
            for (id, kind) in verify_revisions(&revisions) {
                report.problems.push(Problem {
                    kind,
                    path: Some(p.clone()),
                    revision: Some(id),
                    event: None,
                    commit: None,
                });
            }
        }
        if path.is_none() {
            self.verify_commits(&mut report).await?;
        }
        if audit && path.is_none() {
            let scope = AuditScope::Repo(self.repo.clone());
            let mut events = Vec::new();
            let mut before = None;
            loop {
                let page = self
                    .audit
                    .list(
                        &scope,
                        &AuditQuery {
                            before,
                            limit: HISTORY_MAX_LIMIT,
                            ..AuditQuery::default()
                        },
                    )
                    .await?;
                let Some(last) = page.last() else { break };
                before = Some(last.id);
                events.extend(page);
            }
            events.reverse();
            report.audit_events = Some(events.len() as u64);
            for (id, kind) in verify_audit(&scope, &events) {
                report.problems.push(Problem {
                    kind,
                    path: None,
                    revision: None,
                    event: Some(id),
                    commit: None,
                });
            }
        }
        Ok(report)
    }

    /// Checks every commit's id against its fields and its parent links, and each branch head's tree against the
    /// snapshot at that head.
    async fn verify_commits(&self, report: &mut VerifyReport) -> Result<()> {
        let mut all = Vec::new();
        let mut before = None;
        loop {
            let page = self
                .meta
                .commits(&self.repo, None, before.as_ref(), HISTORY_MAX_LIMIT)
                .await?;
            let Some(last) = page.last() else { break };
            before = Some(last.id);
            all.extend(page);
        }
        all.reverse();
        report.commits = all.len() as u64;
        let problem = |kind, c: &Commit| Problem {
            kind,
            path: None,
            revision: None,
            event: None,
            commit: Some(c.id),
        };
        let mut seen = std::collections::HashSet::new();
        for c in &all {
            if c.expected_id()? != c.id {
                report.problems.push(problem(BreakKind::Hash, c));
            }
            let linked = c.parents.iter().all(|p| seen.contains(p))
                && (!c.parents.is_empty() || seen.is_empty());
            if !linked {
                report.problems.push(problem(BreakKind::Link, c));
            }
            seen.insert(c.id);
        }
        for branch in self.meta.list_branches(&self.repo).await? {
            let Some(head) = branch.head.and_then(|h| all.iter().find(|c| c.id == h)) else {
                continue;
            };
            let files = self
                .meta
                .snapshot(&self.repo, &head.id)
                .await?
                .iter()
                .map(|r| TreeChange {
                    path: r.path.clone(),
                    mode: r.file_mode,
                    id: r.content,
                })
                .collect();
            if tree_of(files)?.root != head.tree {
                report.problems.push(problem(BreakKind::Tree, head));
            }
        }
        Ok(())
    }

    /// Repository-wide history, newest first; `limit` defaults to 50 and is capped at 200.
    pub async fn repo_history(
        &self,
        filter: Option<&PathFilter>,
        before: Option<&HistoryCursor>,
        limit: Option<usize>,
    ) -> Result<HistoryPage> {
        self.repo_history_on(DEFAULT_BRANCH, filter, before, limit)
            .await
    }

    /// Like `repo_history`, limited to the revisions `branch` contains.
    pub async fn repo_history_on(
        &self,
        branch: &str,
        filter: Option<&PathFilter>,
        before: Option<&HistoryCursor>,
        limit: Option<usize>,
    ) -> Result<HistoryPage> {
        let limit = limit
            .unwrap_or(HISTORY_DEFAULT_LIMIT)
            .clamp(1, HISTORY_MAX_LIMIT);
        let Some(tip) = self.head_of(branch).await? else {
            return Ok(HistoryPage {
                revisions: Vec::new(),
                next: None,
            });
        };
        let mut revisions = self
            .meta
            .repo_history(&self.repo, Some(&tip), filter, before, limit + 1)
            .await?;
        revisions.iter().try_for_each(Revision::verify)?;
        let next = (revisions.len() > limit).then(|| {
            revisions.truncate(limit);
            HistoryCursor::of(&revisions[limit - 1])
        });
        Ok(HistoryPage { revisions, next })
    }

    /// The head of the default branch; `None` for a repository with no commits.
    pub async fn head_commit(&self) -> Result<Option<ObjectId>> {
        self.default_head().await
    }

    async fn default_head(&self) -> Result<Option<ObjectId>> {
        self.head_of(DEFAULT_BRANCH).await
    }

    /// The head of `branch`; `BranchNotFound` unless it is the default branch, which exists from the start.
    pub async fn head_of(&self, branch: &str) -> Result<Option<ObjectId>> {
        Ok(self.branch(branch).await?.head)
    }

    /// A branch by name. The default branch always exists, empty until its first commit.
    pub async fn branch(&self, name: &str) -> Result<Branch> {
        match self.meta.get_branch(&self.repo, name).await? {
            Some(b) => Ok(b),
            None if name == DEFAULT_BRANCH => Ok(self.empty_default()),
            None => Err(PynError::BranchNotFound(name.to_string())),
        }
    }

    fn empty_default(&self) -> Branch {
        Branch {
            name: DEFAULT_BRANCH.to_string(),
            head: None,
            default: true,
            created_at: self.clock.now(),
            created_by: UserId::new(""),
        }
    }

    /// The branches, the default one first, then by name.
    pub async fn branches(&self) -> Result<Vec<Branch>> {
        let mut all = self.meta.list_branches(&self.repo).await?;
        if !all.iter().any(|b| b.default) {
            all.insert(0, self.empty_default());
        }
        Ok(all)
    }

    /// Makes `name` at `start`: a branch name, a commit id or abbreviation, or the default branch's head if `None`.
    pub async fn create_branch(
        &self,
        who: &Principal,
        name: &str,
        start: Option<&str>,
    ) -> Result<Branch> {
        who.require(Permission::Checkin)?;
        validate_branch_name(name)?;
        if self
            .branches()
            .await?
            .iter()
            .any(|b| conflicts(&b.name, name))
        {
            return Err(PynError::BranchExists(name.to_string()));
        }
        let at = match start {
            None => self.default_head().await?,
            Some(spec) => match self.meta.get_branch(&self.repo, spec).await? {
                Some(b) => b.head,
                None => Some(self.resolve_commit(spec).await?),
            },
        }
        .ok_or_else(|| {
            PynError::InvalidRequest("there is no commit to start the branch from yet".into())
        })?;
        let now = self.clock.now();
        let branch = self
            .meta
            .create_branch(&self.repo, name, &at, &who.user, now)
            .await?;
        self.record_repo(
            &who.user,
            AuditAction::BranchCreated,
            format!("{name} at {at}"),
        )
        .await?;
        Ok(branch)
    }

    /// Removes the name. Without `force` the branch must be contained in the default branch, so no commit is lost
    /// from view; the default branch is never deleted.
    pub async fn delete_branch(&self, who: &Principal, name: &str, force: bool) -> Result<()> {
        who.require(Permission::Checkin)?;
        let branch = self.branch(name).await?;
        if branch.default {
            return Err(PynError::InvalidRequest(
                "the default branch cannot be deleted".into(),
            ));
        }
        if !force
            && let (Some(head), Some(tip)) = (branch.head, self.default_head().await?)
            && !self.meta.is_ancestor(&self.repo, &head, &tip).await?
        {
            return Err(PynError::BranchNotMerged(name.to_string()));
        }
        self.meta
            .delete_branch(&self.repo, name, branch.head.as_ref())
            .await?;
        let at = branch.head.map_or_else(String::new, |h| format!(" at {h}"));
        self.record_repo(&who.user, AuditAction::BranchDeleted, format!("{name}{at}"))
            .await
    }

    /// Renames a branch other than the default one.
    pub async fn rename_branch(&self, who: &Principal, from: &str, to: &str) -> Result<Branch> {
        who.require(Permission::Checkin)?;
        validate_branch_name(to)?;
        let branch = self.meta.rename_branch(&self.repo, from, to).await?;
        self.record_repo(
            &who.user,
            AuditAction::BranchRenamed,
            format!("{from} to {to}"),
        )
        .await?;
        Ok(branch)
    }

    /// A full commit id, or an unambiguous prefix of at least `MIN_COMMIT_PREFIX` hex digits.
    pub async fn resolve_commit(&self, spec: &str) -> Result<ObjectId> {
        let spec = spec.trim();
        let hex = spec.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        if !hex || spec.len() < MIN_COMMIT_PREFIX {
            return Err(PynError::CommitNotFound(spec.to_string()));
        }
        let mut found = self.meta.commits_by_prefix(&self.repo, spec, 2).await?;
        match found.len() {
            0 => Err(PynError::CommitNotFound(spec.to_string())),
            1 => Ok(found.remove(0)),
            _ => Err(PynError::AmbiguousCommit(spec.to_string())),
        }
    }

    pub async fn get_commit(&self, id: &ObjectId) -> Result<Commit> {
        let commit = self
            .meta
            .get_commit(&self.repo, id)
            .await?
            .ok_or_else(|| PynError::CommitNotFound(id.to_string()))?;
        commit.verify()?;
        commit.revisions.iter().try_for_each(Revision::verify)?;
        Ok(commit)
    }

    /// Commits of the default branch newest first; `limit` defaults to 50 and is capped at 200.
    pub async fn commits(
        &self,
        before: Option<&ObjectId>,
        limit: Option<usize>,
    ) -> Result<CommitPage> {
        self.commits_on(DEFAULT_BRANCH, before, limit).await
    }

    /// The commits `branch` contains, newest first, paged like `commits`.
    pub async fn commits_on(
        &self,
        branch: &str,
        before: Option<&ObjectId>,
        limit: Option<usize>,
    ) -> Result<CommitPage> {
        let Some(tip) = self.head_of(branch).await? else {
            return Ok(CommitPage {
                commits: Vec::new(),
                next: None,
            });
        };
        let limit = limit
            .unwrap_or(HISTORY_DEFAULT_LIMIT)
            .clamp(1, HISTORY_MAX_LIMIT);
        if let Some(b) = before {
            self.get_commit(b).await?;
        }
        let mut commits = self
            .meta
            .commits(&self.repo, Some(&tip), before, limit + 1)
            .await?;
        for c in &commits {
            c.verify()?;
            c.revisions.iter().try_for_each(Revision::verify)?;
        }
        let next = (commits.len() > limit).then(|| {
            commits.truncate(limit);
            commits[limit - 1].id
        });
        Ok(CommitPage { commits, next })
    }

    /// The rules in force at `commit`, whose snapshot is `revisions`: the policy file in it when that revision is on the
    /// default branch's history. A commit that is not on that history takes the default branch's current rules, as a
    /// policy file made only on another branch is not policy.
    async fn rules_in(&self, commit: &ObjectId, revisions: &[Revision]) -> Result<Arc<Rules>> {
        let on_default = |at: ObjectId| async move {
            match self.default_head().await? {
                Some(tip) => self.meta.is_ancestor(&self.repo, &at, &tip).await,
                None => Ok(false),
            }
        };
        match revisions.iter().find(|r| r.path.as_str() == POLICY_PATH) {
            Some(policy) if on_default(policy.commit).await? => {
                match self.parse_policy(&policy.content).await {
                    Ok(rules) => Ok(Arc::new(rules)),
                    Err(_) => Ok(self.fallback.clone()),
                }
            }
            Some(_) => Ok(self.rules()),
            None if on_default(*commit).await? => Ok(self.fallback.clone()),
            None => Ok(self.rules()),
        }
    }

    /// Every path as of `commit` (the head of the default branch when `None`). A path's mode is the one
    /// `.pyn/pyn.toml` gives it at that commit, and its revision is the highest-numbered one made in the commit or an
    /// ancestor.
    pub async fn snapshot(&self, commit: Option<&ObjectId>) -> Result<Snapshot> {
        let id = match commit {
            Some(id) => {
                self.get_commit(id).await?;
                Some(*id)
            }
            None => self.default_head().await?,
        };
        self.snapshot_at(id).await
    }

    /// Like `snapshot`, as of the head of `branch`.
    pub async fn snapshot_on(&self, branch: &str) -> Result<Snapshot> {
        let head = self.head_of(branch).await?;
        self.snapshot_at(head).await
    }

    async fn snapshot_at(&self, id: Option<ObjectId>) -> Result<Snapshot> {
        let Some(id) = id else {
            return Ok(Snapshot {
                commit: None,
                entries: Vec::new(),
            });
        };
        let revisions = self.meta.snapshot(&self.repo, &id).await?;
        revisions.iter().try_for_each(Revision::verify)?;
        let rules = self.rules_in(&id, &revisions).await?;
        let entries = revisions
            .into_iter()
            .map(|revision| SnapshotEntry {
                path: revision.path.clone(),
                mode: rules.mode_for(&revision.path),
                revision,
            })
            .collect();
        Ok(Snapshot {
            commit: Some(id),
            entries,
        })
    }
}

/// The shortest commit abbreviation the server resolves.
pub const MIN_COMMIT_PREFIX: usize = 4;
