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

    /// The id the record's fields give; what a server-created commit is stored under.
    pub fn expected_id(&self) -> Result<ObjectId> {
        self.git_commit()
            .id(self.tree.algorithm())
            .map_err(object_error)
    }

    /// The record the raw commit bytes describe, without revisions.
    pub fn from_raw(algorithm: Algorithm, raw: Vec<u8>, changeset: Option<String>) -> Result<Self> {
        let parsed = pyn_object::Commit::parse(algorithm, &raw).map_err(object_error)?;
        let id = hash_object(algorithm, Kind::Commit, &raw).map_err(object_error)?;
        let person = |s: &Signature| -> Result<Person> {
            let text = |b: &[u8]| {
                String::from_utf8(b.to_vec())
                    .map_err(|_| PynError::Storage("commit identity is not UTF-8".into()))
            };
            Ok(Person {
                name: text(&s.name)?,
                email: text(&s.email)?,
                offset_minutes: s.offset_minutes,
            })
        };
        let text = |b: &[u8]| {
            String::from_utf8(b.to_vec())
                .map_err(|_| PynError::Storage("commit text is not UTF-8".into()))
        };
        Ok(Self {
            id,
            parents: parsed.parents.clone(),
            tree: parsed.tree,
            author: person(&parsed.author)?,
            committer: person(&parsed.committer)?,
            created_at: Utc
                .timestamp_opt(parsed.committer.time, 0)
                .single()
                .ok_or_else(|| PynError::Storage("commit time out of range".into()))?,
            message: text(&parsed.message)?,
            signature: parsed.gpgsig().map(text).transpose()?,
            changeset,
            raw,
            revisions: Vec::new(),
        })
    }

    /// Checks that the raw bytes hash to `id` and that the record's fields are what they say.
    pub fn verify(&self) -> Result<()> {
        let bad = || PynError::HistoryCorrupt(format!("commit {} does not match its id", self.id));
        let derived =
            Self::from_raw(self.tree.algorithm(), self.raw.clone(), None).map_err(|_| bad())?;
        let message = if self.message.ends_with('\n') {
            self.message.clone()
        } else {
            format!("{}\n", self.message)
        };
        let same = derived.id == self.id
            && derived.tree == self.tree
            && derived.parents == self.parents
            && derived.author == self.author
            && derived.committer == self.committer
            && derived.created_at == self.created_at
            && derived.message == message
            && derived.signature == self.signature;
        if same { Ok(()) } else { Err(bad()) }
    }
}

/// A commit about to be written: its record (without revisions) and the trees it adds.
pub struct CommitDraft {
    pub record: Commit,
    pub trees: Vec<(ObjectId, Vec<u8>)>,
}

/// The commit by `author` at `at` with `message` on top of `parents`, over the tree `build` made.
pub fn draft_commit(
    parents: Vec<ObjectId>,
    build: TreeBuild,
    author: &Person,
    message: &str,
    at: DateTime<Utc>,
    changeset: Option<String>,
) -> Result<CommitDraft> {
    let mut record = Commit {
        id: build.root,
        parents,
        tree: build.root,
        author: author.clone(),
        committer: author.clone(),
        created_at: commit_time(at),
        message: message.to_string(),
        signature: None,
        changeset,
        raw: Vec::new(),
        revisions: Vec::new(),
    };
    record.raw = record.git_commit().encode();
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
