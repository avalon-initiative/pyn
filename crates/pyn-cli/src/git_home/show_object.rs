//! `pyn show` arguments that are not plain commits: blobs and trees (`<rev>:<path>`), tags and the objects they
//! point at, in the order given.

use std::path::Path;

use anyhow::{Result, bail};
use pyn_object::{FileMode, Kind, ObjectId, Tag};

use super::GitHome;
use super::plumbing::{Resolver, prefix, quote, tree_colon};

pub(super) enum Item {
    Blob(Vec<u8>),
    Tree { name: String, id: ObjectId },
    Tag(Tag),
    Commit(ObjectId),
}

/// The items every argument names, or `None` when one is not a single object or all of them are commits.
pub(super) fn items(
    git: &GitHome,
    cwd: &Path,
    args: &[String],
    separated: bool,
) -> Result<Option<Vec<Item>>> {
    if args.is_empty() {
        return Ok(None);
    }
    let pre = prefix(git, cwd).unwrap_or_default();
    let resolver = Resolver { git, prefix: &pre };
    let mut out = Vec::new();
    for a in args {
        let colon = a.starts_with(':') || tree_colon(a).is_some();
        let Some(mut id) = resolver.object(a)? else {
            return Ok(None);
        };
        if !colon && !separated && git.cwd_has(cwd, a) {
            bail!(
                "ambiguous argument '{a}': both revision and filename; \
                 use '--' to separate paths from revisions"
            );
        }
        loop {
            let Some(object) = git.repo.objects().read_opt(&id)? else {
                return Ok(None);
            };
            match object.kind {
                Kind::Blob => out.push(Item::Blob(object.data)),
                Kind::Tree => out.push(Item::Tree {
                    name: a.clone(),
                    id,
                }),
                Kind::Commit => out.push(Item::Commit(id)),
                Kind::Tag => {
                    let tag = Tag::parse(git.algorithm(), &object.data)?;
                    id = tag.object;
                    out.push(Item::Tag(tag));
                    continue;
                }
            }
            break;
        }
    }
    let commits_only = out.iter().all(|i| matches!(i, Item::Commit(_)));
    Ok((!commits_only).then_some(out))
}

/// The entries of a tree as `show` lists them, one per line, directories with a trailing `/`.
pub(super) fn tree_entries(git: &GitHome, id: &ObjectId) -> Result<String> {
    let mut out = String::new();
    for e in git.repo.tree(id)? {
        let dir = if e.mode == FileMode::Tree { "/" } else { "" };
        out.push_str(&format!("{}{dir}\n", quote(git, &e.name)));
    }
    Ok(out)
}
