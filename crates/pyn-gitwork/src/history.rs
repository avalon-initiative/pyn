//! `git log` order: newest committer time first across all parents, with git's default history simplification
//! when a path limit is given. Commits reachable from a hidden set are skipped, as in `git log ^x y`.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};

use pyn_object::{Commit, ObjectId};

use crate::error::Result;
use crate::pathspec::Pathspec;
use crate::repo::Repo;

pub struct LogEntry {
    pub id: ObjectId,
    pub commit: Commit,
}

struct Queued {
    time: i64,
    seq: Reverse<u64>,
    id: ObjectId,
    commit: Commit,
}

impl PartialEq for Queued {
    fn eq(&self, other: &Self) -> bool {
        (self.time, &self.seq) == (other.time, &other.seq)
    }
}

impl Eq for Queued {}

impl PartialOrd for Queued {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Queued {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.time, &self.seq).cmp(&(other.time, &other.seq))
    }
}

/// An iterator over the commits reachable from the starting points.
pub struct Log<'a> {
    repo: &'a Repo,
    spec: Pathspec,
    queue: BinaryHeap<Queued>,
    seen: HashSet<ObjectId>,
    hidden: HashSet<ObjectId>,
    next_seq: u64,
    first_parent: bool,
    skipped_to: HashMap<ObjectId, ObjectId>,
}

impl Repo {
    pub fn log(&self, start: &[ObjectId], spec: Pathspec) -> Result<Log<'_>> {
        self.log_hiding(start, HashSet::new(), spec)
    }

    /// Like [`Repo::log`], skipping `hidden`, which must be closed under ancestry.
    pub fn log_hiding(
        &self,
        start: &[ObjectId],
        hidden: HashSet<ObjectId>,
        spec: Pathspec,
    ) -> Result<Log<'_>> {
        let mut log = Log {
            repo: self,
            spec,
            queue: BinaryHeap::new(),
            seen: HashSet::new(),
            hidden,
            next_seq: 0,
            first_parent: false,
            skipped_to: HashMap::new(),
        };
        for id in start {
            log.push(*id)?;
        }
        Ok(log)
    }
}

impl Log<'_> {
    /// Follows only the first parent of each commit, as `--first-parent`.
    pub fn first_parent_only(&mut self) {
        self.first_parent = true;
    }

    /// The commit a parent link stands for once path simplification has skipped the commits that changed nothing.
    pub fn rewritten(&self, mut id: ObjectId) -> ObjectId {
        while let Some(next) = self.skipped_to.get(&id) {
            id = *next;
        }
        id
    }

    fn push(&mut self, id: ObjectId) -> Result<()> {
        if self.hidden.contains(&id) || !self.seen.insert(id) {
            return Ok(());
        }
        let commit = self.repo.commit(&id)?;
        self.next_seq += 1;
        self.queue.push(Queued {
            time: commit.committer.time,
            seq: Reverse(self.next_seq),
            id,
            commit,
        });
        Ok(())
    }

    fn touches(&self, tree: &ObjectId, parent: Option<&ObjectId>) -> Result<bool> {
        let parent_tree = match parent {
            Some(p) => Some(self.repo.commit(p)?.tree),
            None => None,
        };
        Ok(!self
            .repo
            .diff_trees(parent_tree.as_ref(), Some(tree), &self.spec)?
            .is_empty())
    }

    pub fn next_entry(&mut self) -> Result<Option<LogEntry>> {
        while let Some(w) = self.next_walked()? {
            if w.shown {
                return Ok(Some(w.entry));
            }
        }
        Ok(None)
    }

    /// The next commit the walk visits, shown or skipped by path simplification, with the parents the walk follows.
    pub fn next_walked(&mut self) -> Result<Option<Walked>> {
        let Some(q) = self.queue.pop() else {
            return Ok(None);
        };
        let mut parents = q.commit.parents.clone();
        if self.first_parent {
            parents.truncate(1);
        }
        let entry = LogEntry {
            id: q.id,
            commit: q.commit,
        };
        if self.spec.is_empty() {
            for p in &parents {
                self.push(*p)?;
            }
            return Ok(Some(Walked {
                entry,
                shown: true,
                parents,
            }));
        }
        let mut same_as = None;
        let mut all_same = true;
        for p in &parents {
            if self.touches(&entry.commit.tree, Some(p))? {
                all_same = false;
            } else if !self.hidden.contains(p) {
                same_as = Some(*p);
                break;
            }
        }
        if let Some(p) = same_as {
            self.skipped_to.insert(entry.id, p);
            self.push(p)?;
            return Ok(Some(Walked {
                entry,
                shown: false,
                parents: vec![p],
            }));
        }
        if all_same && !parents.is_empty() && parents.iter().all(|p| self.hidden.contains(p)) {
            return Ok(Some(Walked {
                entry,
                shown: false,
                parents,
            }));
        }
        let show =
            parents.is_empty() && self.touches(&entry.commit.tree, None)? || !parents.is_empty();
        for p in &parents {
            self.push(*p)?;
        }
        Ok(Some(Walked {
            entry,
            shown: show,
            parents,
        }))
    }
}

/// A commit the walk visited.
pub struct Walked {
    pub entry: LogEntry,
    pub shown: bool,
    pub parents: Vec<ObjectId>,
}

impl Iterator for Log<'_> {
    type Item = Result<LogEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        self.next_entry().transpose()
    }
}
