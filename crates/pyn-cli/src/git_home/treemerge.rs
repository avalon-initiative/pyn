//! A three-way merge of whole trees: each path is taken from the side that changed it, merged line by line when both
//! did (`pyn-merge`), or left as a conflict. Renames are not detected, so a rename on one side and an edit on the
//! other is a conflict where git would merge cleanly. A file against a folder is moved aside as git does when asked to.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, bail};
use pyn_gitwork::{Entry, Pathspec};
use pyn_merge::{
    Favor, Outcome as Text, merge_text_with, render_markers_sized, render_two_way_sized,
};
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
    /// A file moved aside because a folder took its path.
    FileDirectory,
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
    /// The path a file was moved from because a folder took it.
    pub moved_from: Option<Vec<u8>>,
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
            moved_from: None,
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
            let size = pyn_merge::MARKER_SIZE + settings.marker_extra;
            let bytes = if settings.plain_markers {
                render_two_way_sized(&chunks, size, labels.ours, labels.theirs)
            } else {
                render_markers_sized(&chunks, size, (labels.ours, labels.base, labels.theirs))
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
    merge_trees_with(git, [base, ours, theirs], labels, settings, false)
}

/// As `merge_trees`; with `move_files` a file meeting a folder is moved aside as `ort` does instead of refused.
pub(super) fn merge_trees_with(
    git: &GitHome,
    [base, ours, theirs]: [Option<&ObjectId>; 3],
    labels: &Labels,
    settings: &Settings,
    move_files: bool,
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
                    moved_from: None,
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
                    moved_from: None,
                }),
                (None, None) => unreachable!("equal sides were taken above"),
            }
        };
        out.paths.insert(path.clone(), resolved);
    }
    if move_files {
        move_files_aside(git, &mut out, [&b, &o, &t], labels)?;
    } else {
        refuse_file_folder_clashes(&out)?;
    }
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

type Leaves = BTreeMap<Vec<u8>, Entry>;

/// Moves each file that a folder shares its path with to `<path>~<side>` (`_<n>` while taken), as `ort` does.
fn move_files_aside(
    git: &GitHome,
    out: &mut Merge,
    [b, o, t]: [&Leaves; 3],
    labels: &Labels,
) -> Result<()> {
    let live: BTreeSet<Vec<u8>> = out
        .paths
        .iter()
        .filter(|(_, r)| kept(r))
        .map(|(p, _)| p.clone())
        .collect();
    let clashing: BTreeSet<&[u8]> = live
        .iter()
        .flat_map(|p| {
            let folders = p.iter().enumerate().filter(|(_, c)| **c == b'/');
            folders.map(|(i, _)| &p[..i]).collect::<Vec<_>>()
        })
        .filter(|f| live.contains(*f))
        .collect();
    let clashing: Vec<Vec<u8>> = clashing.into_iter().map(<[u8]>::to_vec).collect();
    let mut taken: BTreeSet<Vec<u8>> = b.keys().chain(o.keys()).chain(t.keys()).cloned().collect();
    for path in clashing {
        let (entry, label) = match (o.get(&path), t.get(&path)) {
            (Some(e), None) => (*e, labels.ours),
            (None, Some(e)) => (*e, labels.theirs),
            _ => bail!(
                "'{}' is a file and a folder in the merge; pyn cannot merge that yet, and nothing was changed",
                String::from_utf8_lossy(&path)
            ),
        };
        if entry.mode == FileMode::Gitlink {
            bail!(
                "'{}' is a submodule meeting a folder; pyn cannot merge that yet, and nothing was changed",
                String::from_utf8_lossy(&path)
            );
        }
        let moved = unique_name(&path, label, &taken);
        taken.insert(moved.clone());
        let ours_side = o.contains_key(&path);
        let kind = match out.paths.get(&path) {
            Some(Resolved::Conflict(c)) => c.kind,
            _ => Conflicting::FileDirectory,
        };
        let conflict = Conflict {
            base: b.get(&path).copied(),
            ours: ours_side.then_some(entry),
            theirs: (!ours_side).then_some(entry),
            worktree: Worktree::Write {
                mode: entry.mode,
                bytes: git.repo.blob(&entry.id)?,
            },
            kind,
            moved_from: Some(path.clone()),
        };
        out.paths.insert(path, Resolved::Take(None));
        out.paths.insert(moved, Resolved::Conflict(conflict));
        out.trivial = false;
    }
    Ok(())
}

fn unique_name(path: &[u8], label: &str, taken: &BTreeSet<Vec<u8>>) -> Vec<u8> {
    let mut base = path.to_vec();
    base.push(b'~');
    base.extend(label.bytes().map(|c| if c == b'/' { b'_' } else { c }));
    let used = |name: &[u8]| {
        taken
            .iter()
            .any(|k| k == name || (k.starts_with(name) && k[name.len()] == b'/'))
    };
    let mut name = base.clone();
    let mut n = 0;
    while used(&name) {
        name = base.clone();
        name.extend(format!("_{n}").bytes());
        n += 1;
    }
    name
}
