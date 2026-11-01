//! Merge commits in a push: each path's pointer is judged as the server's own merge judges it (docs/concepts/merging.md),
//! with the merged content taken from the pushed tree.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use pyn_object::{FileMode, ObjectId};

use super::RepoService;
use crate::access::Principal;
use crate::commit_record::TreeChange;
use crate::error::{PynError, Result};
use crate::rules::{Mode, Rules};
use crate::types::{Commit, NewRevision, RepoPath, Revision};

type Version = Option<(ObjectId, FileMode)>;

/// The branch's pointer for a path as the pushed commits before the merge leave it.
#[derive(Clone)]
enum Pointer {
    Existing(Box<Revision>),
    /// A revision this push creates; `roots` are the existing revisions it follows.
    New {
        version: Version,
        roots: Vec<Revision>,
    },
}

impl Pointer {
    fn version(&self) -> Version {
        match self {
            Self::Existing(r) => version_of(r),
            Self::New { version, .. } => *version,
        }
    }
}

fn version_of(r: &Revision) -> Version {
    (!r.deleted).then_some((r.content, r.file_mode))
}

/// What the pushed commits so far leave of the branch's pointers, and the commits their history contains.
pub(super) struct Chain {
    start: Option<ObjectId>,
    tips: Vec<ObjectId>,
    ours: HashMap<RepoPath, Pointer>,
}

/// A merge commit's accounting: the paths that get a new revision, and the existing revisions it points at.
pub(super) struct MergeOutcome {
    pub news: Vec<TreeChange>,
    pub adopted: Vec<Revision>,
}

enum Verdict {
    Keep,
    Adopt,
    Conflict,
}

impl Chain {
    /// `head` is the branch before the push; its history predates the push's policy commits.
    pub(super) fn new(start: Option<ObjectId>, head: Option<ObjectId>) -> Self {
        let mut tips: Vec<ObjectId> = start.into_iter().collect();
        tips.extend(head.filter(|h| Some(*h) != start));
        Self {
            start,
            tips,
            ours: HashMap::new(),
        }
    }
}

impl RepoService {
    async fn pointer_at_start(&self, chain: &Chain, path: &RepoPath) -> Result<Option<Pointer>> {
        if let Some(p) = chain.ours.get(path) {
            return Ok(Some(p.clone()));
        }
        Ok(match &chain.start {
            Some(start) => self
                .meta
                .revision_at(&self.repo, start, path)
                .await?
                .map(|r| Pointer::Existing(Box::new(r))),
            None => None,
        })
    }

    async fn roots_of(&self, chain: &Chain, path: &RepoPath) -> Result<Vec<Revision>> {
        Ok(match self.pointer_at_start(chain, path).await? {
            Some(Pointer::Existing(r)) => vec![*r],
            Some(Pointer::New { roots, .. }) => roots,
            None => Vec::new(),
        })
    }

    /// Records the revisions an ordinary pushed commit makes.
    pub(super) async fn note_pushed(&self, chain: &mut Chain, items: &[NewRevision]) -> Result<()> {
        for item in items {
            let roots = self.roots_of(chain, &item.path).await?;
            let version = (!item.deleted).then_some((item.content, item.file_mode));
            chain
                .ours
                .insert(item.path.clone(), Pointer::New { version, roots });
        }
        Ok(())
    }

    async fn snapshot_of(&self, commit: &ObjectId) -> Result<BTreeMap<RepoPath, Revision>> {
        let revisions = self.meta.snapshot(&self.repo, commit).await?;
        revisions.iter().try_for_each(Revision::verify)?;
        Ok(revisions.into_iter().map(|r| (r.path.clone(), r)).collect())
    }

    /// Whether the pusher holds a live lock on `path`.
    async fn holds_lock(&self, who: &Principal, path: &RepoPath) -> Result<bool> {
        let lock = self
            .meta
            .get_lock(&self.repo, path, self.clock.now())
            .await?;
        Ok(lock.is_some_and(|l| l.owner == who.user))
    }

