//! Merging two commits (docs/concepts/merging.md): the data types and the per-path decisions, with no I/O.

use pyn_object::{FileMode, Kind, ObjectId, hash_object};

use crate::error::{PynError, Result};
use crate::object::REPO_ALGORITHM;
use crate::rules::Mode;
use crate::types::ChangesetItem;
use crate::types::{Person, RepoPath, Revision, RevisionId};

/// Files larger than this are never merged as text: changed on both sides, they are a whole-file conflict.
pub const MAX_TEXT_MERGE: u64 = 8 * 1024 * 1024;

/// How far a virtual merge base may recurse into the bases of its own bases.
pub const MAX_BASE_DEPTH: u8 = 4;

/// Whether a merge may or must fast-forward.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FastForward {
    #[default]
    Allow,
    /// Always make a merge commit when the histories differ.
    Never,
    /// Refuse unless the target can fast-forward.
    Only,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeOutcome {
    UpToDate,
    FastForward,
    Merge,
    /// A preview only: the merge cannot be written until the conflicts are resolved.
    Conflicted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictKind {
    /// Both sides changed the same lines of a text file.
    Content,
    /// Both sides added the path with different content.
    BothAdded,
    /// Both sides changed a file that cannot be merged line by line.
    Binary,
    /// One side removed the path and the other changed it.
    DeleteModify,
    /// Both sides changed the git file mode differently.
    FileMode,
    /// A shared path changed on a branch that meets its conversion to exclusive (decision #195 item 4).
    ExclusiveConversion,
}

/// One version of a path as a merge sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Side {
    /// The path's revision holding this version; `None` for the virtual base of a criss-cross merge.
    pub revision: Option<RevisionId>,
    pub content: ObjectId,
    pub file_mode: FileMode,
}

/// The three versions a path's merge is made from.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Inputs {
    pub base: Option<Side>,
    pub ours: Option<Side>,
    pub theirs: Option<Side>,
    /// The base's bytes when no revision holds it, so a client can still show or merge against it.
    pub base_inline: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    pub path: RepoPath,
    pub mode: Mode,
    pub kind: ConflictKind,
    pub inputs: Inputs,
}

/// A path the merge changes in the target, with the content it ends up with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub path: RepoPath,
    pub mode: Mode,
    pub content: ObjectId,
    pub file_mode: FileMode,
    /// A revision that holds exactly this content and file mode, when one does.
    pub revision: Option<RevisionId>,
    /// The versions to merge, only when no revision holds the result (a text merge).
    pub inputs: Option<Inputs>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeReport {
    pub target: String,
    pub target_head: Option<ObjectId>,
    pub source: ObjectId,
    pub bases: Vec<ObjectId>,
    /// True when the bases were merged into a virtual one (a criss-cross history).
    pub virtual_base: bool,
    pub outcome: MergeOutcome,
    pub changes: Vec<Change>,
    pub conflicts: Vec<Conflict>,
    /// The commit the merge wrote: the merge commit, or the source's commit for a fast-forward.
    pub commit: Option<ObjectId>,
}

/// A person's chosen content for one path of a merge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    pub path: RepoPath,
    pub content: ObjectId,
    pub file_mode: FileMode,
}

#[derive(Debug, Clone)]
pub struct MergeRequest {
    /// A branch name, or a commit id or abbreviation.
    pub source: String,
    /// The branch that moves; the default branch if `None`.
    pub target: Option<String>,
    pub fast_forward: FastForward,
    pub message: Option<String>,
    /// The commit the caller saw as the target's head; a different head is `MergeStale`.
    pub expect_target: Option<ObjectId>,
    pub resolutions: Vec<Resolution>,
}

/// A merge commit about to be written on a branch.
#[derive(Debug, Clone)]
pub struct MergeWrite {
    /// The branch's head the merge was computed on; the commit's first parent.
    pub expected_head: ObjectId,
    /// The commit merged in; the second parent.
    pub other: ObjectId,
    /// Existing revisions the merge points paths at.
    pub adopted: Vec<Revision>,
    /// New revisions appended to paths' lines.
    pub items: Vec<ChangesetItem>,
    pub author: Person,
    pub message: String,
}

/// What `commit_merge_on` wrote.
#[derive(Debug, Clone)]
pub struct MergeWritten {
    pub commit: ObjectId,
    pub revisions: Vec<Revision>,
}

/// File content, either already stored or still only bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Content {
    Blob(ObjectId),
    Bytes(Vec<u8>),
}

impl Content {
    /// The git blob id of the content.
    pub fn id(&self) -> Result<ObjectId> {
        match self {
            Self::Blob(id) => Ok(*id),
            Self::Bytes(b) => hash_object(REPO_ALGORITHM, Kind::Blob, b)
                .map_err(|e| PynError::Storage(format!("git object: {e}"))),
        }
    }
}

/// The version of a path in a snapshot or a virtual base.
#[derive(Debug, Clone)]
pub struct Entry {
    pub revision: Option<Revision>,
    pub content: Content,
    pub file_mode: FileMode,
}

impl Entry {
    pub fn of(revision: Revision) -> Self {
        Self {
            content: Content::Blob(revision.content),
            file_mode: revision.file_mode,
            revision: Some(revision),
        }
    }

    pub fn version(&self) -> Result<Version> {
        Ok(Version {
            content: self.content.id()?,
            file_mode: self.file_mode,
        })
    }
}

