//! The `patch` file of a stopped rebase: the stopped commit's change against its first parent, as `git diff-tree -p`
//! prints it (no renames; binary files as `Binary files ... differ`).

use anyhow::Result;
use pyn_gitwork::{Entry, Pathspec};
use pyn_object::{FileMode, ObjectId};

use super::GitHome;
use super::stage::raw_mode;

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
    let abbrev = |entry: Option<&Entry>| {
        entry.map_or_else(
            || abbreviations.short_null(),
            |e| abbreviations.short(&e.id),
        )
    };
    let mut out = String::new();
    for change in &changes {
        let path = super::path_string(&change.path);
        let (old, new) = (change.old.as_ref(), change.new.as_ref());
        out.push_str(&format!("diff --git a/{path} b/{path}\n"));
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
            continue;
        }
        let tail = match (old, new) {
            (Some(o), Some(n)) if o.mode == n.mode => format!(" {:o}", raw_mode(o.mode)),
            _ => String::new(),
        };
        out.push_str(&format!("index {}..{}{tail}\n", abbrev(old), abbrev(new)));
        let (from, to) = (
            old.map_or("/dev/null".into(), |_| format!("a/{path}")),
            new.map_or("/dev/null".into(), |_| format!("b/{path}")),
        );
        let (a, b) = (content(git, old)?, content(git, new)?);
        if pyn_diff::is_binary(&a) || pyn_diff::is_binary(&b) {
            out.push_str(&format!("Binary files {from} and {to} differ\n"));
        } else if let Some(hunks) = pyn_diff::unified_hunks(&a, &b, 3, false) {
            out.push_str(&format!("--- {from}\n+++ {to}\n{hunks}"));
        }
    }
    Ok(out)
}
