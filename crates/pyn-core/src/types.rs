use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{PynError, Result};
use crate::rules::Mode;
use pyn_object::{FileMode, ObjectId};

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

/// A commit's author or committer as git records it, apart from the time: the commit's own time applies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Person {
    pub name: String,
    pub email: String,
    /// Minutes east of UTC.
    pub offset_minutes: i32,
}

impl Person {
    /// Strips the characters git's identity line cannot hold; an empty name becomes `unknown`.
    pub fn new(name: &str, email: &str, offset_minutes: i32) -> Self {
        let clean = |s: &str| {
            s.chars()
                .filter(|c| !matches!(c, '<' | '>' | '\n' | '\0'))
                .collect::<String>()
                .trim()
                .to_string()
        };
        let name = clean(name);
        Self {
            name: if name.is_empty() {
                "unknown".to_string()
            } else {
                name
            },
            email: clean(email),
            offset_minutes,
        }
    }

    /// The account's name and email, UTC; the `.invalid` placeholder stands in for an account without an email.
    pub fn for_user(user: &UserId, email: Option<&str>) -> Self {
        match email {
            Some(e) => Self::new(user.as_str(), e, 0),
            None => Self::new(user.as_str(), &format!("{user}@pyn.invalid"), 0),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Revision {
    pub id: RevisionId,
    pub path: RepoPath,
    /// The git blob id of the content.
    pub content: ObjectId,
    /// The content string the v1 hash covers, when it is not the blob id: revisions migrated from SHA-256 content hashes.
    pub legacy_content: Option<String>,
    pub author: UserId,
    /// The structured author the v2 hash covers; `None` for a v1 revision.
    pub person: Option<Person>,
    /// The encoding `hash` uses (`pyn_hash::REVISION_V1` or `REVISION_V2`); it must agree with `person`.
    pub hash_version: u32,
    pub message: String,
    pub created_at: DateTime<Utc>,
    /// Set when this revision restores the content of an earlier one.
    pub restored_from: Option<RevisionId>,
    /// The path's mode when the revision was made; `None` for revisions that predate recording.
    pub mode: Option<Mode>,
    /// Git's file mode of the content: regular, executable or symlink.
    pub file_mode: FileMode,
    /// Shared by the revisions a changeset checked in together; `None` for a single-path checkin.
    pub changeset: Option<String>,
    /// The id of the commit this revision belongs to.
    pub commit: ObjectId,
    pub hash: RevisionHash,
    /// Hash of the path's previous revision; `None` for the first.
    pub previous: Option<RevisionHash>,
}

/// One atomic write: a single checkin or a whole changeset, recorded as a git commit on the single mainline. Its id is
/// the git commit id over `tree`, `parents`, the author and committer and the message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    pub id: ObjectId,
    pub parents: Vec<ObjectId>,
    /// The root git tree of the repository as of this commit.
    pub tree: ObjectId,
    pub author: Person,
    pub committer: Person,
    /// Whole seconds; the author and committer time.
    pub created_at: DateTime<Utc>,
    pub message: String,
    /// The commit's `gpgsig` header, if it is signed.
    pub signature: Option<String>,
    pub changeset: Option<String>,
    /// The revisions it made, ordered by path.
    pub revisions: Vec<Revision>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewRevision {
    pub path: RepoPath,
    pub content: ObjectId,
    pub file_mode: FileMode,
    pub author: UserId,
    pub person: Person,
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
