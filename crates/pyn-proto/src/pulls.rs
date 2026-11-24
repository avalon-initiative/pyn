//! Pull requests (`/v1/repos/{owner}/{name}/pulls`): the record, its commits and events, and the requests that change it.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{Lock, MergeConflictKind, MergeFastForward, MergeOutcome, PublicIdentity};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PullRequestState {
    Open,
    Merged,
    Closed,
}

/// A pull request as a list shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PullRequestSummary {
    pub number: u64,
    pub title: String,
    pub state: PullRequestState,
    /// A draft cannot be merged until it is marked ready.
    pub draft: bool,
    pub author: PublicIdentity,
    /// The branch that moves when it merges.
    pub base: String,
    /// The branch whose commits are proposed.
    pub head: String,
    pub opened_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merged_at: Option<DateTime<Utc>>,
    /// Who merged it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merged_by: Option<PublicIdentity>,
    /// Comments on it.
    pub comments: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PullRequestPage {
    /// Newest first.
    pub pulls: Vec<PullRequestSummary>,
    /// Pass as `before` to fetch the next, older page; null on the last page.
    #[schema(required = true)]
    pub next_before: Option<u64>,
}

/// A path whose merge would conflict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PullConflict {
    pub path: String,
    pub kind: MergeConflictKind,
}

/// One pull request with what the page for it shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PullRequest {
    #[serde(flatten)]
    pub summary: PullRequestSummary,
    pub body: String,
    /// The head branch's commit: what an open pull request would merge, or what it merged or was closed at.
    /// Absent for an open pull request whose head branch is gone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_commit: Option<String>,
    /// The base branch's commit the same way (before the merge, for a merged one); pass it as `expect_target`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_commit: Option<String>,
    /// The commit a merge wrote; absent unless merged, and for a head that was already in the base.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_commit: Option<String>,
    /// Commits in the range, at most 1000.
    pub commits: u32,
    pub changed_files: u32,
    pub additions: u32,
    pub deletions: u32,
    /// What merging would do now; only for an open pull request whose branches exist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<MergeOutcome>,
    /// The paths that would conflict, when `outcome` is `conflicted`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflicts: Vec<PullConflict>,
    /// Live locks on the paths it changes, whoever holds them; those held by others block the merge unless the merger
    /// may force unlocks. Empty once closed or merged.
    pub locks: Vec<Lock>,
    /// The author and everyone who commented, in order of first appearance.
    pub participants: Vec<PublicIdentity>,
}

/// A commit of a pull request's range, oldest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PullCommit {
    pub id: String,
    pub message: String,
    pub author: PublicIdentity,
    pub committed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PullEventKind {
    Comment,
    Merged,
    Closed,
    Reopened,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PullEvent {
    /// Unique and increasing within the repository; pass the last one as `after`.
    pub id: String,
    pub kind: PullEventKind,
    pub actor: PublicIdentity,
    pub at: DateTime<Utc>,
    /// The Markdown of a comment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// The commit a `merged` event wrote.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CreatePullRequest {
    /// One line, 1 to 256 characters.
    pub title: String,
    /// Markdown, up to 64 KiB.
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub draft: Option<bool>,
    /// The branch to merge into; the default branch if omitted.
    #[serde(default)]
    pub base: Option<String>,
    pub head: String,
}

/// Only the fields present change.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UpdatePullRequest {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub draft: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MergePullRequest {
    /// `allow` (the default), `never` or `only`, as for a merge.
    #[serde(default)]
    pub fast_forward: Option<MergeFastForward>,
    /// The merge commit's message; `Merge pull request #N from <head>` if omitted.
    #[serde(default)]
    pub message: Option<String>,
    /// The base branch's commit the caller saw; a different head is `409 merge_stale`.
    #[serde(default)]
    pub expect_target: Option<String>,
    /// The head branch's commit the caller saw.
    #[serde(default)]
    pub expect_head: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CommentPullRequest {
    pub body: String,
}
