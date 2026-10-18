//! Directory listings and the repository summary, derived from head revisions, live locks and the audit log.

use chrono::{DateTime, Utc};

use crate::audit::{AuditAction, AuditEvent};
use crate::rules::Mode;
use crate::types::{Lock, RepoPath, Revision};

/// Audit actions that describe work on files; administrative events stay behind `view_audit`.
pub const ACTIVITY_ACTIONS: [AuditAction; 6] = [
    AuditAction::Checkout,
    AuditAction::Release,
    AuditAction::Checkin,
    AuditAction::Delete,
    AuditAction::Restore,
    AuditAction::ForceUnlock,
];

/// Heads one folder listing reads at most; a larger folder is `invalid_request` and its subfolders are listed instead.
pub const TREE_MAX_PATHS: usize = 1_000_000;

/// Folders sort before files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EntryKind {
    Folder,
    File,
}

/// A folder is `Mixed` when the files under it do not all share one mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryMode {
    Shared,
    Exclusive,
    Mixed,
}

impl EntryMode {
    pub(crate) fn merge(self, other: Mode) -> Self {
        if self == Self::from(other) {
            self
        } else {
            Self::Mixed
        }
    }
}

impl From<Mode> for EntryMode {
    fn from(m: Mode) -> Self {
        match m {
            Mode::Shared => Self::Shared,
            Mode::Exclusive => Self::Exclusive,
        }
    }
}

/// One child of a directory. `last_change` is the newest head revision at or under the entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    pub name: String,
    pub path: RepoPath,
    pub kind: EntryKind,
    pub mode: EntryMode,
    pub last_change: Option<Revision>,
    /// Live lock on the file; always `None` for folders.
    pub lock: Option<Lock>,
}

/// What a commit's snapshot holds, kept by the store as commits are written so reading it costs the same at any size.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HeadCounts {
    /// Paths with a live revision.
    pub files: u64,
    /// Those whose revision records the exclusive mode.
    pub exclusive_files: u64,
    /// Time of the newest revision up to the commit; `None` when there is none.
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoSummary {
    pub default_branch: String,
    pub branch_count: u32,
    pub files: u64,
    pub exclusive_files: u64,
    pub shared_files: u64,
    /// Time of the newest revision; `None` for an empty repository.
    pub updated_at: Option<DateTime<Utc>>,
    /// Live locks, ordered by path.
    pub locks: Vec<Lock>,
    /// File and lock events, newest first.
    pub activity: Vec<AuditEvent>,
}
