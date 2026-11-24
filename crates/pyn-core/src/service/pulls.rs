//! Pull requests: the rules of who may open, comment, close and merge, and what a pull request shows
//! (docs/concepts/pull-requests.md).

use std::collections::BTreeSet;

use pyn_object::ObjectId;

use super::RepoService;
use super::diff::{Diff, DiffOptions};
use crate::access::{Permission, Principal};
use crate::branch::DEFAULT_BRANCH;
use crate::error::{PynError, Result};
use crate::merge::{ConflictKind, FastForward, MergeOutcome, MergeRequest};
use crate::pull::{
    NewPull, PULL_COMMITS_MAX, PULL_LIST_DEFAULT, PULL_LIST_MAX, PullEdit, PullEvent, PullFilter,
    PullRequest, PullState, PullTransition, validate_body, validate_title,
};
use crate::types::{Commit, Lock, RepoPath, Revision, UserId};

#[derive(Debug, Clone)]
pub struct OpenPull {
    pub title: String,
    pub body: String,
    pub draft: bool,
    /// The branch to merge into; the default branch when `None`.
    pub base: Option<String>,
    pub head: String,
}

#[derive(Debug, Clone, Default)]
pub struct MergePull {
    pub fast_forward: FastForward,
    /// The merge commit's message; `Merge pull request #N from <head>` when `None`.
    pub message: Option<String>,
    /// The commit the caller saw as the base branch's head; a different head is `merge_stale`.
    pub expect_target: Option<ObjectId>,
    /// The commit the caller saw as the head branch's head.
    pub expect_head: Option<ObjectId>,
}

/// What merging an open pull request would do right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullMergeability {
    pub outcome: MergeOutcome,
    pub conflicts: Vec<(RepoPath, ConflictKind)>,
}

#[derive(Debug, Clone)]
pub struct PullDetail {
    pub pull: PullRequest,
    /// Only for an open pull request whose branches both exist.
    pub mergeability: Option<PullMergeability>,
    /// Commits in the range, at most `PULL_COMMITS_MAX`.
    pub commits: u32,
    pub changed_files: u32,
    pub additions: u32,
    pub deletions: u32,
    /// The live locks on paths the pull request changes, whoever holds them; empty once it is closed or merged.
    pub locks: Vec<Lock>,
    /// The author and everyone who commented, in order of first appearance.
    pub participants: Vec<UserId>,
}

fn changed(a: Option<&Revision>, b: Option<&Revision>) -> bool {
    let live = |r: Option<&Revision>| r.filter(|r| !r.deleted).map(|r| (r.content, r.file_mode));
    live(a) != live(b)
}

impl RepoService {
    /// Proposes merging `head` into `base`. Needs `checkin`.
    pub async fn open_pull(&self, who: &Principal, req: OpenPull) -> Result<PullRequest> {
        who.require(Permission::Checkin)?;
        self.ensure_writable().await?;
        let title = validate_title(&req.title)?;
        validate_body(&req.body)?;
        let base = req.base.unwrap_or_else(|| DEFAULT_BRANCH.to_string());
        if base == req.head {
            return Err(PynError::InvalidRequest(
                "a pull request needs two different branches".into(),
            ));
        }
        let base_branch = self.branch(&base).await?;
        let head_branch = self.branch(&req.head).await?;
        let head_commit = head_branch.head.ok_or_else(|| {
            PynError::InvalidRequest(format!("the branch {} has no commits yet", req.head))
        })?;
        if let Some(base_commit) = base_branch.head
            && self
                .commits_between(head_commit, base_commit)
                .await?
                .is_empty()
        {
            return Err(PynError::InvalidRequest(format!(
                "nothing to merge: {} has no commits that {base} lacks",
                req.head
            )));
        }
        self.meta
            .pulls()
            .create_pull(
                &self.repo,
                NewPull {
                    title,
                    body: req.body,
                    draft: req.draft,
                    base,
                    head: req.head,
                    author: who.user.clone(),
                    now: self.clock.now(),
                },
            )
            .await
    }

    pub async fn get_pull(&self, number: u64) -> Result<PullRequest> {
        self.meta
            .pulls()
            .get_pull(&self.repo, number)
            .await?
            .ok_or(PynError::PullRequestNotFound(number))
    }

    /// Newest first; `before` is the number of the last one of the previous page.
    pub async fn list_pulls(
        &self,
        filter: &PullFilter,
        before: Option<u64>,
        limit: Option<usize>,
    ) -> Result<Vec<PullRequest>> {
        let limit = limit.unwrap_or(PULL_LIST_DEFAULT).clamp(1, PULL_LIST_MAX);
        self.meta
            .pulls()
            .list_pulls(&self.repo, filter, before, limit)
            .await
    }

