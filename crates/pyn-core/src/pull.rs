//! Pull requests: a proposal to merge one branch into another, with comments and events (docs/concepts/pull-requests.md).

use chrono::{DateTime, Utc};
use pyn_object::ObjectId;

use crate::error::{PynError, Result};
use crate::types::{RepoId, UserId};

pub const PULL_TITLE_MAX: usize = 256;
pub const PULL_BODY_MAX: usize = 64 * 1024;
pub const PULL_LIST_MAX: usize = 100;
pub const PULL_LIST_DEFAULT: usize = 30;
/// Most commits a pull request lists.
pub const PULL_COMMITS_MAX: usize = 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PullState {
    Open,
    Merged,
    Closed,
}

impl PullState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Merged => "merged",
            Self::Closed => "closed",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "open" => Ok(Self::Open),
            "merged" => Ok(Self::Merged),
            "closed" => Ok(Self::Closed),
            other => Err(PynError::InvalidRequest(format!(
                "unknown pull request state {other:?}"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub body: String,
    pub state: PullState,
    pub draft: bool,
    /// The branch that moves when the pull request merges.
    pub base: String,
    /// The branch whose commits are proposed.
    pub head: String,
    pub author: UserId,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
    pub merged_at: Option<DateTime<Utc>>,
    pub merged_by: Option<UserId>,
    /// The commit a merge wrote, or the head's own commit for a fast-forward; none when the head was already in the base.
    pub merge_commit: Option<ObjectId>,
    /// The two branch heads when the pull request was closed or merged (the base's before the merge), so its commits and
    /// diff stay readable after the branches move or go.
    pub frozen: Option<(ObjectId, ObjectId)>,
    /// Comments on it.
    pub comments: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PullEventKind {
    Comment,
    Merged,
    Closed,
    Reopened,
}

impl PullEventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Comment => "comment",
            Self::Merged => "merged",
            Self::Closed => "closed",
            Self::Reopened => "reopened",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "comment" => Ok(Self::Comment),
            "merged" => Ok(Self::Merged),
            "closed" => Ok(Self::Closed),
            "reopened" => Ok(Self::Reopened),
            other => Err(PynError::Storage(format!("unknown pull event {other:?}"))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullEvent {
    /// Unique and increasing within the repository.
    pub id: u64,
    pub number: u64,
    pub kind: PullEventKind,
    pub actor: UserId,
    pub at: DateTime<Utc>,
    pub body: Option<String>,
    /// The commit a `merged` event wrote.
    pub commit: Option<ObjectId>,
}

#[derive(Debug, Clone)]
pub struct NewPull {
    pub title: String,
    pub body: String,
    pub draft: bool,
    pub base: String,
    pub head: String,
    pub author: UserId,
    pub now: DateTime<Utc>,
}

#[derive(Debug, Clone, Default)]
pub struct PullEdit {
    pub title: Option<String>,
    pub body: Option<String>,
    pub draft: Option<bool>,
}

#[derive(Debug, Clone, Default)]
pub struct PullFilter {
    pub state: Option<PullState>,
    pub base: Option<String>,
    pub head: Option<String>,
    pub author: Option<UserId>,
}

/// A change of state; each writes its event in the same step.
#[derive(Debug, Clone)]
pub enum PullTransition {
    /// Open to closed, freezing the two heads.
    Close { base: ObjectId, head: ObjectId },
    /// Closed to open; an open pull request with the same branches blocks it.
    Reopen,
    /// Open or closed to merged, freezing the heads.
    Merge {
        base: ObjectId,
        head: ObjectId,
        commit: Option<ObjectId>,
    },
}

/// Storage for pull requests, reached through `MetadataStore::pulls`. Every method is an atomic check-and-write; the
/// rules of who may do what live in `RepoService`.
#[async_trait::async_trait]
pub trait PullStore: Send + Sync {
    /// Numbers the repository's pull requests from 1. `PullRequestExists` when an open one has the same branches.
    async fn create_pull(&self, repo: &RepoId, new: NewPull) -> Result<PullRequest>;

    async fn get_pull(&self, repo: &RepoId, number: u64) -> Result<Option<PullRequest>>;

    /// Newest first, the numbers below `before` when given.
    async fn list_pulls(
        &self,
        repo: &RepoId,
        filter: &PullFilter,
        before: Option<u64>,
        limit: usize,
    ) -> Result<Vec<PullRequest>>;

    /// Edits an open pull request; `PullRequestNotFound`, or `PullRequestNotOpen`.
    async fn edit_pull(
        &self,
        repo: &RepoId,
        number: u64,
        edit: PullEdit,
        now: DateTime<Utc>,
    ) -> Result<PullRequest>;

    /// `PullRequestNotFound`, `PullRequestNotOpen` when the state does not allow it, `PullRequestExists` for a reopen.
    async fn transition_pull(
        &self,
        repo: &RepoId,
        number: u64,
        transition: PullTransition,
        actor: &UserId,
        now: DateTime<Utc>,
    ) -> Result<PullRequest>;

    /// Adds a comment in any state; `PullRequestNotFound`.
    async fn comment_pull(
        &self,
        repo: &RepoId,
        number: u64,
        actor: &UserId,
        body: &str,
        now: DateTime<Utc>,
    ) -> Result<PullEvent>;

    /// The events of one pull request, oldest first, with ids above `after`.
    async fn pull_events(
        &self,
        repo: &RepoId,
        number: u64,
        after: Option<u64>,
        limit: usize,
    ) -> Result<Vec<PullEvent>>;

    /// Points the pull requests that name branch `from` at `to`.
    async fn rename_pull_branch(&self, repo: &RepoId, from: &str, to: &str) -> Result<()>;
}

/// A title is one line of 1 to `PULL_TITLE_MAX` characters.
pub fn validate_title(title: &str) -> Result<String> {
    let title = title.trim();
    if title.is_empty()
        || title.chars().count() > PULL_TITLE_MAX
        || title.chars().any(char::is_control)
    {
        return Err(PynError::InvalidRequest(format!(
            "a pull request title is 1 to {PULL_TITLE_MAX} characters on one line"
        )));
    }
    Ok(title.to_string())
}

/// A body is at most `PULL_BODY_MAX` bytes without control characters other than newline and tab.
pub fn validate_body(body: &str) -> Result<()> {
    if body.len() > PULL_BODY_MAX
        || body
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\t' | '\r'))
    {
        return Err(PynError::InvalidRequest(format!(
            "text is at most {PULL_BODY_MAX} bytes without control characters other than newline and tab"
        )));
    }
    Ok(())
}
