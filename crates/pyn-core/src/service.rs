use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use chrono::Duration;

use crate::access::{Permission, Principal};
use crate::audit::{AuditAction, AuditEvent, AuditQuery, AuditScope, AuditStore, NewAuditEvent};
use crate::clock::Clock;
use crate::error::{PynError, Result};
use crate::history::{HISTORY_DEFAULT_LIMIT, HISTORY_MAX_LIMIT, HistoryCursor, HistoryPage};
use crate::integrity::{Problem, VerifyReport, check_events, verify_audit, verify_revisions};
use crate::limits::{StorageCap, StorageCaps};
use crate::object::ObjectStore;
use crate::repo::DEFAULT_MAX_LOCKS_PER_USER;
use crate::rules::{Mode, POLICY_PATH, PathFilter, Rules};
use crate::store::MetadataStore;
use crate::tree::{ACTIVITY_ACTIONS, DEFAULT_BRANCH, EntryKind, RepoSummary, TreeEntry};
use crate::types::{
    ChangesetItem, Commit, ContentHash, Lock, NewRevision, RepoId, RepoPath, Revision, RevisionId,
    UserId,
};

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
    pub lock: Option<Lock>,
}

/// One path of a changeset as the caller names it: the uploaded content and the revision it was based on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeEntry {
    pub path: RepoPath,
    pub content: ContentHash,
    pub base: Option<RevisionId>,
}

/// One path of a snapshot: the revision the commit points at, with the mode it had then.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotEntry {
    pub path: RepoPath,
    pub mode: Mode,
    pub revision: Revision,
}

/// The whole repository as of one commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// The commit number; 0 for a repository with no commits.
    pub commit: u64,
    /// Ordered by path.
    pub entries: Vec<SnapshotEntry>,
}

