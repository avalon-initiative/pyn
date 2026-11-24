//! What a branch list shows beside each name: who last updated it, how far it is from the default branch, whether it is stale.

use chrono::Duration;

use super::RepoService;
use super::compare::COMPARE_MAX_COMMITS;
use super::fetch::FETCH_MAX_EXAMINED;
use crate::branch::Branch;
use crate::branch::DEFAULT_BRANCH;
use crate::error::{PynError, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchStatus {
    /// The author of the head commit; `None` for a branch with no commits.
    pub updated_by: Option<String>,
    /// Commits the branch has and the default branch lacks, at most `COMPARE_MAX_COMMITS`.
    pub ahead: u32,
    /// Commits the default branch has and the branch lacks, at most `COMPARE_MAX_COMMITS`.
    pub behind: u32,
    /// Either count reached its limit, so the real number is larger.
    pub counts_capped: bool,
    /// The head commit is older than the stale period; never the default branch.
    pub stale: bool,
}

impl RepoService {
    /// The status of `branch` against the default branch; `stale_after` is the age past which a branch counts as stale.
    pub async fn branch_status(
        &self,
        branch: &Branch,
        stale_after: Duration,
    ) -> Result<BranchStatus> {
        let Some(head) = &branch.head else {
            return Ok(BranchStatus {
                updated_by: None,
                ahead: 0,
                behind: 0,
                counts_capped: false,
                stale: false,
            });
        };
        let commit = self.meta.get_commit(&self.repo, head).await?;
        let updated_by = commit.as_ref().map(|c| c.author.name.clone());
        let stale = !branch.default
            && commit
                .as_ref()
                .is_some_and(|c| self.clock.now() - c.created_at > stale_after);
        let base = match self.meta.get_branch(&self.repo, DEFAULT_BRANCH).await? {
            Some(b) if !branch.default => b.head,
            _ => None,
        };
        let (ahead, behind, counts_capped) = if branch.default {
            (0, 0, false)
        } else {
            let haves: Vec<_> = base.iter().copied().collect();
            let ahead = self.count_missing(head, &haves).await?;
            let behind = match &base {
                Some(b) => self.count_missing(b, &[*head]).await?,
                None => 0,
            };
            (
                ahead.min(COMPARE_MAX_COMMITS) as u32,
                behind.min(COMPARE_MAX_COMMITS) as u32,
                ahead > COMPARE_MAX_COMMITS || behind > COMPARE_MAX_COMMITS,
            )
        };
        Ok(BranchStatus {
            updated_by,
            ahead,
            behind,
            counts_capped,
            stale,
        })
    }

    /// Commits reachable from `tip` and not from `haves`, counted up to one past the comparison limit; a walk too long
    /// to finish counts as past the limit.
    async fn count_missing(
        &self,
        tip: &pyn_object::ObjectId,
        haves: &[pyn_object::ObjectId],
    ) -> Result<usize> {
        match self
            .meta
            .missing_commits(
                &self.repo,
                &[*tip],
                haves,
                None,
                COMPARE_MAX_COMMITS + 1,
                FETCH_MAX_EXAMINED,
            )
            .await
        {
            Ok(commits) => Ok(commits.len()),
            Err(PynError::InvalidRequest(_)) => Ok(COMPARE_MAX_COMMITS + 1),
            Err(e) => Err(e),
        }
    }
}
