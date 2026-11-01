//! The base of a merge with several merge bases (a criss-cross): `ort`'s recursive merge of the bases into one tree
//! that is never committed. Content conflicts inside it stay as markers, the way git leaves them.

use anyhow::{Result, bail};
use pyn_object::ObjectId;

use super::GitHome;
use super::strategy::Settings;
use super::treemerge::{Labels, Resolved, merge_trees, result_tree};

const MAX_DEPTH: u8 = 16;
pub(super) const VIRTUAL_LABEL: &str = "merged common ancestors";

/// Merge bases oldest first by committer time, and whether two of them share a time (so git's order is not known).
fn ordered(git: &GitHome, bases: &[ObjectId]) -> Result<(Vec<ObjectId>, bool)> {
    let mut timed = Vec::new();
    for id in bases {
        timed.push((git.repo.commit(id)?.committer.time, *id));
    }
    timed.sort_by_key(|(time, _)| *time);
    let ambiguous = timed.windows(2).any(|w| w[0].0 == w[1].0);
    Ok((timed.into_iter().map(|(_, id)| id).collect(), ambiguous))
}

/// The tree standing for `bases`, which are two or more commits.
pub(super) fn virtual_tree(
    git: &GitHome,
    bases: &[ObjectId],
    settings: &Settings,
    depth: u8,
) -> Result<ObjectId> {
    if depth > MAX_DEPTH {
        bail!("the merge bases nest too deeply for pyn to merge them, and nothing was changed");
    }
    let tree_of = |c: &ObjectId| -> Result<ObjectId> { Ok(git.repo.commit(c)?.tree) };
    let mut inner_settings = settings.clone();
    inner_settings.text.favor = None;
    inner_settings.marker_extra = settings.marker_extra + 2;
    let (order, ambiguous) = ordered(git, bases)?;
    let mut heads = vec![order[0]];
    let mut tree = tree_of(&order[0])?;
    for next in &order[1..] {
        let inner = git.merge_bases_from(&heads, next)?;
        let (base, label) = match inner.as_slice() {
            [] => (None, "empty tree".to_string()),
            [one] => (Some(tree_of(one)?), git.repo.abbreviate(one)?),
            many => (
                Some(virtual_tree(git, many, &inner_settings, depth + 1)?),
                VIRTUAL_LABEL.to_string(),
            ),
        };
        let merged = merge_trees(
            git,
            base.as_ref(),
            Some(&tree),
            Some(&tree_of(next)?),
            &Labels {
                ours: "Temporary merge branch 1",
                theirs: "Temporary merge branch 2",
                base: &label,
            },
            &inner_settings,
        )?;
        if merged
            .paths
            .values()
            .any(|r| matches!(r, Resolved::Conflict(c) if !c.notes.is_empty()))
        {
            bail!(
                "the merge bases conflict over a rename among themselves; pyn cannot merge that yet, and nothing was changed"
            );
        }
        if ambiguous
            && merged
                .paths
                .values()
                .any(|r| matches!(r, Resolved::Conflict(_)))
        {
            bail!(
                "the merge bases conflict among themselves and share a commit time, so pyn cannot tell which git merges first; nothing was changed"
            );
        }
        tree = result_tree(git, &merged)?;
        heads.push(*next);
    }
    Ok(tree)
}
