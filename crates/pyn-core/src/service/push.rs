//! Receiving client-built commits (decision #300): everything is verified here, then one store primitive applies the
//! push or refuses it whole.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use pyn_object::{FileMode, Kind, ObjectId, TreeEntry, hash_object, parse_tree};

use super::push_merge::Chain;
use super::{DEFAULT_BRANCH, RepoService, changeset_id};
use crate::access::{Permission, Principal};
use crate::audit::AuditAction;
use crate::commit_record::TreeChange;
use crate::error::{PynError, Result};
use crate::object::REPO_ALGORITHM;
use crate::rules::{Mode, POLICY_PATH, Rules};
use crate::stranded::StrandedVersion;
use crate::types::{Commit, NewRevision, PushedCommit, RepoPath, Revision};

/// Raw git objects a push carries: commits oldest first, and the trees they need that the server may not have.
#[derive(Debug, Default, Clone)]
pub struct PushObjects {
    pub commits: Vec<(ObjectId, Vec<u8>)>,
    pub trees: Vec<(ObjectId, Vec<u8>)>,
}

fn unsupported(what: impl Into<String>) -> PynError {
    PynError::PushUnsupported(what.into())
}

fn invalid(what: impl Into<String>) -> PynError {
    PynError::PushInvalid(what.into())
}

/// The revisions a push made and the shared versions its policy commits stranded.
#[derive(Debug, Clone)]
pub struct Pushed {
    pub revisions: Vec<Revision>,
    pub stranded: Vec<StrandedVersion>,
}

/// What one commit changes against its parent tree, and the trees of it that were pushed.
struct TreeDiff {
    changes: Vec<TreeChange>,
    trees: Vec<(ObjectId, Vec<u8>)>,
}

impl RepoService {
    /// Appends the pushed commits to `branch`, keeping their ids, or refuses the whole push. The author is not matched
    /// to the pusher; the revisions are the pusher's.
    pub async fn push_commits(
        &self,
        who: &Principal,
        branch: &str,
        objects: PushObjects,
    ) -> Result<Vec<Revision>> {
        Ok(self
            .push_commits_recorded(who, branch, objects)
            .await?
            .revisions)
    }

