//! A three-way merge of whole trees: each path is taken from the side that changed it, merged line by line when both
//! did (`pyn-merge`), or left as a conflict. Renames are not detected, so a rename on one side and an edit on the
//! other is a conflict where git would merge cleanly.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, bail};
use pyn_gitwork::{Entry, Pathspec};
use pyn_merge::{Favor, Outcome as Text, merge_text_with, render_markers, render_two_way};
use pyn_object::{FileMode, Kind, ObjectId};

use super::GitHome;
use super::convert;
use super::stage::write_leaves;
use super::strategy::Settings;

pub(super) struct Labels<'a> {
    pub ours: &'a str,
    pub theirs: &'a str,
    pub base: &'a str,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Conflicting {
    Content,
    Binary,
    AddAdd,
    /// `ours_deleted`: the path is gone on our side and was changed on theirs.
    ModifyDelete {
        ours_deleted: bool,
    },
}

pub(super) enum Worktree {
    /// What is in the work tree stays.
    Keep,
    Write {
        mode: FileMode,
        bytes: Vec<u8>,
    },
}

pub(super) struct Conflict {
    pub base: Option<Entry>,
    pub ours: Option<Entry>,
    pub theirs: Option<Entry>,
    pub worktree: Worktree,
    pub kind: Conflicting,
}

pub(super) enum Resolved {
    /// The path ends as this entry, or is gone.
    Take(Option<Entry>),
    Conflict(Conflict),
}

pub(super) struct Merge {
    pub paths: BTreeMap<Vec<u8>, Resolved>,
    /// Paths both sides changed, which needed a line merge (`Auto-merging` lines).
    pub line_merged: BTreeSet<Vec<u8>>,
    /// Every path is taken whole from one side, which read-tree's plain three-way merge also resolves.
    pub trivial: bool,
}

fn mode_of(b: Option<&Entry>, o: &Entry, t: &Entry) -> Option<FileMode> {
    if o.mode == t.mode {
        return Some(o.mode);
    }
    match b {
        Some(b) if o.mode == b.mode => Some(t.mode),
        Some(b) if t.mode == b.mode => Some(o.mode),
        _ => None,
    }
}

fn regular(mode: FileMode) -> bool {
    matches!(mode, FileMode::Regular | FileMode::Executable)
}

struct Both<'a> {
    path: &'a [u8],
    labels: &'a Labels<'a>,
    settings: &'a Settings,
}

fn both_present(
    git: &GitHome,
    b: Option<&Entry>,
    o: &Entry,
    t: &Entry,
    at: &Both,
    line_merged: &mut bool,
) -> Result<Resolved> {
    let (labels, settings) = (at.labels, at.settings);
    let conflict = |kind, worktree| {
        Ok(Resolved::Conflict(Conflict {
            base: b.copied(),
            ours: Some(*o),
            theirs: Some(*t),
            worktree,
            kind,
        }))
    };
    let kind = if b.is_none() {
        Conflicting::AddAdd
    } else {
        Conflicting::Content
    };
    let Some(mode) = mode_of(b, o, t) else {
        return conflict(kind, Worktree::Keep);
    };
    if o.mode == FileMode::Symlink
        && t.mode == FileMode::Symlink
        && let Some(favor) = settings.text.favor
    {
        let chosen = if favor == Favor::Ours { o } else { t };
        return Ok(Resolved::Take(Some(*chosen)));
    }
    if !regular(o.mode) || !regular(t.mode) || b.is_some_and(|b| !regular(b.mode)) {
        return conflict(kind, Worktree::Keep);
    }
    if o.id == t.id {
        return Ok(Resolved::Take(Some(Entry { mode, id: o.id })));
    }
    if settings.renormalize {
        convert::refuse_renormalize(git, at.path)?;
    }
    *line_merged = true;
    let base = match b {
        Some(b) => git.repo.blob(&b.id)?,
        None => Vec::new(),
    };
    let (ours, theirs) = (git.repo.blob(&o.id)?, git.repo.blob(&t.id)?);
    match merge_text_with(&base, &ours, &theirs, settings.text) {
        Text::Clean(bytes) => {
            let id = git.repo.objects().write(Kind::Blob, &bytes)?;
            Ok(Resolved::Take(Some(Entry { mode, id })))
        }
        Text::Conflicted(chunks) => {
            let bytes = if settings.plain_markers {
                render_two_way(&chunks, labels.ours, labels.theirs)
            } else {
                render_markers(&chunks, labels.ours, labels.base, labels.theirs)
            };
            conflict(kind, Worktree::Write { mode, bytes })
        }
        Text::BinaryConflict => match settings.text.favor {
            Some(favor) => {
                let chosen = if favor == Favor::Ours { o } else { t };
                Ok(Resolved::Take(Some(Entry {
                    mode,
                    id: chosen.id,
                })))
            }
            None => conflict(Conflicting::Binary, Worktree::Keep),
        },
    }
}