/// A path's content id and git file mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Version {
    pub content: ObjectId,
    pub file_mode: FileMode,
}

/// Which side a merged value comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    Ours,
    Theirs,
    /// Both sides changed it: the lines have to be merged.
    Merge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Take { content: Pick, mode: Pick },
    Conflict(ConflictKind),
}

enum Chosen {
    Side(Pick),
    Both,
}

fn choose<T: PartialEq>(base: Option<&T>, ours: &T, theirs: &T) -> Chosen {
    if ours == theirs || base == Some(theirs) {
        Chosen::Side(Pick::Ours)
    } else if base == Some(ours) {
        Chosen::Side(Pick::Theirs)
    } else {
        Chosen::Both
    }
}

/// The three-way decision for one path from its base, ours and theirs versions (`None` for absent).
pub fn decide(
    base: Option<&Version>,
    ours: Option<&Version>,
    theirs: Option<&Version>,
) -> Decision {
    let (o, t) = match (ours, theirs) {
        (Some(o), Some(t)) => (o, t),
        (None, None) => {
            return Decision::Take {
                content: Pick::Ours,
                mode: Pick::Ours,
            };
        }
        (Some(_), None) | (None, Some(_)) if base.is_some() => {
            return Decision::Conflict(ConflictKind::DeleteModify);
        }
        (Some(_), None) => {
            return Decision::Take {
                content: Pick::Ours,
                mode: Pick::Ours,
            };
        }
        (None, Some(_)) => {
            return Decision::Take {
                content: Pick::Theirs,
                mode: Pick::Theirs,
            };
        }
    };
    let Chosen::Side(mode) = choose(base.map(|b| &b.file_mode), &o.file_mode, &t.file_mode) else {
        return Decision::Conflict(ConflictKind::FileMode);
    };
    match choose(base.map(|b| &b.content), &o.content, &t.content) {
        Chosen::Side(content) => Decision::Take { content, mode },
        Chosen::Both if o.file_mode == FileMode::Symlink || t.file_mode == FileMode::Symlink => {
            Decision::Conflict(ConflictKind::Binary)
        }
        Chosen::Both => Decision::Take {
            content: Pick::Merge,
            mode,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blob(s: &str) -> ObjectId {
        Content::Bytes(s.as_bytes().to_vec()).id().unwrap()
    }

    fn v(s: &str, mode: FileMode) -> Version {
        Version {
            content: blob(s),
            file_mode: mode,
        }
    }

    fn reg(s: &str) -> Version {
        v(s, FileMode::Regular)
    }

    fn take(content: Pick, mode: Pick) -> Decision {
        Decision::Take { content, mode }
    }

    #[test]
    fn a_side_that_did_not_change_yields_to_the_one_that_did() {
        let (b, a, c) = (reg("b"), reg("a"), reg("c"));
        assert_eq!(
            decide(Some(&b), Some(&a), Some(&b)),
            take(Pick::Ours, Pick::Ours)
        );
        assert_eq!(
            decide(Some(&b), Some(&b), Some(&c)),
            take(Pick::Theirs, Pick::Ours)
        );
        assert_eq!(
            decide(Some(&b), Some(&a), Some(&a)),
            take(Pick::Ours, Pick::Ours)
        );
    }

    #[test]
    fn both_sides_changing_content_differently_needs_a_line_merge() {
        let (b, a, c) = (reg("b"), reg("a"), reg("c"));
        assert_eq!(
            decide(Some(&b), Some(&a), Some(&c)),
            take(Pick::Merge, Pick::Ours)
        );
        assert_eq!(
            decide(None, Some(&a), Some(&c)),
            take(Pick::Merge, Pick::Ours)
        );
    }

    #[test]
    fn a_path_only_one_side_added_is_taken_and_a_removal_against_an_edit_conflicts() {
        let (b, a) = (reg("b"), reg("a"));
        assert_eq!(decide(None, Some(&a), None), take(Pick::Ours, Pick::Ours));
        assert_eq!(
            decide(None, None, Some(&a)),
            take(Pick::Theirs, Pick::Theirs)
        );
        assert_eq!(
            decide(Some(&b), Some(&a), None),
            Decision::Conflict(ConflictKind::DeleteModify)
        );
        assert_eq!(
            decide(Some(&b), None, Some(&b)),
            Decision::Conflict(ConflictKind::DeleteModify)
        );
    }

    #[test]
    fn file_modes_merge_independently_of_content() {
        let base = v("x", FileMode::Regular);
        let exec = v("x", FileMode::Executable);
        let edited = v("y", FileMode::Regular);
        assert_eq!(
            decide(Some(&base), Some(&exec), Some(&edited)),
            take(Pick::Theirs, Pick::Ours)
        );
        let link = v("x", FileMode::Symlink);
        assert_eq!(
            decide(Some(&base), Some(&exec), Some(&link)),
            Decision::Conflict(ConflictKind::FileMode)
        );
    }

    #[test]
    fn a_symbolic_link_changed_on_both_sides_is_a_whole_file_conflict() {
        let (b, a, c) = (
            v("b", FileMode::Symlink),
            v("a", FileMode::Symlink),
            v("c", FileMode::Symlink),
        );
        assert_eq!(
            decide(Some(&b), Some(&a), Some(&c)),
            Decision::Conflict(ConflictKind::Binary)
        );
    }
}
