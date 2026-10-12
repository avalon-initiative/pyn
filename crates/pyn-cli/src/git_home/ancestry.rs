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
}