    /// Like `push_commits`, with the stranded versions of the policy commits the push carried. On the default branch a
    /// commit that changes the policy file is validated like a direct policy commit, and refused whole if it is invalid;
    /// its rules apply to the commits after it and to the repository once the push is recorded.
    pub async fn push_commits_recorded(
        &self,
        who: &Principal,
        branch: &str,
        objects: PushObjects,
    ) -> Result<Pushed> {
        who.require(Permission::Checkin)?;
        if objects.commits.is_empty() {
            return Err(PynError::InvalidRequest(
                "a push needs at least one commit".into(),
            ));
        }
        let mut pushed_trees = HashMap::new();
        for (id, body) in objects.trees {
            let actual = hash_object(REPO_ALGORITHM, Kind::Tree, &body)
                .map_err(|e| invalid(format!("tree {id}: {e}")))?;
            if actual != id {
                return Err(invalid(format!("tree {id} does not hash to its id")));
            }
            pushed_trees.insert(id, body);
        }
        let mut rules = self.rules_on(branch).await?;
        let on_default = branch == DEFAULT_BRANCH;
        let policy_path = RepoPath::new(POLICY_PATH)?;
        let mut has_policy = on_default && self.pointer_on(branch, &policy_path).await?.is_some();
        let mut rules_id = match (on_default, self.head_of(branch).await?) {
            (false, Some(head)) => self.policy_revision_at(&head).await?.map(|r| r.id),
            _ => None,
        };
        let mut records = Vec::with_capacity(objects.commits.len());
        for (id, raw) in objects.commits {
            records.push(self.read_pushed_commit(id, raw)?);
        }
        let mut chain = records
            .iter()
            .any(|r| r.parents.len() > 1)
            .then(|| Chain::new(records[0].parents.first().copied()));
        let mut policies = Vec::new();
        let mut merges = Vec::new();
        let mut arrivals = Vec::new();
        let mut commits = Vec::with_capacity(records.len());
        let mut previous: Option<Commit> = None;
        for record in records {
            let parent_tree = match (&previous, record.parents.first()) {
                (Some(prev), Some(parent)) if parent == &prev.id => Some(prev.tree),
                (Some(_), _) => {
                    return Err(invalid(format!(
                        "commit {} must be a child of the commit before it",
                        record.id
                    )));
                }
                (None, Some(parent)) => Some(
                    self.meta
                        .get_commit(&self.repo, parent)
                        .await?
                        .ok_or_else(|| PynError::CommitNotFound(parent.to_string()))?
                        .tree,
                ),
                (None, None) => None,
            };
            let diff = self
                .diff_pushed(parent_tree, record.tree, &pushed_trees)
                .await?;
            let mut adopted = Vec::new();
            let mut arriving = None;
            let mut changes = diff.changes;
            if let (true, Some(chain)) = (record.parents.len() > 1, chain.as_mut()) {
                if !on_default
                    && let Some(theirs) = self.policy_revision_at(&record.parents[1]).await?
                    && Some(theirs.id) > rules_id
                {
                    rules_id = Some(theirs.id);
                    rules = self.rules_of(&theirs).await?;
                }
                let out = self
                    .account_merge(who, chain, &record, &changes, &rules)
                    .await?;
                changes = out.news;
                adopted = out.adopted;
                merges.push((record.id, record.parents[1]));
                if on_default && let Some(rev) = adopted.iter().find(|r| r.path == policy_path) {
                    let parsed = self.check_policy_edit(who, &rev.content).await?;
                    arrivals.push((record.id, rev.clone(), parsed.clone()));
                    arriving = Some(parsed);
                }
            }
            let (items, policy) = self
                .revisions_of(
                    &record,
                    &changes,
                    &rules,
                    who,
                    on_default,
                    has_policy,
                    record.parents.len() > 1,
                )
                .await?;
            if record.parents.len() < 2
                && let Some(chain) = chain.as_mut()
            {
                self.note_pushed(chain, &items).await?;
            }
            if let Some(rules_in) = arriving {
                has_policy = true;
                rules = Arc::new(rules_in);
            }
            let mut unlocked = Vec::new();
            if let Some(new) = policy {
                if !has_policy {
                    unlocked.push(policy_path.clone());
                }
                has_policy = true;
                policies.push(new.clone());
                rules = Arc::new(new);
            }
            let mut record = record;
            if items.len() > 1 {
                record.changeset = Some(changeset_id()?);
            }
            let items = items
                .into_iter()
                .map(|mut i| {
                    i.changeset = record.changeset.clone();
                    i
                })
                .collect();
            previous = Some(record.clone());
            commits.push(PushedCommit {
                record,
                trees: diff.trees,
                items,
                unlocked,
                adopted,
            });
        }
        let now = self.clock.now();
        let cap = self.storage_cap().await?;
        let revs = self
            .meta
            .push_commits_on(&self.repo, branch, commits, &who.user, now, cap)
            .await?;
        let mut applied = policies.into_iter();
        let mut stranded = Vec::new();
        for rev in &revs {
            let action = if rev.deleted {
                AuditAction::Delete
            } else {
                AuditAction::Checkin
            };
            self.record(
                &who.user,
                action,
                &rev.path,
                format!("r{}: {} (push)", rev.id, rev.message),
            )
            .await?;
            if on_default
                && rev.path.as_str() == POLICY_PATH
                && let Some(new) = applied.next()
                && let Some(old) = self.apply(rev.id, new.clone())
            {
                stranded.extend(self.policy_applied(who, rev, &old, &new).await?);
            }
        }
        for (commit, other) in &merges {
            self.record_repo(
                &who.user,
                AuditAction::BranchMerged,
                format!("{other} into {branch} at {commit} (push)"),
            )
            .await?;
        }
        for (_, rev, new) in arrivals {
            if let Some(old) = self.apply(rev.id, new.clone()) {
                stranded.extend(self.policy_applied(who, &rev, &old, &new).await?);
            }
        }
        Ok(Pushed {
            revisions: revs,
            stranded,
        })
    }