/// One page of commits, newest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitPage {
    pub commits: Vec<Commit>,
    /// Pass back as `before` for the next, older page; `None` on the last page.
    pub next: Option<u64>,
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
        Self {
            repo,
            applied: RwLock::new(Applied {
                revision: None,
                rules: Arc::new(rules),
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
        if let Some(head) = self.meta.head_revision(&self.repo, &policy).await? {
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

    async fn parse_policy(&self, content: &ContentHash) -> Result<Rules> {
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
        actor: &UserId,
        rev: &Revision,
        old: &Rules,
        new: &Rules,
    ) -> Result<()> {
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
            let detail = match self.convert_to_exclusive(actor, path, rev, new).await? {
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

    /// Records the path's current content as the next revision under the new mode; no lock is needed.
    /// Retries when a concurrent checkin moves the head; `None` if the path has no revision.
    async fn convert_to_exclusive(
        &self,
        actor: &UserId,
        path: &RepoPath,
        policy: &Revision,
        new: &Rules,
    ) -> Result<Option<Revision>> {
        for _ in 0..3 {
            let Some(head) = self.head(path).await? else {
                return Ok(None);
            };
            let size = self
                .objects
                .size(&head.content)
                .await?
                .ok_or_else(|| PynError::ObjectMissing(head.content.to_string()))?;
            let revision = NewRevision {
                path: path.clone(),
                content: head.content,
                author: actor.clone(),
                message: format!("Convert to exclusive: policy r{}", policy.id),
                created_at: self.clock.now(),
                restored_from: None,
                mode: new.mode_for(path),
                size,
                changeset: None,
            };
            match self
                .meta
                .commit_revision_capped(
                    &self.repo,
                    revision,
                    Some(head.id),
                    None,
                    self.clock.now(),
                    None,
                )
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

    /// Paths whose resolved mode differs between `old` and `new`, and the live locks on those becoming shared.
    async fn conversions(&self, old: &Rules, new: &Rules) -> Result<PolicyPreview> {
        let mut preview = PolicyPreview::default();
        let mut paths: BTreeMap<RepoPath, ()> = BTreeMap::new();
        for rev in self.meta.list_head_revisions(&self.repo).await? {
            paths.insert(rev.path, ());
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
    async fn check_policy_edit(&self, who: &Principal, content: &ContentHash) -> Result<Rules> {
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

    /// Appends the revision under the rules in force; a policy revision is validated first and applied once recorded.
    /// The first policy revision is the owner's bootstrap: no lock, and the mode it declares for itself applies.
    async fn commit(
        &self,
        who: &Principal,
        path: &RepoPath,
        content: ContentHash,
        base: Option<RevisionId>,
        message: String,
        restored_from: Option<RevisionId>,
    ) -> Result<Revision> {
        let policy = path.as_str() == POLICY_PATH;
        let parsed = if policy {
            Some(self.check_policy_edit(who, &content).await?)
        } else {
            None
        };
        let bootstrap = policy && base.is_none();
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
            author: who.user.clone(),
            message,
            created_at: now,
            restored_from,
            mode,
            size,
            changeset: None,
        };
        let rev = self
            .meta
            .commit_revision_capped(&self.repo, revision, base, lock_holder, now, cap)
            .await?;
        if let Some(new) = parsed
            && let Some(old) = self.apply(rev.id, new.clone())
        {
            self.policy_applied(&who.user, &rev, &old, &new).await?;
        }
        Ok(rev)
    }

    /// Record a revision of `content` (already in the object store). Exclusive paths need the caller's
    /// live lock; every path needs `base` == head. `.pyn/pyn.toml` also needs the policy permission.
    pub async fn checkin(
        &self,
        who: &Principal,
        path: &RepoPath,
        content: ContentHash,
        base: Option<RevisionId>,
        message: String,
    ) -> Result<Revision> {
        if !self.objects.exists(&content).await? {
            return Err(PynError::ObjectMissing(content.to_string()));
        }
        let rev = self.commit(who, path, content, base, message, None).await?;
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
                    author: who.user.clone(),
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
            .commit_changeset_capped(&self.repo, items, now, cap)
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
            .commit(who, path, old.content, Some(base), message, Some(source))
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

    /// Head revision and live lock of every path that has either, ordered by path.
    async fn live_paths(&self) -> Result<BTreeMap<RepoPath, (Option<Revision>, Option<Lock>)>> {
        let mut paths: BTreeMap<RepoPath, (Option<Revision>, Option<Lock>)> = BTreeMap::new();
        for rev in self.meta.list_head_revisions(&self.repo).await? {
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
        let paths = self.live_paths().await?;
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
        let paths = self.live_paths().await?;
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
            activity.extend(check_events(&scope, self.audit.list(&scope, &query).await?)?);
        }
        activity.sort_by_key(|e| std::cmp::Reverse(e.id));
        activity.truncate(activity_limit);
        Ok(RepoSummary {
            default_branch: DEFAULT_BRANCH.to_string(),
            branch_count: 1,
            files,
            exclusive_files,
            shared_files: files - exclusive_files,
            updated_at,
            locks: paths.into_values().filter_map(|(_, lock)| lock).collect(),
            activity,
        })
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
        rev.verify()?;
        let bytes = self
            .objects
            .get(&rev.content)
            .await?
            .ok_or_else(|| PynError::ObjectMissing(rev.content.to_string()))?;
        Ok((rev, bytes))
    }

    /// Like `read`, but streams the content, or the byte `range` of it.
    pub async fn read_stream(
        &self,
        path: &RepoPath,
        revision: Option<RevisionId>,
        range: Option<std::ops::Range<u64>>,
    ) -> Result<(Revision, crate::ObjectRead)> {
        let found = match revision {
            Some(id) => self.meta.get_revision(&self.repo, path, id).await?,
            None => self.meta.head_revision(&self.repo, path).await?,
        };
        let rev = found.ok_or_else(|| PynError::RevisionNotFound {
            path: path.clone(),
            revision: revision.map_or("(head)".to_string(), |r| r.to_string()),
        })?;
        rev.verify()?;
        let read = self
            .objects
            .read(&rev.content, range)
            .await?
            .ok_or_else(|| PynError::ObjectMissing(rev.content.to_string()))?;
        Ok((rev, read))
    }

    pub async fn head(&self, path: &RepoPath) -> Result<Option<Revision>> {
        let head = self.meta.head_revision(&self.repo, path).await?;
        head.iter().try_for_each(Revision::verify)?;
        Ok(head)
    }

    /// A path's revisions, oldest first, after checking each hash and the links between them.
    pub async fn history(&self, path: &RepoPath) -> Result<Vec<Revision>> {
        let revisions = self.meta.history(&self.repo, path).await?;
        if let Some((id, kind)) = verify_revisions(&revisions).first() {
            return Err(PynError::HistoryCorrupt(format!(
                "{path}@r{id}: {}",
                kind.as_str()
            )));
        }
        Ok(revisions)
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
            let revisions = self.meta.history(&self.repo, &p).await?;
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
                });
            }
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
                });
            }
        }
        Ok(report)
    }

    /// Repository-wide history, newest first; `limit` defaults to 50 and is capped at 200.
    pub async fn repo_history(
        &self,
        filter: Option<&PathFilter>,
        before: Option<&HistoryCursor>,
        limit: Option<usize>,
    ) -> Result<HistoryPage> {
        let limit = limit
            .unwrap_or(HISTORY_DEFAULT_LIMIT)
            .clamp(1, HISTORY_MAX_LIMIT);
        let mut revisions = self
            .meta
            .repo_history(&self.repo, filter, before, limit + 1)
            .await?;
        revisions.iter().try_for_each(Revision::verify)?;
        let next = (revisions.len() > limit).then(|| {
            revisions.truncate(limit);
            HistoryCursor::of(&revisions[limit - 1])
        });
        Ok(HistoryPage { revisions, next })
    }

    pub async fn head_commit(&self) -> Result<u64> {
        self.meta.head_commit(&self.repo).await
    }

    pub async fn get_commit(&self, id: u64) -> Result<Commit> {
        let commit = self
            .meta
            .get_commit(&self.repo, id)
            .await?
            .ok_or(PynError::CommitNotFound(id))?;
        commit.revisions.iter().try_for_each(Revision::verify)?;
        Ok(commit)
    }

    /// Commits newest first; `limit` defaults to 50 and is capped at 200.
    pub async fn commits(&self, before: Option<u64>, limit: Option<usize>) -> Result<CommitPage> {
        let limit = limit
            .unwrap_or(HISTORY_DEFAULT_LIMIT)
            .clamp(1, HISTORY_MAX_LIMIT);
        let mut commits = self.meta.commits(&self.repo, before, limit + 1).await?;
        for c in &commits {
            c.revisions.iter().try_for_each(Revision::verify)?;
        }
        let next = (commits.len() > limit).then(|| {
            commits.truncate(limit);
            commits[limit - 1].id
        });
        Ok(CommitPage { commits, next })
    }

    /// Every path as of commit `id` (the newest when `None`); a path's mode is the one recorded with its revision.
    pub async fn snapshot(&self, id: Option<u64>) -> Result<Snapshot> {
        let id = match id {
            Some(id) => {
                self.get_commit(id).await?;
                id
            }
            None => self.head_commit().await?,
        };
        let mut entries = Vec::new();
        for revision in self.meta.snapshot(&self.repo, id).await? {
            revision.verify()?;
            let mode = revision
                .mode
                .unwrap_or_else(|| self.mode_for(&revision.path));
            entries.push(SnapshotEntry {
                path: revision.path.clone(),
                mode,
                revision,
            });
        }
        Ok(Snapshot {
            commit: id,
            entries,
        })
    }
}
