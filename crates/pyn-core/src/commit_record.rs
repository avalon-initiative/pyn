//! The commit record (decision #245): git-format trees built incrementally and the git commit id.
//!
//! Trees are plain git trees of every file with its blob id and file mode; an exclusive path's revision is not in them.
//! Stores persist the trees a commit adds and hand back raw bodies to `TreeEdit`.

use std::collections::{BTreeSet, HashMap};

use chrono::{DateTime, TimeZone, Utc};
use pyn_object::{
    Algorithm, FileMode, Kind, ObjectId, Signature, TreeEntry, encode_tree, hash_object, parse_tree,
};

use crate::error::{PynError, Result};
use crate::types::{Commit, NewRevision, Person, RepoPath};

fn object_error(e: pyn_object::Error) -> PynError {
    PynError::Storage(format!("git object: {e}"))
}

/// One file to set in a tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeChange {
    pub path: RepoPath,
    pub mode: FileMode,
    pub id: ObjectId,
}

/// The root of a built tree and the trees it added, as `(id, body)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeBuild {
    pub root: ObjectId,
    pub trees: Vec<(ObjectId, Vec<u8>)>,
}

/// Applies file changes to a stored root tree. The caller loops: `wanted` names a tree it must `supply`, until none is
/// left, then `finish` builds the new trees. Only the directories on the changed paths are ever loaded.
pub struct TreeEdit {
    root: Option<ObjectId>,
    changes: Vec<TreeChange>,
    touched: BTreeSet<String>,
    loaded: HashMap<ObjectId, Vec<TreeEntry>>,
}

fn parent_dirs(path: &str) -> impl Iterator<Item = &str> {
    path.match_indices('/').map(|(i, _)| &path[..i])
}

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

impl TreeEdit {
    pub fn new(root: Option<ObjectId>, changes: Vec<TreeChange>) -> Self {
        let mut touched = BTreeSet::new();
        for c in &changes {
            touched.extend(parent_dirs(c.path.as_str()).map(str::to_string));
        }
        Self {
            root,
            changes,
            touched,
            loaded: HashMap::new(),
        }
    }

    /// The next stored tree on a changed path that has not been supplied yet.
    pub fn wanted(&self) -> Option<ObjectId> {
        let mut stack = vec![(self.root?, String::new())];
        while let Some((id, dir)) = stack.pop() {
            let Some(entries) = self.loaded.get(&id) else {
                return Some(id);
            };
            for e in entries.iter().filter(|e| e.mode.is_tree()) {
                let child = join(&dir, &String::from_utf8_lossy(&e.name));
                if self.touched.contains(&child) {
                    stack.push((e.id, child));
                }
            }
        }
        None
    }

    pub fn supply(&mut self, id: ObjectId, body: &[u8]) -> Result<()> {
        let entries = parse_tree(id.algorithm(), body).map_err(object_error)?;
        self.loaded.insert(id, entries);
        Ok(())
    }

    pub fn finish(self) -> Result<TreeBuild> {
        let algorithm = self.algorithm();
        let mut trees = Vec::new();
        let root = self.build("", self.root.as_ref(), algorithm, &mut trees)?;
        Ok(TreeBuild { root, trees })
    }

    fn algorithm(&self) -> Algorithm {
        self.changes
            .first()
            .map(|c| c.id.algorithm())
            .or_else(|| self.root.as_ref().map(ObjectId::algorithm))
            .unwrap_or(Algorithm::Sha256)
    }

