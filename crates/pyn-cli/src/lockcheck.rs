//! Client-side view of exclusive paths that lack the signed-in user's lock: a convenience, the server is the authority.

use std::collections::BTreeMap;

use anyhow::Result;
use pyn_proto as api;

use crate::backend::Backend;

/// An exclusive path the user cannot publish: `holder` is the other account holding it, or none when no live lock exists.
pub(crate) struct Gap {
    pub path: String,
    pub holder: Option<String>,
}

impl Gap {
    pub fn describe(&self) -> String {
        match &self.holder {
            Some(h) => format!("{} is locked by {h}", self.path),
            None => format!(
                "{} is exclusive and you hold no live lock on it (never taken, or the lease lapsed)",
                self.path
            ),
        }
    }
}

pub(crate) struct Locks {
    pub me: String,
    pub files: BTreeMap<String, api::FileEntry>,
}

impl Locks {
    pub fn fetch(backend: &dyn Backend) -> Result<Self> {
        Ok(Self {
            me: backend.whoami()?,
            files: backend
                .list_files()?
                .into_iter()
                .map(|e| (e.path.clone(), e))
                .collect(),
        })
    }

    /// The gap of `path`, using `declared` for a path the server does not list yet.
    pub fn gap(&self, path: &str, declared: Option<api::Mode>) -> Option<Gap> {
        let entry = self.files.get(path);
        let mode = entry.map(|e| e.mode).or(declared)?;
        if mode != api::Mode::Exclusive {
            return None;
        }
        let lock = entry.and_then(|e| e.lock.as_ref());
        match lock {
            Some(l) if l.owner == self.me => None,
            Some(l) => Some(Gap {
                path: path.to_string(),
                holder: Some(l.owner.clone()),
            }),
            None => Some(Gap {
                path: path.to_string(),
                holder: None,
            }),
        }
    }
}

/// Prints, for each path, that it can be staged or committed offline but publishing needs the lock; says nothing when
/// the server cannot be asked.
pub(crate) fn note_offline(backend: &dyn Backend, paths: &[&String]) {
    if paths.is_empty() {
        return;
    }
    let Ok(locks) = Locks::fetch(backend) else {
        return;
    };
    for gap in paths.iter().filter_map(|p| locks.gap(p, None)) {
        match &gap.holder {
            Some(h) => eprintln!(
                "note: {} is locked by {h}; you can work on it offline, but a push is refused while they hold it",
                gap.path
            ),
            None => eprintln!(
                "note: {} is exclusive and you do not hold its lock; you can work on it offline, but publishing needs the lock: `pyn lock {}`",
                gap.path, gap.path
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, mode: api::Mode, owner: Option<&str>) -> api::FileEntry {
        api::FileEntry {
            path: path.into(),
            mode,
            revision: Some(1),
            file_mode: api::FileMode::Regular,
            lock: owner.map(|o| api::Lock {
                path: path.into(),
                owner: o.into(),
                identity: None,
                acquired_at: chrono::Utc::now(),
                expires_at: chrono::Utc::now(),
            }),
        }
    }

    fn locks(entries: Vec<api::FileEntry>) -> Locks {
        Locks {
            me: "alice".into(),
            files: entries.into_iter().map(|e| (e.path.clone(), e)).collect(),
        }
    }

    #[test]
    fn only_exclusive_paths_without_my_lock_are_gaps() {
        let l = locks(vec![
            entry("a", api::Mode::Exclusive, None),
            entry("b", api::Mode::Exclusive, Some("bob")),
            entry("c", api::Mode::Exclusive, Some("alice")),
            entry("d", api::Mode::Shared, None),
        ]);
        assert!(l.gap("a", None).is_some_and(|g| g.holder.is_none()));
        assert_eq!(l.gap("b", None).unwrap().holder.as_deref(), Some("bob"));
        assert!(l.gap("c", None).is_none() && l.gap("d", None).is_none());
    }

    #[test]
    fn an_unlisted_path_uses_the_declared_mode() {
        let l = locks(Vec::new());
        assert!(l.gap("x", None).is_none());
        assert!(l.gap("x", Some(api::Mode::Exclusive)).is_some());
        assert!(l.gap("x", Some(api::Mode::Shared)).is_none());
    }
}
