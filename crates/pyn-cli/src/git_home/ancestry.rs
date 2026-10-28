//! Reachability between commits: whether one is an ancestor of another, and their merge bases.

use std::collections::{HashSet, VecDeque};

use anyhow::Result;
use pyn_object::ObjectId;

use super::GitHome;

impl GitHome {
    /// Whether `ancestor` is `descendant` or one of its ancestors.
    pub(super) fn is_ancestor(&self, ancestor: &ObjectId, descendant: &ObjectId) -> Result<bool> {
        let mut seen = HashSet::new();
        let mut queue = VecDeque::from([*descendant]);
        while let Some(id) = queue.pop_front() {
            if id == *ancestor {
                return Ok(true);
            }
            if !seen.insert(id) {
                continue;
            }
            queue.extend(self.repo.commit(&id)?.parents);
        }
        Ok(false)
    }

    /// Every commit reachable from `start`, `start` included.
    pub(super) fn reachable(&self, start: &ObjectId) -> Result<HashSet<ObjectId>> {
        let mut seen = HashSet::new();
        let mut queue = VecDeque::from([*start]);
        while let Some(id) = queue.pop_front() {
            if !seen.insert(id) {
                continue;
            }
            queue.extend(self.repo.commit(&id)?.parents);
        }
        Ok(seen)
    }

    /// The best common ancestors of `a` and `b`: those that no other common ancestor descends from.
    pub(super) fn merge_bases(&self, a: &ObjectId, b: &ObjectId) -> Result<Vec<ObjectId>> {
        self.merge_bases_from(&[*a], b)
    }

    /// As `merge_bases`, with the first side being everything reachable from any of `heads`.
    pub(super) fn merge_bases_from(
        &self,
        heads: &[ObjectId],
        b: &ObjectId,
    ) -> Result<Vec<ObjectId>> {
        let mut from_a = HashSet::new();
        for head in heads {
            from_a.extend(self.reachable(head)?);
        }
        let mut candidates: Vec<ObjectId> = Vec::new();
        let mut seen = HashSet::new();
        let mut queue = VecDeque::from([*b]);
        while let Some(id) = queue.pop_front() {
            if !seen.insert(id) {
                continue;
            }
            if from_a.contains(&id) {
                candidates.push(id);
                continue;
            }
            queue.extend(self.repo.commit(&id)?.parents);
        }
        let mut best = Vec::new();
        for (i, c) in candidates.iter().enumerate() {
            let mut covered = false;
            for (j, other) in candidates.iter().enumerate() {
                if i != j && self.is_ancestor(c, other)? {
                    covered = true;
                    break;
                }
            }
            if !covered {
                best.push(*c);
            }
        }
        best.sort_by_key(ObjectId::to_hex);
        Ok(best)
    }
}
