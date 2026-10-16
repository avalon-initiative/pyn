//! Local commits on the store of a workspace with `local_store` on: the commit is recorded in the store as git
//! encodes it and is not sent to the server.

use std::collections::{BTreeMap, HashSet, VecDeque};

use anyhow::{Context, Result};
use pyn_git::Expect;
use pyn_gitwork::{Entry, Pathspec};
use pyn_object::{Commit, Kind, ObjectId, Signature, hash_object};
use pyn_proto as api;

use crate::git_home::clean_message;
use crate::local_store::{ALGORITHM, Files, LocalStore, build_trees, file_mode};

/// A staged change: the new mode and content of a path, or `None` for its removal.
pub struct Staged {
    pub path: String,
    pub content: Option<(api::FileMode, Vec<u8>)>,
}

pub struct Made {
    pub id: ObjectId,
    pub root: bool,
    pub changed: usize,
}

/// Local commits the server lacks, and the paths they changed.
#[derive(Default)]
pub struct Unpublished {
    pub ahead: usize,
    pub paths: BTreeMap<String, Change>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Added,
    Modified,
    Deleted,
}

impl Change {
    pub fn word(self) -> &'static str {
        match self {
            Change::Added => "added",
            Change::Modified => "modified",
            Change::Deleted => "deleted",
        }
    }
}

fn local_ref(branch: &str) -> String {
    format!("refs/heads/{branch}")
}

fn tracking_ref(branch: &str) -> String {
    format!("refs/remotes/origin/{branch}")
}

fn clean(text: &str) -> Vec<u8> {
    text.chars()
        .filter(|c| !matches!(c, '<' | '>' | '\n' | '\0'))
        .collect::<String>()
        .trim()
        .as_bytes()
        .to_vec()
}

/// The signature the server gives an account without an email, at the current time.
pub fn signature(user: &str) -> Signature {
    let name = clean(user);
    let mut email = name.clone();
    email.extend_from_slice(b"@pyn.invalid");
    Signature {
        name: if name.is_empty() {
            b"unknown".to_vec()
        } else {
            name
        },
        email,
        time: chrono::Utc::now().timestamp(),
        offset_minutes: chrono::Local::now().offset().local_minus_utc() / 60,
    }
}

impl LocalStore {
    /// The local branch's tip, if it has one.
    pub fn tip(&self, branch: &str) -> Result<Option<ObjectId>> {
        Ok(self.repo().refs().resolve(&local_ref(branch))?)
    }

    pub(crate) fn published(&self, branch: &str) -> Result<Option<ObjectId>> {
        Ok(self.repo().refs().resolve(&tracking_ref(branch))?)
    }

    /// Records the staged changes as one commit on the local branch; `None` when they change nothing.
    pub fn commit_staged(
        &self,
        branch: &str,
        staged: &[Staged],
        message: &str,
        who: &Signature,
    ) -> Result<Option<Made>> {
        let repo = self.repo();
        let parent = self.tip(branch)?;
        let parent_tree = match &parent {
            Some(p) => Some(repo.commit(p)?.tree),
            None => None,
        };
        let mut files: Files = repo.flatten(parent_tree.as_ref(), &Pathspec::default())?;
        let objects = repo.objects();
        for s in staged {
            let path = s.path.clone().into_bytes();
            match &s.content {
                Some((mode, bytes)) => {
                    let id = objects.write(Kind::Blob, bytes)?;
                    files.insert(
                        path,
                        Entry {
                            mode: file_mode(*mode),
                            id,
                        },
                    );
                }
                None => {
                    files.remove(&path);
                }
            }
        }
        let (tree, trees) = build_trees(&files)?;
        if parent_tree == Some(tree) || (parent_tree.is_none() && files.is_empty()) {
            return Ok(None);
        }
        for (id, body) in trees {
            let written = objects.write(Kind::Tree, &body)?;
            debug_assert_eq!(written, id);
        }
        let record = Commit {
            tree,
            parents: parent.into_iter().collect(),
            author: who.clone(),
            committer: who.clone(),
            extra_headers: Vec::new(),
            message: clean_message(&[message.to_string()]),
        };
        let body = record.encode(ALGORITHM)?;
        let id = hash_object(ALGORITHM, Kind::Commit, &body)?;
        objects.write(Kind::Commit, &body)?;
        let expect = parent.map_or(Expect::Absent, Expect::Id);
        repo.refs()
            .update_checked(&local_ref(branch), expect, &id)
            .context("the local branch moved while committing; run the command again")?;
        let changed = repo
            .diff_trees(parent_tree.as_ref(), Some(&tree), &Pathspec::default())?
            .len();
        Ok(Some(Made {
            id,
            root: parent.is_none(),
            changed,
        }))
    }

    /// The local commits the server's tip does not have, and the paths whose content they changed in all.
    pub fn unpublished(&self, branch: &str) -> Result<Unpublished> {
        let repo = self.repo();
        let (Some(tip), published) = (self.tip(branch)?, self.published(branch)?) else {
            return Ok(Unpublished::default());
        };
        if Some(tip) == published {
            return Ok(Unpublished::default());
        }
        let mut known = HashSet::new();
        let mut queue: VecDeque<ObjectId> = published.into_iter().collect();
        while let Some(id) = queue.pop_front() {
            if known.insert(id) {
                queue.extend(repo.commit(&id)?.parents);
            }
        }
        let (mut ahead, mut at) = (0, Some(tip));
        let mut base = None;
        while let Some(id) = at {
            if known.contains(&id) {
                base = Some(id);
                break;
            }
            ahead += 1;
            at = repo.commit(&id)?.parents.first().copied();
        }
        if ahead == 0 {
            return Ok(Unpublished::default());
        }
        let tree_of = |id: Option<ObjectId>| -> Result<Option<ObjectId>> {
            Ok(match id {
                Some(id) => Some(repo.commit(&id)?.tree),
                None => None,
            })
        };
        let changes = repo.diff_trees(
            tree_of(base)?.as_ref(),
            tree_of(Some(tip))?.as_ref(),
            &Pathspec::default(),
        )?;
        let paths = changes
            .iter()
            .map(|c| {
                let change = match (&c.old, &c.new) {
                    (None, _) => Change::Added,
                    (_, None) => Change::Deleted,
                    _ => Change::Modified,
                };
                (String::from_utf8_lossy(&c.path).into_owned(), change)
            })
            .collect();
        Ok(Unpublished { ahead, paths })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_signature_is_the_servers_placeholder_identity() {
        let s = signature("al<i>ce\n");
        assert_eq!(s.name, b"alice");
        assert_eq!(s.email, b"alice@pyn.invalid");
        assert_eq!(signature("<>").name, b"unknown");
    }

    #[test]
    fn changes_are_named_for_status() {
        assert_eq!(Change::Added.word(), "added");
        assert_eq!(Change::Deleted.word(), "deleted");
    }
}
