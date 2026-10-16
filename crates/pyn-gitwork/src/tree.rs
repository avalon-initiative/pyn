use std::collections::BTreeMap;

use pyn_object::{FileMode, ObjectId, TreeEntry};

use crate::error::{Error, Result};
use crate::pathspec::Pathspec;
use crate::repo::Repo;

/// A leaf of a tree: a file, a symlink or a submodule commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    pub mode: FileMode,
    pub id: ObjectId,
}

/// One path whose entry differs between two trees; `None` on a side means the path is absent there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub path: Vec<u8>,
    pub old: Option<Entry>,
    pub new: Option<Entry>,
}

/// Deepest directory nesting a tree walk follows.
pub const MAX_TREE_DEPTH: usize = 4096;

fn check_depth(depth: usize) -> Result<()> {
    if depth > MAX_TREE_DEPTH {
        return Err(Error::TooDeep {
            limit: MAX_TREE_DEPTH,
        });
    }
    Ok(())
}

type Level = BTreeMap<Vec<u8>, TreeEntry>;

struct DiffFrame {
    old: Level,
    new: Level,
    names: std::vec::IntoIter<Vec<u8>>,
    base: usize,
}

impl Repo {
    /// Every leaf under `tree` that `spec` selects, by path.
    pub fn flatten(
        &self,
        tree: Option<&ObjectId>,
        spec: &Pathspec,
    ) -> Result<BTreeMap<Vec<u8>, Entry>> {
        let mut out = BTreeMap::new();
        if let Some(id) = tree {
            self.flatten_into(id, spec, &mut out)?;
        }
        Ok(out)
    }

    /// Walks with an explicit stack and one growing path buffer.
    fn flatten_into(
        &self,
        id: &ObjectId,
        spec: &Pathspec,
        out: &mut BTreeMap<Vec<u8>, Entry>,
    ) -> Result<()> {
        let mut path: Vec<u8> = Vec::new();
        let mut stack = vec![(self.tree(id)?.into_iter(), 0usize)];
        while let Some((entries, base)) = stack.last_mut() {
            let base = *base;
            let Some(e) = entries.next() else {
                stack.pop();
                continue;
            };
            path.truncate(base);
            path.extend_from_slice(&e.name);
            if e.mode.is_tree() {
                if spec.may_contain(&path) {
                    check_depth(stack.len())?;
                    path.push(b'/');
                    stack.push((self.tree(&e.id)?.into_iter(), path.len()));
                }
            } else if spec.matches(&path) {
                out.insert(
                    path.clone(),
                    Entry {
                        mode: e.mode,
                        id: e.id,
                    },
                );
            }
        }
        Ok(())
    }

    /// The leaf at `path` under `tree`, if any.
    pub fn entry_at(&self, tree: &ObjectId, path: &[u8]) -> Result<Option<TreeEntry>> {
        let mut current = *tree;
        let mut parts = path.split(|b| *b == b'/').peekable();
        while let Some(part) = parts.next() {
            let Some(found) = self.tree(&current)?.into_iter().find(|e| e.name == part) else {
                return Ok(None);
            };
            if parts.peek().is_none() {
                return Ok(Some(found));
            }
            if !found.mode.is_tree() {
                return Ok(None);
            }
            current = found.id;
        }
        Ok(None)
    }

    /// The leaves that differ between two trees (either may be absent) and that `spec` selects, by path.
    pub fn diff_trees(
        &self,
        old: Option<&ObjectId>,
        new: Option<&ObjectId>,
        spec: &Pathspec,
    ) -> Result<Vec<Change>> {
        let mut out = Vec::new();
        self.diff_into(old.copied(), new.copied(), spec, &mut out)?;
        out.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(out)
    }

    fn load_level(&self, id: Option<ObjectId>) -> Result<Level> {
        Ok(match id {
            Some(id) => self
                .tree(&id)?
                .into_iter()
                .map(|e| (e.name.clone(), e))
                .collect(),
            None => BTreeMap::new(),
        })
    }

    fn diff_frame(
        &self,
        old: Option<ObjectId>,
        new: Option<ObjectId>,
        base: usize,
    ) -> Result<DiffFrame> {
        let (old, new) = (self.load_level(old)?, self.load_level(new)?);
        let names: std::collections::BTreeSet<&Vec<u8>> = old.keys().chain(new.keys()).collect();
        let names: Vec<Vec<u8>> = names.into_iter().cloned().collect();
        Ok(DiffFrame {
            old,
            new,
            names: names.into_iter(),
            base,
        })
    }

    /// Walks with an explicit stack and one growing path buffer.
    fn diff_into(
        &self,
        old: Option<ObjectId>,
        new: Option<ObjectId>,
        spec: &Pathspec,
        out: &mut Vec<Change>,
    ) -> Result<()> {
        if old == new {
            return Ok(());
        }
        let mut path: Vec<u8> = Vec::new();
        let mut stack = vec![self.diff_frame(old, new, 0)?];
        while let Some(frame) = stack.last_mut() {
            let Some(name) = frame.names.next() else {
                stack.pop();
                continue;
            };
            let (x, y) = (frame.old.get(&name), frame.new.get(&name));
            path.truncate(frame.base);
            path.extend_from_slice(&name);
            let leaf = |e: Option<&TreeEntry>| {
                e.filter(|e| !e.mode.is_tree()).map(|e| Entry {
                    mode: e.mode,
                    id: e.id,
                })
            };
            let tree = |e: Option<&TreeEntry>| e.filter(|e| e.mode.is_tree()).map(|e| e.id);
            let (old_leaf, new_leaf) = (leaf(x), leaf(y));
            let (old_tree, new_tree) = (tree(x), tree(y));
            if (old_leaf.is_some() || new_leaf.is_some())
                && spec.matches(&path)
                && old_leaf != new_leaf
            {
                out.push(Change {
                    path: path.clone(),
                    old: old_leaf,
                    new: new_leaf,
                });
            }
            if old_tree != new_tree
                && (old_tree.is_some() || new_tree.is_some())
                && spec.may_contain(&path)
            {
                check_depth(stack.len())?;
                path.push(b'/');
                let next = self.diff_frame(old_tree, new_tree, path.len())?;
                stack.push(next);
            }
        }
        Ok(())
    }
}
