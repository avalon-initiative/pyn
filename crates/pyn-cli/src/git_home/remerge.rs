//! `restore --merge`: the conflicted merge of one unmerged path, redone from the index stages.

use anyhow::Result;
use pyn_gitwork::Entry;
use pyn_merge::{Outcome, merge_text, render_markers};
use pyn_object::FileMode;

use super::{GitHome, path_string};

/// The mode (ours) and content the work tree gets: a clean merge, or text with diff3 markers; binary content is ours.
pub(super) fn recreate(
    git: &GitHome,
    path: &[u8],
    base: Option<&Entry>,
    ours: &Entry,
    theirs: &Entry,
) -> Result<(FileMode, Vec<u8>)> {
    let base_bytes = match base {
        Some(b) => git.repo.blob(&b.id)?,
        None => Vec::new(),
    };
    let (o, t) = (git.repo.blob(&ours.id)?, git.repo.blob(&theirs.id)?);
    let bytes = match merge_text(&base_bytes, &o, &t) {
        Outcome::Clean(merged) => merged,
        Outcome::Conflicted(chunks) => render_markers(&chunks, "ours", "base", "theirs"),
        Outcome::BinaryConflict => {
            eprintln!(
                "warning: Cannot merge binary files: {} (ours vs. theirs)",
                path_string(path)
            );
            o
        }
    };
    Ok((ours.mode, bytes))
}