    /// The record the raw bytes describe, or why the push cannot carry it.
    fn read_pushed_commit(&self, id: ObjectId, raw: Vec<u8>) -> Result<Commit> {
        let actual = hash_object(REPO_ALGORITHM, Kind::Commit, &raw)
            .map_err(|e| invalid(format!("commit {id}: {e}")))?;
        if actual != id {
            return Err(invalid(format!("commit {id} does not hash to its id")));
        }
        let parsed = pyn_object::Commit::parse(REPO_ALGORITHM, &raw)
            .map_err(|e| invalid(format!("commit {id} is not a git commit: {e}")))?;
        if parsed.parents.len() > 2
            || parsed.parents.len() == 2 && parsed.parents[0] == parsed.parents[1]
        {
            return Err(unsupported(format!(
                "commit {id} has {} parents; a merge commit has two different ones",
                parsed.parents.len()
            )));
        }
        let record = Commit::from_raw(REPO_ALGORITHM, raw, None)
            .map_err(|e| invalid(format!("commit {id}: {e}")))?;
        record.verify().map_err(|_| {
            unsupported(format!(
                "commit {id} cannot be recorded as sent; its message must end with a newline"
            ))
        })?;
        Ok(record)
    }

    /// A tree's entries, from the push first, then the server, and whether it came from the push.
    async fn load_tree(
        &self,
        id: &ObjectId,
        pushed: &HashMap<ObjectId, Vec<u8>>,
    ) -> Result<(Vec<TreeEntry>, bool)> {
        let (body, from_push) = match pushed.get(id) {
            Some(body) => (body.clone(), true),
            None => match self.meta.get_tree(&self.repo, id).await? {
                Some(body) => (body, false),
                None => {
                    return Err(invalid(format!(
                        "tree {id} is neither in the push nor on the server"
                    )));
                }
            },
        };
        let entries = parse_tree(REPO_ALGORITHM, &body)
            .map_err(|e| invalid(format!("tree {id} is not a git tree: {e}")))?;
        Ok((entries, from_push))
    }

    /// The files `new` adds, changes or removes against `parent`; a submodule is refused.
    async fn diff_pushed(
        &self,
        parent: Option<ObjectId>,
        new: ObjectId,
        pushed: &HashMap<ObjectId, Vec<u8>>,
    ) -> Result<TreeDiff> {
        let mut changes = Vec::new();
        let mut trees = Vec::new();
        let mut seen = HashSet::new();
        let mut stack = vec![(String::new(), parent, new)];
        while let Some((dir, old_id, new_id)) = stack.pop() {
            let (entries, from_push) = self.load_tree(&new_id, pushed).await?;
            if from_push && seen.insert(new_id) {
                trees.push((new_id, pushed[&new_id].clone()));
            }
            let old = match &old_id {
                Some(id) => self.load_tree(id, pushed).await?.0,
                None => Vec::new(),
            };
            let old_by_name: HashMap<&[u8], &TreeEntry> =
                old.iter().map(|e| (e.name.as_slice(), e)).collect();
            for e in &entries {
                let path = join_path(&dir, &e.name)?;
                let prior = old_by_name.get(e.name.as_slice());
                if let Some(o) = prior {
                    if o.id == e.id && o.mode == e.mode {
                        continue;
                    }
                    if o.mode.is_tree() != e.mode.is_tree() {
                        self.removed_under(&path, o, pushed, &mut changes).await?;
                    }
                }
                let same_kind = prior.filter(|o| o.mode.is_tree() == e.mode.is_tree());
                if e.mode.is_tree() {
                    stack.push((path, same_kind.map(|o| o.id), e.id));
                } else if e.mode == FileMode::Gitlink {
                    return Err(unsupported(format!("{path} is a submodule")));
                } else {
                    changes.push(TreeChange {
                        path: RepoPath::new(path)?,
                        mode: e.mode,
                        id: e.id,
                        removed: false,
                    });
                }
            }
            let kept: HashSet<&[u8]> = entries.iter().map(|e| e.name.as_slice()).collect();
            for gone in old.iter().filter(|o| !kept.contains(o.name.as_slice())) {
                let path = join_path(&dir, &gone.name)?;
                self.removed_under(&path, gone, pushed, &mut changes)
                    .await?;
            }
        }
        changes.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(TreeDiff { changes, trees })
    }

