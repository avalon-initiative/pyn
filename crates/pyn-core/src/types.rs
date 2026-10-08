use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{PynError, Result};

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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewRevision {
    pub path: RepoPath,
    pub content: ContentHash,
    pub author: UserId,
    pub message: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lock {
    pub path: RepoPath,
    pub owner: UserId,
    pub acquired_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}
