//! `git log` order: newest committer time first across all parents, with git's default history simplification
//! when a path limit is given. Commits reachable from a hidden set are skipped, as in `git log ^x y`.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashSet};

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
        };
        for id in start {
            log.push(*id)?;
        }
        Ok(log)
    }
}

impl Log<'_> {
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
        while let Some(q) = self.queue.pop() {
            let parents = q.commit.parents.clone();
            if self.spec.is_empty() {
                for p in parents {
                    self.push(p)?;
                }
                return Ok(Some(LogEntry {
                    id: q.id,
                    commit: q.commit,
                }));
            }
            let mut same_as = None;
            let mut all_same = true;
            for p in &parents {
                if self.touches(&q.commit.tree, Some(p))? {
                    all_same = false;
                } else if !self.hidden.contains(p) {
                    same_as = Some(*p);
                    break;
                }
            }
            if let Some(p) = same_as {
                self.push(p)?;
                continue;
            }
            if all_same && !parents.is_empty() && parents.iter().all(|p| self.hidden.contains(p)) {
                continue;
            }
            let show =
                parents.is_empty() && self.touches(&q.commit.tree, None)? || !parents.is_empty();
            for p in parents {
                self.push(p)?;
            }
            if show {
                return Ok(Some(LogEntry {
                    id: q.id,
                    commit: q.commit,
                }));
            }
        }
        Ok(None)
    }
}

impl Iterator for Log<'_> {
    type Item = Result<LogEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        self.next_entry().transpose()
    }
}