    fn build(
        &self,
        dir: &str,
        existing: Option<&ObjectId>,
        algorithm: Algorithm,
        out: &mut Vec<(ObjectId, Vec<u8>)>,
    ) -> Result<ObjectId> {
        let mut entries = match existing {
            Some(id) => self
                .loaded
                .get(id)
                .ok_or_else(|| PynError::Storage(format!("tree {id} was not loaded")))?
                .clone(),
            None => Vec::new(),
        };
        let prefix = if dir.is_empty() {
            String::new()
        } else {
            format!("{dir}/")
        };
        let mut subdirs = BTreeSet::new();
        for c in &self.changes {
            let Some(rest) = c.path.as_str().strip_prefix(&prefix) else {
                continue;
            };
            match rest.split_once('/') {
                None => {
                    if entries
                        .iter()
                        .any(|e| e.name == rest.as_bytes() && e.mode.is_tree())
                    {
                        return Err(PynError::InvalidRequest(format!(
                            "{} is a folder in the repository",
                            c.path
                        )));
                    }
                    entries.retain(|e| e.name != rest.as_bytes());
                    entries.push(TreeEntry {
                        mode: c.mode,
                        name: rest.as_bytes().to_vec(),
                        id: c.id,
                    });
                }
                Some((first, _)) => {
                    subdirs.insert(first.to_string());
                }
            }
        }
        for name in subdirs {
            let current = entries.iter().find(|e| e.name == name.as_bytes());
            if current.is_some_and(|e| !e.mode.is_tree()) {
                return Err(PynError::InvalidRequest(format!(
                    "{prefix}{name} is a file in the repository"
                )));
            }
            let child_existing = current.map(|e| e.id);
            let id = self.build(&join(dir, &name), child_existing.as_ref(), algorithm, out)?;
            entries.retain(|e| e.name != name.as_bytes());
            entries.push(TreeEntry {
                mode: FileMode::Tree,
                name: name.into_bytes(),
                id,
            });
        }
        let body = encode_tree(&entries).map_err(object_error)?;
        let id = hash_object(algorithm, Kind::Tree, &body).map_err(object_error)?;
        out.push((id, body));
        Ok(id)
    }
}

/// The root tree of `files` built from nothing; for checking a stored tree against a snapshot.
pub fn tree_of(files: Vec<TreeChange>) -> Result<TreeBuild> {
    TreeEdit::new(None, files).finish()
}

/// Whole seconds, the resolution of a git commit time.
pub fn commit_time(at: DateTime<Utc>) -> DateTime<Utc> {
    Utc.timestamp_opt(at.timestamp(), 0).single().unwrap_or(at)
}

fn signature(who: &Person, at: DateTime<Utc>) -> Signature {
    Signature {
        name: who.name.clone().into_bytes(),
        email: who.email.clone().into_bytes(),
        time: at.timestamp(),
        offset_minutes: who.offset_minutes,
    }
}

impl Commit {
    /// The git commit this record encodes.
    pub fn git_commit(&self) -> pyn_object::Commit {
        let message = if self.message.ends_with('\n') {
            self.message.clone()
        } else {
            format!("{}\n", self.message)
        };
        pyn_object::Commit {
            tree: self.tree,
            parents: self.parents.clone(),
            author: signature(&self.author, self.created_at),
            committer: signature(&self.committer, self.created_at),
            extra_headers: self
                .signature
                .iter()
                .map(|s| (b"gpgsig".to_vec(), s.clone().into_bytes()))
                .collect(),
            message: message.into_bytes(),
        }
    }

    /// The id the record's fields give; equals `id` unless the record was altered.
    pub fn expected_id(&self) -> Result<ObjectId> {
        self.git_commit()
            .id(self.tree.algorithm())
            .map_err(object_error)
    }

    pub fn verify(&self) -> Result<()> {
        if self.expected_id()? == self.id {
            Ok(())
        } else {
            Err(PynError::HistoryCorrupt(format!(
                "commit {} does not match its id",
                self.id
            )))
        }
    }
}

/// A commit about to be written: its record (without revisions) and the trees it adds.
pub struct CommitDraft {
    pub record: Commit,
    pub trees: Vec<(ObjectId, Vec<u8>)>,
}

/// The commit for `first`'s message and author on top of `parent`, over the tree `build` made.
pub fn draft_commit(
    parent: Option<ObjectId>,
    build: TreeBuild,
    first: &NewRevision,
) -> Result<CommitDraft> {
    let mut record = Commit {
        id: build.root,
        parents: parent.into_iter().collect(),
        tree: build.root,
        author: first.person.clone(),
        committer: first.person.clone(),
        created_at: commit_time(first.created_at),
        message: first.message.clone(),
        signature: None,
        changeset: first.changeset.clone(),
        revisions: Vec::new(),
    };
    record.id = record.expected_id()?;
    Ok(CommitDraft {
        record,
        trees: build.trees,
    })
}

impl NewRevision {
    pub fn tree_change(&self) -> TreeChange {
        TreeChange {
            path: self.path.clone(),
            mode: self.file_mode,
            id: self.content,
        }
    }
}
