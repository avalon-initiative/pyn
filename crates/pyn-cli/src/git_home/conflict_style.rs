//! `merge.conflictStyle`: how a conflicted text merge is written to the work tree.

use pyn_merge::{Chunk, render_markers_sized, render_two_way_sized, zealous_diff3};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Style {
    Merge,
    Diff3,
    Zdiff3,
}

impl Style {
    /// The configured style; unset or unknown is git's two-sided default.
    pub fn of(config: &pyn_git::Config) -> Self {
        match config
            .get("merge.conflictstyle")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "diff3" => Self::Diff3,
            "zdiff3" => Self::Zdiff3,
            _ => Self::Merge,
        }
    }

    /// Markers `size` wide labelled `(ours, base, theirs)`; the base label is used only by the diff3 styles. Lines both sides
    /// share at the edges of a conflict stay out of it, except under `diff3`.
    pub fn render(
        self,
        chunks: Vec<Chunk<&[u8]>>,
        size: usize,
        (ours, base, theirs): (&str, &str, &str),
    ) -> Vec<u8> {
        match self {
            Self::Merge => render_two_way_sized(&zealous_diff3(chunks), size, ours, theirs),
            Self::Diff3 => render_markers_sized(&chunks, size, (ours, base, theirs)),
            Self::Zdiff3 => {
                render_markers_sized(&zealous_diff3(chunks), size, (ours, base, theirs))
            }
        }
    }
}