    async fn follows(&self, ours: &Pointer, theirs: &Revision) -> Result<bool> {
        match ours {
            Pointer::Existing(o) => self.descends(o, theirs).await,
            Pointer::New { roots, .. } => {
                for r in roots {
                    if r.id == theirs.id || self.descends(r, theirs).await? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
        }
    }

    async fn pushed_exclusive_verdict(
        &self,
        chain: &Chain,
        ours: &Pointer,
        theirs: &Revision,
        second: &ObjectId,
    ) -> Result<Verdict> {
        let same = ours.version() == version_of(theirs);
        if !same
            && let Pointer::Existing(o) = ours
            && self.is_diverged(o, second).await?
        {
            return Ok(Verdict::Conflict);
        }
        if !same && theirs.mode == Some(Mode::Shared) {
            let mut known = false;
            for tip in &chain.tips {
                known |= self
                    .meta
                    .is_ancestor(&self.repo, &theirs.commit, tip)
                    .await?;
            }
            if !known {
                return Ok(Verdict::Conflict);
            }
        }
        if same {
            return Ok(match ours {
                Pointer::Existing(o) if o.id <= theirs.id => Verdict::Adopt,
                _ => Verdict::Keep,
            });
        }
        if self.follows(ours, theirs).await? {
            return Ok(Verdict::Keep);
        }
        if let Pointer::Existing(o) = ours
            && self.descends(theirs, o).await?
        {
            return Ok(Verdict::Adopt);
        }
        Ok(Verdict::Conflict)
    }

    /// Which paths of merge commit `record` get a new revision and which point at an existing one; a diverged
    /// exclusive path without a resolution is `MergeConflict`.
    pub(super) async fn account_merge(
        &self,
        who: &Principal,
        chain: &mut Chain,
        record: &Commit,
        changes: &[TreeChange],
        rules: &Rules,
    ) -> Result<MergeOutcome> {
        let second = record.parents[1];
        let Some(start) = chain.start else {
            return Err(PynError::PushInvalid(format!(
                "merge commit {} has no first parent",
                record.id
            )));
        };
        if self.meta.get_commit(&self.repo, &second).await?.is_none() {
            return Err(PynError::PushInvalid(format!(
                "commit {} merges {second}, which is not in the repository",
                record.id
            )));
        }
        let (snap_ours, snap_theirs) = (
            self.snapshot_of(&start).await?,
            self.snapshot_of(&second).await?,
        );
        let diff: HashMap<&RepoPath, &TreeChange> = changes.iter().map(|c| (&c.path, c)).collect();
        let paths: BTreeSet<&RepoPath> = snap_ours
            .keys()
            .chain(snap_theirs.keys())
            .chain(chain.ours.keys())
            .chain(diff.keys().copied())
            .collect();

        let mut news = Vec::new();
        let mut adopted = Vec::new();
        let mut conflicts = Vec::new();
        let mut update = Vec::new();
        for path in paths {
            let ours = match chain.ours.get(path) {
                Some(p) => Some(p.clone()),
                None => match snap_ours.get(path) {
                    Some(r) => Some(Pointer::Existing(Box::new(r.clone()))),
                    None => self
                        .meta
                        .revision_at(&self.repo, &start, path)
                        .await?
                        .map(|r| Pointer::Existing(Box::new(r))),
                },
            };
            let theirs = match snap_theirs.get(path) {
                Some(r) => Some(r.clone()),
                None => self.meta.revision_at(&self.repo, &second, path).await?,
            };
            let change = diff.get(path).copied();
            if change.is_none()
                && let (Some(Pointer::Existing(o)), Some(t)) = (&ours, &theirs)
                && o.id == t.id
            {
                continue;
            }
            let ours_version = ours.as_ref().and_then(Pointer::version);
            let merged: Version = match change {
                Some(c) if c.removed => None,
                Some(c) => Some((c.id, c.mode)),
                None => ours_version,
            };
            let mode = rules.mode_for(path);
            let (verdict, expected) = match (&ours, &theirs) {
                (None, None) => (Verdict::Keep, None),
                (Some(o), None) => (Verdict::Keep, o.version()),
                (None, Some(t)) => (Verdict::Adopt, version_of(t)),
                (Some(o), Some(t)) => match (mode, o) {
                    (Mode::Exclusive, o) => {
                        let v = self.pushed_exclusive_verdict(chain, o, t, &second).await?;
                        let expected = match v {
                            Verdict::Adopt => version_of(t),
                            _ => o.version(),
                        };
                        (v, expected)
                    }
                    (Mode::Shared, Pointer::Existing(o)) if o.id < t.id => {
                        (Verdict::Adopt, version_of(t))
                    }
                    (Mode::Shared, o) => (Verdict::Keep, o.version()),
                },
            };
            let is_new = match verdict {
                Verdict::Conflict => {
                    let resolved = merged != ours_version || self.holds_lock(who, path).await?;
                    if !resolved {
                        conflicts.push(path.clone());
                        continue;
                    }
                    true
                }
                _ => merged != expected,
            };
            if is_new {
                let removed = merged.is_none();
                let (id, file_mode) = match merged {
                    Some(v) => v,
                    None => match ours_version.or_else(|| theirs.as_ref().and_then(version_of)) {
                        Some(v) => v,
                        None => continue,
                    },
                };
                news.push(TreeChange {
                    path: path.clone(),
                    mode: file_mode,
                    id,
                    removed,
                });
                let mut roots = self.roots_of(chain, path).await?;
                roots.extend(theirs.clone());
                update.push((
                    path.clone(),
                    Pointer::New {
                        version: merged,
                        roots,
                    },
                ));
            } else if matches!(verdict, Verdict::Adopt)
                && let Some(t) = theirs
            {
                update.push((path.clone(), Pointer::Existing(Box::new(t.clone()))));
                adopted.push(t);
            }
        }
        if !conflicts.is_empty() {
            return Err(PynError::MergeConflict(conflicts));
        }
        chain.ours.extend(update);
        chain.tips.push(second);
        Ok(MergeOutcome { news, adopted })
    }
}
