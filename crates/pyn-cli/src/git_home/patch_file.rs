//! The `patch` file of a stopped rebase: the stopped commit's change against its first parent, as `git diff-tree -p`
//! prints it (no renames; binary files as `Binary files ... differ`).

use anyhow::Result;
use pyn_gitwork::{Change, Entry, Pathspec};
use pyn_object::{FileMode, ObjectId};

use super::GitHome;
use super::stage::raw_mode;
use crate::backend::Backend;
use crate::diffstat::Counts;

fn content(git: &GitHome, entry: Option<&Entry>) -> Result<Vec<u8>> {
    Ok(match entry {
        Some(e) if e.mode == FileMode::Gitlink => {
            format!("Subproject commit {}\n", e.id).into_bytes()
        }
        Some(e) => git.repo.blob(&e.id)?,
        None => Vec::new(),
    })
}

/// The patch text for the commit; empty for a root commit.
pub(super) fn of_commit(git: &GitHome, id: &ObjectId) -> Result<String> {
    let commit = git.repo.commit(id)?;
    let Some(parent) = commit.parents.first() else {
        return Ok(String::new());
    };
    let parent_tree = git.repo.commit(parent)?.tree;
    let changes =
        git.repo
            .diff_trees(Some(&parent_tree), Some(&commit.tree), &Pathspec::default())?;
    let abbreviations = git.repo.abbreviations(None)?;
    let mut out = String::new();
    for change in &changes {
        out.push_str(&of_change(git, change, 3, &abbreviations)?.text);
    }
    Ok(out)
}

/// One path's patch and its line counts (`Binary` sizes when the content is binary).
pub(super) struct Rendered {
    pub text: String,
    pub counts: Counts,
}

/// The `diff --git` section for one change with `context` lines around each hunk; names are quoted as git does.
pub(super) fn of_change(
    git: &GitHome,
    change: &Change,
    context: usize,
    abbreviations: &pyn_gitwork::Abbreviations,
) -> Result<Rendered> {
    let abbrev = |entry: Option<&Entry>| {
        entry.map_or_else(
            || abbreviations.short_null(),
            |e| abbreviations.short(&e.id),
        )
    };
    let q = |prefix: &str| {
        let mut full = prefix.as_bytes().to_vec();
        full.extend_from_slice(&change.path);
        git.quote_diff_path(&full)
    };
    let (a_name, b_name) = (q("a/"), q("b/"));
    let (old, new) = (change.old.as_ref(), change.new.as_ref());
    let mut out = format!("diff --git {a_name} {b_name}\n");
    match (old, new) {
        (None, Some(n)) => out.push_str(&format!("new file mode {:o}\n", raw_mode(n.mode))),
        (Some(o), None) => out.push_str(&format!("deleted file mode {:o}\n", raw_mode(o.mode))),
        (Some(o), Some(n)) if o.mode != n.mode => out.push_str(&format!(
            "old mode {:o}\nnew mode {:o}\n",
            raw_mode(o.mode),
            raw_mode(n.mode)
        )),
        _ => {}
    }
    let same_id = old.zip(new).is_some_and(|(o, n)| o.id == n.id);
    if same_id {
        return Ok(Rendered {
            text: out,
            counts: Counts::Lines(0, 0),
        });
    }
    let tail = match (old, new) {
        (Some(o), Some(n)) if o.mode == n.mode => format!(" {:o}", raw_mode(o.mode)),
        _ => String::new(),
    };
    out.push_str(&format!("index {}..{}{tail}\n", abbrev(old), abbrev(new)));
    let spaced = |name: &str| {
        if change.path.contains(&b' ') {
            format!("{name}\t")
        } else {
            name.to_string()
        }
    };
    let (from, to) = (
        old.map_or("/dev/null".into(), |_| spaced(&a_name)),
        new.map_or("/dev/null".into(), |_| spaced(&b_name)),
    );
    let (a, b) = (content(git, old)?, content(git, new)?);
    if pyn_diff::is_binary(&a) || pyn_diff::is_binary(&b) {
        out.push_str(&format!(
            "Binary files {} and {} differ\n",
            from.trim_end_matches('\t'),
            to.trim_end_matches('\t')
        ));
        return Ok(Rendered {
            text: out,
            counts: Counts::Binary(a.len(), b.len()),
        });
    }
    if let Some(hunks) = pyn_diff::unified_hunks(&a, &b, context, false) {
        out.push_str(&format!("--- {from}\n+++ {to}\n{hunks}"));
    }
    let (added, deleted) = pyn_diff::line_counts(&a, &b, false);
    Ok(Rendered {
        text: out,
        counts: Counts::Lines(added, deleted),
    })
}
