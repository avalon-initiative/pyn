//! How far one branch is from another: commits ahead and behind, and the paths the head changes (docs/concepts/branches.md).

use std::collections::BTreeSet;

use pyn_object::ObjectId;

use super::RepoService;
use super::fetch::FETCH_MAX_EXAMINED;
use crate::error::Result;
use crate::types::{RepoPath, Revision};

/// Most commits each side of a comparison counts; past it the count stops here and `counts_capped` is set.
pub const COMPARE_MAX_COMMITS: usize = 1_000;
/// Most changed paths a comparison lists; past it `changes_truncated` is set.
pub const COMPARE_MAX_PATHS: usize = 1_000;
/// Most paths examined to find the changed ones; a longer candidate list also sets `changes_truncated`.
const COMPARE_MAX_CANDIDATES: usize = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathChange {
    pub path: RepoPath,
    pub kind: ChangeKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comparison {
    pub base: String,
    pub base_commit: ObjectId,
    pub head: String,
    pub head_commit: ObjectId,
    /// The first of the commits' lowest common ancestors (ordered by id); the changes are measured from it.
    pub merge_base: Option<ObjectId>,
    pub ahead: u32,
    pub behind: u32,
    pub counts_capped: bool,
    pub changes: Vec<PathChange>,
    pub changes_truncated: bool,
}

/// A changed path with its live revision on each side.
pub(super) struct Changed {
    pub path: RepoPath,
    pub kind: ChangeKind,
    pub before: Option<Revision>,
    pub now: Option<Revision>,
}

fn live(rev: Option<&Revision>) -> Option<&Revision> {
    rev.filter(|r| !r.deleted)
}

impl RepoService {
    /// Compares `head` with `base` (the default branch when `None`); each is a branch or a commit id or abbreviation.
    /// A history that makes a walk examine more than `FETCH_MAX_EXAMINED` commits is `invalid_request`.
    pub async fn compare(&self, base: Option<&str>, head: &str) -> Result<Comparison> {
        let (base_commit, base_name) = match base {
            Some(spec) => self.resolve_ref(spec).await?,
            None => self.resolve_ref(crate::branch::DEFAULT_BRANCH).await?,
        };
        let (head_commit, head_name) = self.resolve_ref(head).await?;
        let merge_base = self
            .meta
            .merge_bases(&self.repo, &base_commit, &head_commit)
            .await?
            .into_iter()
            .next();
        let ahead = self.commits_between(head_commit, base_commit).await?;
        let behind = self.commits_between(base_commit, head_commit).await?;
        let counts_capped = ahead.len() > COMPARE_MAX_COMMITS || behind.len() > COMPARE_MAX_COMMITS;
        let (changed, changes_truncated) = self
            .changed_paths(&ahead, &head_commit, merge_base.as_ref())
            .await?;
        let changes = changed
            .into_iter()
            .map(|c| PathChange {
                path: c.path,
                kind: c.kind,
            })
            .collect();
        Ok(Comparison {
            base: base_name,
            base_commit,
            head: head_name,
            head_commit,
            merge_base,
            ahead: ahead.len().min(COMPARE_MAX_COMMITS) as u32,
            behind: behind.len().min(COMPARE_MAX_COMMITS) as u32,
            counts_capped,
            changes,
            changes_truncated,
        })
    }

    /// The commits reachable from `tip` and not from `have`, at most `COMPARE_MAX_COMMITS` and one more.
    pub(super) async fn commits_between(
        &self,
        tip: ObjectId,
        have: ObjectId,
    ) -> Result<Vec<crate::types::Commit>> {
        self.meta
            .missing_commits(
                &self.repo,
                &[tip],
                &[have],
                None,
                COMPARE_MAX_COMMITS + 1,
                FETCH_MAX_EXAMINED,
            )
            .await
    }

    /// Every commit reachable from `tip`, at most `COMPARE_MAX_COMMITS` and one more.
    pub(super) async fn commits_between_none(
        &self,
        tip: ObjectId,
    ) -> Result<Vec<crate::types::Commit>> {
        self.meta
            .missing_commits(
                &self.repo,
                &[tip],
                &[],
                None,
                COMPARE_MAX_COMMITS + 1,
                FETCH_MAX_EXAMINED,
            )
            .await
    }

    /// The paths the commits touched whose live state at `head` differs from the one at `merge_base`, by path.
    pub(super) async fn changed_paths(
        &self,
        commits: &[crate::types::Commit],
        head: &ObjectId,
        merge_base: Option<&ObjectId>,
    ) -> Result<(Vec<Changed>, bool)> {
        let mut truncated = commits.len() > COMPARE_MAX_COMMITS;
        let mut candidates = BTreeSet::new();
        for commit in commits.iter().take(COMPARE_MAX_COMMITS) {
            for rev in self.meta.commit_pointers(&self.repo, &commit.id).await? {
                candidates.insert(rev.path);
            }
        }
        let mut changes = Vec::new();
        for (seen, path) in candidates.into_iter().enumerate() {
            if seen >= COMPARE_MAX_CANDIDATES || changes.len() >= COMPARE_MAX_PATHS {
                truncated = true;
                break;
            }
            let now = self.meta.revision_at(&self.repo, head, &path).await?;
            let before = match merge_base {
                Some(b) => self.meta.revision_at(&self.repo, b, &path).await?,
                None => None,
            };
            let kind = match (live(before.as_ref()), live(now.as_ref())) {
                (None, Some(_)) => ChangeKind::Added,
                (Some(_), None) => ChangeKind::Deleted,
                (Some(a), Some(b)) if (a.content, a.file_mode) != (b.content, b.file_mode) => {
                    ChangeKind::Modified
                }
                _ => continue,
            };
            changes.push(Changed {
                path,
                kind,
                before: live(before.as_ref()).cloned(),
                now: live(now.as_ref()).cloned(),
            });
        }
        Ok((changes, truncated))
    }
}