/// Merges `theirs` into `ours`, both derived from `base`; nothing is written except the merged blobs.
pub(super) fn merge_trees(
    git: &GitHome,
    base: Option<&ObjectId>,
    ours: Option<&ObjectId>,
    theirs: Option<&ObjectId>,
    labels: &Labels,
    settings: &Settings,
) -> Result<Merge> {
    let spec = Pathspec::default();
    let (b, o, t) = (
        git.repo.flatten(base, &spec)?,
        git.repo.flatten(ours, &spec)?,
        git.repo.flatten(theirs, &spec)?,
    );
    let paths: BTreeSet<&Vec<u8>> = b.keys().chain(o.keys()).chain(t.keys()).collect();
    let mut out = Merge {
        paths: BTreeMap::new(),
        line_merged: BTreeSet::new(),
        trivial: true,
    };
    for path in paths {
        let (pb, po, pt) = (b.get(path), o.get(path), t.get(path));
        out.trivial &=
            (po == pt && po.is_some()) || (po == pb && pt.is_some()) || (pt == pb && po.is_some());
        let resolved = if po == pt {
            Resolved::Take(po.copied())
        } else if po == pb {
            Resolved::Take(pt.copied())
        } else if pt == pb {
            Resolved::Take(po.copied())
        } else {
            match (po, pt) {
                (Some(o), Some(t)) => {
                    let mut merged = false;
                    let at = Both {
                        path,
                        labels,
                        settings,
                    };
                    let r = both_present(git, pb, o, t, &at, &mut merged)?;
                    if merged {
                        out.line_merged.insert(path.clone());
                    }
                    r
                }
                (Some(o), None) => Resolved::Conflict(Conflict {
                    base: pb.copied(),
                    ours: Some(*o),
                    theirs: None,
                    worktree: Worktree::Keep,
                    kind: Conflicting::ModifyDelete {
                        ours_deleted: false,
                    },
                }),
                (None, Some(t)) => Resolved::Conflict(Conflict {
                    base: pb.copied(),
                    ours: None,
                    theirs: Some(*t),
                    worktree: Worktree::Write {
                        mode: t.mode,
                        bytes: git.repo.blob(&t.id)?,
                    },
                    kind: Conflicting::ModifyDelete { ours_deleted: true },
                }),
                (None, None) => unreachable!("equal sides were taken above"),
            }
        };
        out.paths.insert(path.clone(), resolved);
    }
    refuse_file_folder_clashes(&out)?;
    Ok(out)
}

/// The tree `AUTO_MERGE` records: the merge as written to the work tree, conflict markers included.
pub(super) fn result_tree(git: &GitHome, merge: &Merge) -> Result<ObjectId> {
    let mut leaves = BTreeMap::new();
    for (path, resolved) in &merge.paths {
        let entry = match resolved {
            Resolved::Take(e) => *e,
            Resolved::Conflict(c) => match &c.worktree {
                Worktree::Write { mode, bytes } => Some(Entry {
                    mode: *mode,
                    id: git.repo.objects().write(Kind::Blob, bytes)?,
                }),
                Worktree::Keep => c.ours.or(c.theirs),
            },
        };
        if let Some(e) = entry {
            leaves.insert(path.clone(), e);
        }
    }
    write_leaves(git, &leaves)
}

fn kept(r: &Resolved) -> bool {
    match r {
        Resolved::Take(e) => e.is_some(),
        Resolved::Conflict(_) => true,
    }
}

/// A path that ends as a file while another ends below it as a folder cannot be written; git resolves those with
/// renames, which pyn does not do yet.
fn refuse_file_folder_clashes(merge: &Merge) -> Result<()> {
    let live: BTreeSet<&[u8]> = merge
        .paths
        .iter()
        .filter(|(_, r)| kept(r))
        .map(|(p, _)| p.as_slice())
        .collect();
    for path in &live {
        let folders = path
            .iter()
            .enumerate()
            .filter(|(_, c)| **c == b'/')
            .map(|(i, _)| &path[..i]);
        for folder in folders {
            if live.contains(folder) {
                bail!(
                    "'{}' is a file on one side and a folder on the other; pyn cannot merge that yet, and nothing was changed",
                    String::from_utf8_lossy(folder)
                );
            }
        }
    }
    Ok(())
}