    /// Every file under (or at) the removed entry `at`, as a removal change.
    async fn removed_under(
        &self,
        at: &str,
        entry: &TreeEntry,
        pushed: &HashMap<ObjectId, Vec<u8>>,
        changes: &mut Vec<TreeChange>,
    ) -> Result<()> {
        let mut stack = vec![(at.to_string(), entry.mode, entry.id)];
        while let Some((path, mode, id)) = stack.pop() {
            if mode.is_tree() {
                for child in self.load_tree(&id, pushed).await?.0 {
                    stack.push((join_path(&path, &child.name)?, child.mode, child.id));
                }
            } else if mode == FileMode::Gitlink {
                return Err(unsupported(format!("{path} is a submodule")));
            } else {
                changes.push(TreeChange {
                    path: RepoPath::new(path)?,
                    mode,
                    id,
                    removed: true,
                });
            }
        }
        Ok(())
    }

    /// One revision per changed path, after checking the blobs are stored and the mode is one a file can have. On the
    /// default branch a changed policy file is validated and its parsed rules returned; removing it is refused.
    async fn revisions_of(
        &self,
        record: &Commit,
        changes: &[TreeChange],
        rules: &Rules,
        who: &Principal,
        on_default: bool,
        has_policy: bool,
        merge: bool,
    ) -> Result<(Vec<NewRevision>, Option<Rules>)> {
        if changes.is_empty() && !merge {
            return Err(unsupported(format!(
                "commit {} changes no file; empty commits are not supported yet",
                record.id
            )));
        }
        let message = record
            .message
            .strip_suffix('\n')
            .unwrap_or(&record.message)
            .to_string();
        let mut policy = None;
        let mut items = Vec::with_capacity(changes.len());
        for c in changes {
            let mut mode: Mode = rules.mode_for(&c.path);
            if on_default && c.path.as_str() == POLICY_PATH {
                if c.removed {
                    return Err(unsupported(format!(
                        "commit {} removes {POLICY_PATH}; the policy file cannot be deleted",
                        record.id
                    )));
                }
                who.require(Permission::EditPolicy)?;
                let new = self.parse_policy(&c.id).await?;
                if !has_policy {
                    mode = new.mode_for(&c.path);
                }
                policy = Some(new);
            }
            if !c.removed {
                super::check_file_mode(c.mode)?;
            }
            let size = self
                .objects
                .size(&c.id)
                .await?
                .ok_or_else(|| PynError::ObjectMissing(c.id.to_string()))?;
            items.push(NewRevision {
                path: c.path.clone(),
                content: c.id,
                file_mode: c.mode,
                author: who.user.clone(),
                person: record.author.clone(),
                message: message.clone(),
                created_at: record.created_at,
                restored_from: None,
                mode,
                deleted: c.removed,
                size,
                changeset: None,
            });
        }
        Ok((items, policy))
    }
}

fn join_path(dir: &str, name: &[u8]) -> Result<String> {
    let name = std::str::from_utf8(name).map_err(|_| unsupported("a path is not UTF-8"))?;
    Ok(if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    })
}
