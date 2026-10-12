use std::collections::BTreeMap;

use pyn_object::{FileMode, ObjectId, TreeEntry};

use crate::error::Result;
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

fn join(prefix: &[u8], name: &[u8]) -> Vec<u8> {
    let mut p = prefix.to_vec();
    p.extend_from_slice(name);
    p
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
            self.flatten_into(id, b"", spec, &mut out)?;
        }
        Ok(out)
    }

    fn flatten_into(
        &self,
        id: &ObjectId,
        prefix: &[u8],
        spec: &Pathspec,
        out: &mut BTreeMap<Vec<u8>, Entry>,
    ) -> Result<()> {
        for e in self.tree(id)? {
            let path = join(prefix, &e.name);
            if e.mode.is_tree() {
                if spec.may_contain(&path) {
                    self.flatten_into(&e.id, &join(&path, b"/"), spec, out)?;
                }
            } else if spec.matches(&path) {
                out.insert(
                    path,
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
        self.diff_into(b"", old.copied(), new.copied(), spec, &mut out)?;
        out.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(out)
    }

    fn diff_into(
        &self,
        prefix: &[u8],
        old: Option<ObjectId>,
        new: Option<ObjectId>,
        spec: &Pathspec,
        out: &mut Vec<Change>,
    ) -> Result<()> {
        if old == new {
            return Ok(());
        }
        let load = |id: Option<ObjectId>| -> Result<BTreeMap<Vec<u8>, TreeEntry>> {
            Ok(match id {
                Some(id) => self
                    .tree(&id)?
                    .into_iter()
                    .map(|e| (e.name.clone(), e))
                    .collect(),
                None => BTreeMap::new(),
            })
        };
        let (a, b) = (load(old)?, load(new)?);
        let names: std::collections::BTreeSet<&Vec<u8>> = a.keys().chain(b.keys()).collect();
        for name in names {
            let (x, y) = (a.get(name), b.get(name));
            let path = join(prefix, name);
            let dir = join(&path, b"/");
            let leaf = |e: Option<&TreeEntry>| {
                e.filter(|e| !e.mode.is_tree()).map(|e| Entry {
                    mode: e.mode,
                    id: e.id,
                })
            };
            let tree = |e: Option<&TreeEntry>| e.filter(|e| e.mode.is_tree()).map(|e| e.id);
            let (old_leaf, new_leaf) = (leaf(x), leaf(y));
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
            let (old_tree, new_tree) = (tree(x), tree(y));
            if (old_tree.is_some() || new_tree.is_some()) && spec.may_contain(&path) {
                self.diff_into(&dir, old_tree, new_tree, spec, out)?;
            }
        }
        Ok(())
    }
}