    /// The ends of a pull request: the commits frozen when it closed, otherwise the two branch heads (the base
    /// is `None` when its branch has no commits).
    async fn pull_ends(&self, p: &PullRequest) -> Result<(Option<ObjectId>, ObjectId)> {
        if let Some((base, head)) = p.frozen {
            return Ok((Some(base), head));
        }
        let head = self.branch(&p.head).await?.head.ok_or_else(|| {
            PynError::InvalidRequest(format!("the branch {} has no commits yet", p.head))
        })?;
        Ok((self.branch(&p.base).await?.head, head))
    }

    pub async fn pull_commits(&self, number: u64) -> Result<Vec<Commit>> {
        let p = self.get_pull(number).await?;
        let (base, head) = self.pull_ends(&p).await?;
        let mut commits = match base {
            Some(base) => self.commits_between(head, base).await?,
            None => self.commits_between_none(head).await?,
        };
        commits.truncate(PULL_COMMITS_MAX);
        Ok(commits)
    }

    pub async fn pull_diff(&self, number: u64, context: usize) -> Result<Diff> {
        let p = self.get_pull(number).await?;
        let (base, head) = self.pull_ends(&p).await?;
        let options = DiffOptions {
            context,
            ..DiffOptions::default()
        };
        self.diff(base.map(|b| b.to_hex()).as_deref(), &head.to_hex(), options)
            .await
            .map(|mut d| {
                d.base.clone_from(&p.base);
                d.head.clone_from(&p.head);
                d
            })
    }

    /// The pull request with its mergeability, change counts, locks and participants.
    pub async fn pull_detail(&self, number: u64) -> Result<PullDetail> {
        let pull = self.get_pull(number).await?;
        let diff = self.pull_diff(number, 0).await.ok();
        let (changed_files, additions, deletions) = diff.as_ref().map_or((0, 0, 0), |d| {
            (
                d.files.len() as u32,
                d.files.iter().map(|f| f.additions).sum(),
                d.files.iter().map(|f| f.deletions).sum(),
            )
        });
        let commits = match self.pull_commits(number).await {
            Ok(c) => c.len() as u32,
            Err(_) => 0,
        };
        let open = pull.state == PullState::Open;
        let mergeability = if open {
            self.pull_mergeability(&pull).await.ok()
        } else {
            None
        };
        let locks = match (&diff, open) {
            (Some(d), true) => self.locks_on_changes(d).await?,
            _ => Vec::new(),
        };
        let events = self
            .meta
            .pulls()
            .pull_events(&self.repo, number, None, 10_000)
            .await?;
        let mut participants = vec![pull.author.clone()];
        for e in &events {
            if !participants.contains(&e.actor) {
                participants.push(e.actor.clone());
            }
        }
        Ok(PullDetail {
            pull,
            mergeability,
            commits,
            changed_files,
            additions,
            deletions,
            locks,
            participants,
        })
    }

    async fn pull_mergeability(&self, p: &PullRequest) -> Result<PullMergeability> {
        let report = self
            .merge_preview(&MergeRequest {
                source: p.head.clone(),
                target: Some(p.base.clone()),
                fast_forward: FastForward::Allow,
                message: None,
                expect_target: None,
                resolutions: Vec::new(),
            })
            .await?;
        Ok(PullMergeability {
            outcome: report.outcome,
            conflicts: report
                .conflicts
                .into_iter()
                .map(|c| (c.path, c.kind))
                .collect(),
        })
    }

    /// The live locks on paths the diff changes (a lock on a path the pull request does not touch is not listed).
    async fn locks_on_changes(&self, diff: &Diff) -> Result<Vec<Lock>> {
        let paths: BTreeSet<&RepoPath> = diff
            .files
            .iter()
            .flat_map(|f| std::iter::once(&f.path).chain(f.old_path.as_ref()))
            .collect();
        let mut held = Vec::new();
        for lock in self.locks().await? {
            if paths.contains(&lock.path) {
                held.push(lock);
                continue;
            }
            if !diff.truncated {
                continue;
            }
            let (Some(from), head) = (diff.from_commit, diff.head_commit) else {
                continue;
            };
            let before = self.meta.revision_at(&self.repo, &from, &lock.path).await?;
            let now = self.meta.revision_at(&self.repo, &head, &lock.path).await?;
            if changed(before.as_ref(), now.as_ref()) {
                held.push(lock);
            }
        }
        Ok(held)
    }

    pub async fn pull_events(
        &self,
        number: u64,
        after: Option<u64>,
        limit: Option<usize>,
    ) -> Result<Vec<PullEvent>> {
        self.get_pull(number).await?;
        let limit = limit.unwrap_or(PULL_LIST_DEFAULT).clamp(1, PULL_LIST_MAX);
        self.meta
            .pulls()
            .pull_events(&self.repo, number, after, limit)
            .await
    }

    /// Anyone who can read the repository may comment, on a pull request in any state.
    pub async fn comment_pull(
        &self,
        who: &Principal,
        number: u64,
        body: &str,
    ) -> Result<PullEvent> {
        who.require(Permission::Read)?;
        self.ensure_writable().await?;
        let body = body.trim();
        if body.is_empty() {
            return Err(PynError::InvalidRequest("a comment cannot be empty".into()));
        }
        validate_body(body)?;
        self.meta
            .pulls()
            .comment_pull(&self.repo, number, &who.user, body, self.clock.now())
            .await
    }

