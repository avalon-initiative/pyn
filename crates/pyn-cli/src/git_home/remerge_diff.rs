//! `--remerge-diff`: a two-parent merge commit against a fresh merge of its parents, conflict markers included.

use std::collections::BTreeMap;

use anyhow::Result;
use pyn_object::{Commit, ObjectId};

use super::GitHome;
use super::switch::subject;
use super::treemerge::{Conflicting, Labels, Resolved, merge_trees, result_tree};
use crate::commit_format::short;

/// The tree the parents merge to, and the messages each conflicted path gets.
pub(super) struct Recreated {
    pub tree: ObjectId,
    pub notes: BTreeMap<String, Vec<String>>,
}

fn label(git: &GitHome, id: &ObjectId) -> Result<String> {
    Ok(format!("{} ({})", short(id), subject(git, id)?))
}

fn settings(git: &GitHome) -> super::strategy::Settings {
    let mut settings = super::strategy::Settings::default();
    match git.repo.config().get("merge.conflictstyle") {
        None | Some("merge") => settings.plain_markers = true,
        Some("diff3") => {}
        Some(other) => crate::usage_error(&format!(
            "`merge.conflictStyle={other}` is not available for a remerge diff"
        )),
    }
    settings
}

fn messages(
    path: &str,
    kind: Conflicting,
    (ours, theirs): (&str, &str),
    both: bool,
) -> Vec<String> {
    match kind {
        Conflicting::Content | Conflicting::Binary if both => {
            let mut lines = Vec::new();
            if kind == Conflicting::Binary {
                lines.push(format!(
                    "warning: Cannot merge binary files: {path} ({ours} vs. {theirs})"
                ));
            }
            lines.push(format!("CONFLICT (content): Merge conflict in {path}"));
            lines
        }
        Conflicting::AddAdd => vec![format!("CONFLICT (add/add): Merge conflict in {path}")],
        Conflicting::ModifyDelete { ours_deleted } => {
            let (deleted, kept) = if ours_deleted {
                (ours, theirs)
            } else {
                (theirs, ours)
            };
            vec![format!(
                "CONFLICT (modify/delete): {path} deleted in {deleted} and modified in {kept}.  Version {kept} of {path} left in tree."
            )]
        }
        Conflicting::FileDirectory | Conflicting::Renamed => Vec::new(),
        _ => vec![format!("CONFLICT (content): Merge conflict in {path}")],
    }
}

/// Merges the two parents of `c` again; a merge with several merge bases is refused.
pub(super) fn recreate(git: &GitHome, c: &Commit) -> Result<Recreated> {
    let (a, b) = (c.parents[0], c.parents[1]);
    let bases = git.merge_bases(&a, &b)?;
    if bases.len() > 1 {
        crate::usage_error("a remerge diff of a merge with several merge bases is not available");
    }
    let base_tree = match bases.first() {
        Some(id) => Some(git.repo.commit(id)?.tree),
        None => None,
    };
    let (ours, theirs) = (label(git, &a)?, label(git, &b)?);
    let base = bases.first().map(short).unwrap_or_default();
    let merged = merge_trees(
        git,
        base_tree.as_ref(),
        Some(&git.repo.commit(&a)?.tree),
        Some(&git.repo.commit(&b)?.tree),
        &Labels {
            ours: &ours,
            theirs: &theirs,
            base: &base,
        },
        &settings(git),
    )?;
    let mut notes = BTreeMap::new();
    for (path, resolved) in &merged.paths {
        if let Resolved::Conflict(conflict) = resolved {
            let name = super::path_string(path);
            let both = conflict.ours.is_some() && conflict.theirs.is_some();
            let lines = messages(&name, conflict.kind, (&ours, &theirs), both);
            notes.insert(name, lines);
        }
    }
    Ok(Recreated {
        tree: result_tree(git, &merged)?,
        notes,
    })
}

/// Puts the messages of each conflicted path after its `diff --git` line; a conflicted path whose result equals the
/// fresh merge gets a block of its own.
pub(super) fn annotate(patch: &str, notes: &BTreeMap<String, Vec<String>>, whole: bool) -> String {
    let mut remaining: Vec<(&String, &Vec<String>)> = if whole {
        notes.iter().collect()
    } else {
        Vec::new()
    };
    let block = |path: &str, lines: &[String]| {
        let mut text = format!("diff --git a/{path} b/{path}\n");
        lines
            .iter()
            .for_each(|l| text.push_str(&format!("remerge {l}\n")));
        text
    };
    let mut out = String::with_capacity(patch.len());
    for line in patch.split_inclusive('\n') {
        if let Some(rest) = line.strip_prefix("diff --git a/") {
            let path = rest.trim_end().split_once(" b/").map_or("", |(p, _)| p);
            while let Some(at) = remaining.iter().position(|(p, _)| p.as_str() < path) {
                let (p, lines) = remaining.remove(at);
                out.push_str(&block(p, lines));
            }
            out.push_str(line);
            if let Some(at) = remaining.iter().position(|(p, _)| p.as_str() == path) {
                let (_, lines) = remaining.remove(at);
                lines
                    .iter()
                    .for_each(|l| out.push_str(&format!("remerge {l}\n")));
            }
        } else {
            out.push_str(line);
        }
    }
    for (p, lines) in remaining {
        out.push_str(&block(p, lines));
    }
    out
}
