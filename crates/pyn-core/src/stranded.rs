//! Stranded versions: shared versions a branch holds of a path that became exclusive on the default branch
//! (docs/concepts/pyn-toml.md, decisions #38 and #195).

use chrono::{DateTime, Utc};
use pyn_object::ObjectId;

use crate::types::{RepoPath, RevisionId, UserId};

/// A diverged shared version of a path on a branch, outside the default branch's history, that can no longer merge
/// into the path's single line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrandedVersion {
    pub path: RepoPath,
    pub branch: String,
    /// Who created the branch, told of the stranding through the audit log.
    pub branch_owner: UserId,
    pub revision: RevisionId,
    pub commit: ObjectId,
    pub author: UserId,
    pub created_at: DateTime<Utc>,
    /// The conversion revision the default branch made; `None` in a preview.
    pub conversion: Option<RevisionId>,
}

/// The conversion a merge conflict met: what happened to the path on the other side, by whom and when.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversionNote {
    /// The conversion revision, the first of the path's exclusive line.
    pub revision: RevisionId,
    /// The policy revision in force at the conversion.
    pub policy: Option<RevisionId>,
    pub by: UserId,
    pub at: DateTime<Utc>,
    /// The diverged shared version the conversion stranded.
    pub diverged: RevisionId,
    pub diverged_by: UserId,
    pub diverged_at: DateTime<Utc>,
}