    /// The author, or anyone with `checkin`, may edit, close and reopen.
    fn require_pull_manager(&self, who: &Principal, p: &PullRequest) -> Result<()> {
        if p.author == who.user || who.has(Permission::Checkin) {
            Ok(())
        } else {
            Err(PynError::Forbidden(Permission::Checkin))
        }
    }

    pub async fn edit_pull(
        &self,
        who: &Principal,
        number: u64,
        edit: PullEdit,
    ) -> Result<PullRequest> {
        who.require(Permission::Read)?;
        self.ensure_writable().await?;
        let p = self.get_pull(number).await?;
        self.require_pull_manager(who, &p)?;
        let edit = PullEdit {
            title: edit.title.as_deref().map(validate_title).transpose()?,
            ..edit
        };
        if let Some(b) = &edit.body {
            validate_body(b)?;
        }
        self.meta
            .pulls()
            .edit_pull(&self.repo, number, edit, self.clock.now())
            .await
    }

    pub async fn close_pull(&self, who: &Principal, number: u64) -> Result<PullRequest> {
        who.require(Permission::Read)?;
        self.ensure_writable().await?;
        let p = self.get_pull(number).await?;
        self.require_pull_manager(who, &p)?;
        let (base, head) = self.pull_ends(&p).await?;
        let base = base.unwrap_or(head);
        self.meta
            .pulls()
            .transition_pull(
                &self.repo,
                number,
                PullTransition::Close { base, head },
                &who.user,
                self.clock.now(),
            )
            .await
    }

    /// Reopens a closed pull request whose head branch still has commits the base lacks.
    pub async fn reopen_pull(&self, who: &Principal, number: u64) -> Result<PullRequest> {
        who.require(Permission::Read)?;
        self.ensure_writable().await?;
        let p = self.get_pull(number).await?;
        self.require_pull_manager(who, &p)?;
        if p.state == PullState::Closed {
            let head = self.branch(&p.head).await?.head;
            let base = self.branch(&p.base).await?.head;
            let ahead = match (head, base) {
                (Some(h), Some(b)) => self.commits_between(h, b).await?.len(),
                (Some(_), None) => 1,
                _ => 0,
            };
            if ahead == 0 {
                return Err(PynError::InvalidRequest(format!(
                    "nothing to merge: {} has no commits that {} lacks",
                    p.head, p.base
                )));
            }
        }
        self.meta
            .pulls()
            .transition_pull(
                &self.repo,
                number,
                PullTransition::Reopen,
                &who.user,
                self.clock.now(),
            )
            .await
    }

    /// Merges the head branch into the base branch. Needs `checkin`; a draft, a closed pull request, a conflict and
    /// locks other people hold on the changed paths refuse it, unless the merger holds `force_unlock`.
    pub async fn merge_pull(
        &self,
        who: &Principal,
        number: u64,
        request: MergePull,
    ) -> Result<PullRequest> {
        who.require(Permission::Checkin)?;
        self.ensure_writable().await?;
        let p = self.get_pull(number).await?;
        if p.state != PullState::Open {
            return Err(PynError::PullRequestNotOpen {
                number,
                state: p.state.as_str().into(),
            });
        }
        if p.draft {
            return Err(PynError::PullRequestDraft(number));
        }
        let (base_before, head) = self.pull_ends(&p).await?;
        if request.expect_head.is_some_and(|h| h != head) {
            return Err(PynError::MergeStale(p.head));
        }
        if !who.has(Permission::ForceUnlock) {
            let diff = self
                .diff(
                    Some(&p.base),
                    &p.head,
                    DiffOptions {
                        context: 0,
                        ..DiffOptions::default()
                    },
                )
                .await?;
            let blocking: Vec<(RepoPath, UserId)> = self
                .locks_on_changes(&diff)
                .await?
                .into_iter()
                .filter(|l| l.owner != who.user)
                .map(|l| (l.path, l.owner))
                .collect();
            if !blocking.is_empty() {
                return Err(PynError::PullLocksHeld(blocking));
            }
        }
        let message = request
            .message
            .unwrap_or_else(|| format!("Merge pull request #{number} from {}", p.head));
        let report = self
            .merge(
                who,
                MergeRequest {
                    source: p.head.clone(),
                    target: Some(p.base.clone()),
                    fast_forward: request.fast_forward,
                    message: Some(message),
                    expect_target: request.expect_target,
                    resolutions: Vec::new(),
                },
            )
            .await?;
        self.meta
            .pulls()
            .transition_pull(
                &self.repo,
                number,
                PullTransition::Merge {
                    base: base_before.unwrap_or(head),
                    head,
                    commit: report.commit,
                },
                &who.user,
                self.clock.now(),
            )
            .await
    }
}
