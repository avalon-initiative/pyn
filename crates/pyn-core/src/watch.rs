//! Watching a repository: a person's chosen level and the events queued for them (decision #1282).

use std::fmt;
use std::str::FromStr;

use crate::error::{PynError, Result};
use crate::types::{RepoId, UserId};
use crate::webhook::OutboxEvent;

/// What a watcher hears about. No stored level means not watching.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WatchLevel {
    All,
    Locks,
    Ignore,
}

impl WatchLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Locks => "locks",
            Self::Ignore => "ignore",
        }
    }

    /// Whether a watcher at this level is queued an event of `event_type`.
    pub fn wants(self, event_type: &str) -> bool {
        match self {
            Self::All => true,
            Self::Locks => event_type.starts_with("lock."),
            Self::Ignore => false,
        }
    }
}

impl fmt::Display for WatchLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for WatchLevel {
    type Err = PynError;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "all" => Ok(Self::All),
            "locks" => Ok(Self::Locks),
            "ignore" => Ok(Self::Ignore),
            other => Err(PynError::InvalidRequest(format!(
                "unknown watch level {other}: use all, locks or ignore"
            ))),
        }
    }
}

/// An event queued for one watcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    pub id: i64,
    pub user: UserId,
    pub repo: RepoId,
    pub event: OutboxEvent,
}

/// A repository's watcher count and the caller's own level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WatchState {
    pub level: Option<WatchLevel>,
    pub watchers: u64,
}
