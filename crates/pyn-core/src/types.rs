use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{PynError, Result};
use crate::rules::Mode;

macro_rules! string_id {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            pub fn new(s: impl Into<String>) -> Self {
                Self(s.into())
            }
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

string_id!(RepoId);
string_id!(UserId);
string_id!(
    /// SHA-256 hex of a file's content; the `ObjectStore` key.
    ContentHash
);
string_id!(
    /// SHA-256 hex over a revision's canonical encoding (docs/concepts/integrity.md).
    RevisionHash
);

/// Repo-relative `/`-separated path; rejects absolute, `.`/`..` and backslash forms.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RepoPath(String);

impl RepoPath {
    pub fn new(s: impl Into<String>) -> Result<Self> {
        let s = s.into();
        let bad = s.is_empty()
            || s.starts_with('/')
            || s.ends_with('/')
            || s.contains('\\')
            || s.contains('\0')
            || s.split('/')
                .any(|seg| seg.is_empty() || seg == "." || seg == "..");
        if bad {
            Err(PynError::InvalidPath(s))
        } else {
            Ok(Self(s))
        }
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for RepoPath {
    type Error = PynError;
    fn try_from(s: String) -> Result<Self> {
        Self::new(s)
    }
}

impl From<RepoPath> for String {
    fn from(p: RepoPath) -> String {
        p.0
    }
}

impl std::fmt::Display for RepoPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Per-path revision number, starting at 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RevisionId(pub u64);

impl std::fmt::Display for RevisionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Revision {
    pub id: RevisionId,
    pub path: RepoPath,
    pub content: ContentHash,
    pub author: UserId,
    pub message: String,
    pub created_at: DateTime<Utc>,
    /// Set when this revision restores the content of an earlier one.
    pub restored_from: Option<RevisionId>,
    /// The path's mode when the revision was made; `None` for revisions that predate recording.
    pub mode: Option<Mode>,
    /// Shared by the revisions a changeset checked in together; `None` for a single-path checkin.
    pub changeset: Option<String>,
    /// The repository-wide commit this revision belongs to; see `Commit`.
    pub commit: u64,
    pub hash: RevisionHash,
    /// Hash of the path's previous revision; `None` for the first.
    pub previous: Option<RevisionHash>,
}

/// One atomic write to the repository: a single checkin or a whole changeset. Commits are numbered 1, 2, ... in order,
/// and the repository's state at commit N is the newest revision of every path made at or before N.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    pub id: u64,
    pub author: UserId,
    pub message: String,
    pub created_at: DateTime<Utc>,
    pub changeset: Option<String>,
    /// The revisions it made, ordered by path.
    pub revisions: Vec<Revision>,
}

impl Commit {
    /// Groups revisions of one commit; `None` if there are none.
    pub fn from_revisions(mut revisions: Vec<Revision>) -> Option<Self> {
        revisions.sort_by(|a, b| a.path.cmp(&b.path));
        let first = revisions.first()?;
        Some(Self {
            id: first.commit,
            author: first.author.clone(),
            message: first.message.clone(),
            created_at: first.created_at,
            changeset: first.changeset.clone(),
            revisions,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewRevision {
    pub path: RepoPath,
    pub content: ContentHash,
    pub author: UserId,
    pub message: String,
    pub created_at: DateTime<Utc>,
    pub restored_from: Option<RevisionId>,
    pub mode: Mode,
    /// Length of the content in bytes.
    pub size: u64,
    pub changeset: Option<String>,
}

/// One path of a changeset: its revision, the head it must be based on and, for an exclusive path, the lock holder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangesetItem {
    pub revision: NewRevision,
    pub expected_head: Option<RevisionId>,
    pub lock_holder: Option<UserId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lock {
    pub path: RepoPath,
    pub owner: UserId,
    pub acquired_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_cannot_escape_or_alias() {
        for bad in ["", "/etc/passwd", "a/../b", "a//b", "a/", "./a", "a\\b"] {
            assert!(RepoPath::new(bad).is_err(), "{bad:?} should be rejected");
        }
        assert!(RepoPath::new("Content/World/Main.umap").is_ok());
    }
}
