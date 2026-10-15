//! Receiving client-built commits (decision #300): everything is verified here, then one store primitive applies the
//! push or refuses it whole.

use std::collections::{HashMap, HashSet};

use pyn_object::{FileMode, Kind, ObjectId, TreeEntry, hash_object, parse_tree};

use super::{RepoService, changeset_id};
use crate::access::{Permission, Principal};
use crate::audit::AuditAction;
use crate::commit_record::TreeChange;
use crate::error::{PynError, Result};
use crate::object::REPO_ALGORITHM;
use crate::rules::{Mode, POLICY_PATH};
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
        let rules = self.rules_on(branch).await?;
        let mut commits = Vec::with_capacity(objects.commits.len());
        let mut previous: Option<Commit> = None;
        for (id, raw) in objects.commits {
            let record = self.read_pushed_commit(id, raw)?;
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
            let items = self
                .revisions_of(&record, &diff.changes, &rules, who)
                .await?;
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
            });
        }
        let now = self.clock.now();
        let cap = self.storage_cap().await?;
        let revs = self
            .meta
            .push_commits_on(&self.repo, branch, commits, &who.user, now, cap)
            .await?;
        for rev in &revs {
            self.record(
                &who.user,
                AuditAction::Checkin,
                &rev.path,
                format!("r{}: {} (push)", rev.id, rev.message),
            )
            .await?;
        }
        Ok(revs)
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
        if parsed.parents.len() > 1 {
            return Err(unsupported(format!(
                "commit {id} is a merge commit; push merges are not supported yet"
            )));
        }
        if parsed.author.time != parsed.committer.time {
            return Err(unsupported(format!(
                "commit {id} has different author and committer times; a commit keeps one time"
            )));
        }
        let record = Commit::from_raw(REPO_ALGORITHM, raw, None).map_err(|_| {
            unsupported(format!(
                "commit {id} has a name or message that is not UTF-8"
            ))
        })?;
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

    /// The files `new` adds or changes against `parent`; a removal, a file turned folder or back, or a submodule is
    /// refused.
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
                let name =
                    std::str::from_utf8(&e.name).map_err(|_| unsupported("a path is not UTF-8"))?;
                let path = if dir.is_empty() {
                    name.to_string()
                } else {
                    format!("{dir}/{name}")
                };
                let prior = old_by_name.get(e.name.as_slice());
                match prior {
                    Some(o) if o.id == e.id && o.mode == e.mode => continue,
                    Some(o) if o.mode.is_tree() != e.mode.is_tree() => {
                        return Err(unsupported(format!(
                            "{path} replaces a file with a folder or back, which deletes paths; deletions are not supported yet"
                        )));
                    }
                    _ => {}
                }
                if e.mode.is_tree() {
                    stack.push((path, prior.map(|o| o.id), e.id));
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
            if let Some(gone) = old.iter().find(|o| !kept.contains(o.name.as_slice())) {
                return Err(unsupported(format!(
                    "{} is removed; deletions are not supported yet",
                    String::from_utf8_lossy(&gone.name)
                )));
            }
        }
        changes.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(TreeDiff { changes, trees })
    }

    /// One revision per changed path, after checking the policy file is untouched, the blobs are stored and the
    /// mode is one a file can have.
    async fn revisions_of(
        &self,
        record: &Commit,
        changes: &[TreeChange],
        rules: &crate::rules::Rules,
        who: &Principal,
    ) -> Result<Vec<NewRevision>> {
        if changes.is_empty() {
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
        let mut items = Vec::with_capacity(changes.len());
        for c in changes {
            if c.path.as_str() == POLICY_PATH {
                return Err(unsupported(format!(
                    "commit {} changes {POLICY_PATH}; policy changes are not supported in a push yet",
                    record.id
                )));
            }
            super::check_file_mode(c.mode)?;
            let size = self
                .objects
                .size(&c.id)
                .await?
                .ok_or_else(|| PynError::ObjectMissing(c.id.to_string()))?;
            let mode: Mode = rules.mode_for(&c.path);
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
                deleted: false,
                size,
                changeset: None,
            });
        }
        Ok(items)
    }
}
