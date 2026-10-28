//! `pyn show <rev>:<path>` on a git repository: a blob's content or a tree's listing.

use std::path::Path;

use anyhow::Result;
use pyn_object::{FileMode, Kind, ObjectId};

use super::GitHome;
use super::plumbing::{Resolver, emit_bytes, prefix, quote, tree_colon};

/// Shows every argument when all of them name a blob or tree by `<rev>:<path>` or `:<path>`; false otherwise.
pub(super) fn show_objects(git: &GitHome, cwd: &Path, args: &[String]) -> Result<bool> {
    if args.is_empty()
        || !args
            .iter()
            .all(|a| a.starts_with(':') || tree_colon(a).is_some())
    {
        return Ok(false);
    }
    let pre = prefix(git, cwd).unwrap_or_default();
    let resolver = Resolver { git, prefix: &pre };
    let mut found: Vec<(&String, ObjectId)> = Vec::new();
    for a in args {
        match resolver.object(a)? {
            Some(id) => found.push((a, id)),
            None => return Ok(false),
        }
    }
    let mut out = Vec::new();
    for (name, id) in &found {
        let Some(object) = git.repo.objects().read_opt(id)? else {
            return Ok(false);
        };
        match object.kind {
            Kind::Blob => out.extend_from_slice(&object.data),
            Kind::Tree => {
                out.extend_from_slice(format!("tree {name}\n\n").as_bytes());
                for e in git.repo.tree(id)? {
                    let dir = if e.mode == FileMode::Tree { "/" } else { "" };
                    out.extend_from_slice(format!("{}{dir}\n", quote(git, &e.name)).as_bytes());
                }
            }
            _ => return Ok(false),
        }
    }
    emit_bytes(&out);
    Ok(true)
}
