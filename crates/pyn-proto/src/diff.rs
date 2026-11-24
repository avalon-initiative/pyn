//! Line diffs between two revisions and the lines of a file (`GET /v1/repos/{owner}/{name}/diff` and `/lines`).

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{FileMode, Lock, Mode};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DiffFileStatus {
    Added,
    Modified,
    Deleted,
    /// Same content under a new path; `old_path` names the previous one.
    Renamed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DiffLineKind {
    Context,
    Add,
    Delete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    /// Line number in the old file; absent on an added line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_no: Option<u32>,
    /// Line number in the new file; absent on a deleted line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_no: Option<u32>,
    /// The line without its newline.
    pub text: String,
    /// The file's last line on this side has no newline.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub no_newline: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DiffHunk {
    /// The `@@ -a,b +c,d @@ section` line.
    pub header: String,
    pub old_start: u32,
    pub old_lines: u32,
    pub new_start: u32,
    pub new_lines: u32,
    pub lines: Vec<DiffLine>,
    /// Unchanged lines between the previous hunk (or the start of the file) and this one; `/lines` loads them.
    pub hidden_before: u32,
    /// Unchanged lines after the last hunk to the end of the file; 0 on every other hunk.
    pub hidden_after: u32,
}

/// One changed path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DiffFile {
    pub path: String,
    /// The previous path of a renamed file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
    pub status: DiffFileStatus,
    pub additions: u32,
    pub deletions: u32,
    /// A binary file has no hunks.
    pub binary: bool,
    /// The file, or the diff as a whole, is over the size limits: no hunks, and `additions` and `deletions` are 0.
    pub too_large: bool,
    /// The path's mode under the repository's policy.
    pub mode: Mode,
    /// The git file mode of the new side (of the old side for a deletion).
    pub file_mode: FileMode,
    /// The previous git file mode, when it changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_file_mode: Option<FileMode>,
    /// Git blob ids of the two sides; absent where the path does not exist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_content: Option<String>,
    /// The live lock on the path, whoever holds it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock: Option<Lock>,
    pub hunks: Vec<DiffHunk>,
}

/// The changes of a head against a base, with line hunks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RepoDiff {
    /// The base as named in the request (the default branch when none was given).
    pub base: String,
    pub base_commit: String,
    pub head: String,
    pub head_commit: String,
    /// The commit the changes are measured from: the merge base, or the base for a direct diff.
    #[schema(required = true)]
    pub from_commit: Option<String>,
    /// Ordered by path.
    pub files: Vec<DiffFile>,
    /// More paths changed than listed.
    pub truncated: bool,
}

/// A range of the lines of a file at a revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct FileLines {
    pub path: String,
    pub commit: String,
    /// The path's revision number at that commit.
    pub revision: u64,
    /// Lines in the whole file.
    pub total: u32,
    /// The number of the first line in `lines`, counting from 1.
    pub start: u32,
    /// Lines without their newlines; empty past the end of the file.
    pub lines: Vec<String>,
}
