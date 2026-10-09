//! Repository registry types: who owns a repository, what it is called and how it is configured.

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};

use crate::error::{PynError, Result};
use crate::types::{RepoId, UserId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Visibility {
    Public,
    Private,
}

impl Visibility {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Private => "private",
        }
    }
}

impl fmt::Display for Visibility {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Visibility {
    type Err = PynError;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "public" => Ok(Self::Public),
            "private" => Ok(Self::Private),
            _ => Err(PynError::InvalidRequest(format!(
                "unknown visibility {s:?}: use public or private"
            ))),
        }
    }
}

pub const DEFAULT_LEASE_HOURS: u32 = 8;
pub const MAX_LEASE_HOURS: u32 = 24 * 30;
pub const DEFAULT_MAX_LOCKS_PER_USER: u32 = 5;
pub const MAX_LOCKS_PER_USER_CEILING: u32 = 10_000;

/// A lock limit is 1 to `MAX_LOCKS_PER_USER_CEILING`.
pub fn validate_max_locks(limit: u32) -> Result<u32> {
    if (1..=MAX_LOCKS_PER_USER_CEILING).contains(&limit) {
        Ok(limit)
    } else {
        Err(PynError::InvalidRequest(format!(
            "a lock limit is 1 to {MAX_LOCKS_PER_USER_CEILING}"
        )))
    }
}

/// Per-repository configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RepoSettings {
    pub lease_hours: u32,
    /// Locks one user may hold here; `None` follows the server default. The policy file overrides it.
    pub max_locks: Option<u32>,
}

impl Default for RepoSettings {
    fn default() -> Self {
        Self {
            lease_hours: DEFAULT_LEASE_HOURS,
            max_locks: None,
        }
    }
}

impl RepoSettings {
    pub fn validate(self) -> Result<Self> {
        if !(1..=MAX_LEASE_HOURS).contains(&self.lease_hours) {
            return Err(PynError::InvalidRequest(format!(
                "a lease is 1 to {MAX_LEASE_HOURS} hours"
            )));
        }
        self.max_locks.map(validate_max_locks).transpose()?;
        Ok(self)
    }
}

/// A registered repository. `id` is the opaque key for its locks, revisions, roles and audit log and never
/// changes; `owner` and `name` are the address and can be renamed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoRecord {
    pub id: RepoId,
    pub owner: UserId,
    pub name: String,
    pub visibility: Visibility,
    pub settings: RepoSettings,
    pub created_at: DateTime<Utc>,
}

impl RepoRecord {
    pub fn address(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }
}

/// Fields to change; `None` leaves a field as it is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoUpdate {
    pub name: Option<String>,
    pub visibility: Option<Visibility>,
    pub settings: Option<RepoSettings>,
}

pub const MAX_NAME_LEN: usize = 100;

/// Lowercase letters, digits, `-`, `_` and `.`, 1 to 100 characters, starting with a letter or digit and
/// not ending in `.`.
pub fn validate_name(name: &str) -> Result<String> {
    let ok_chars = name
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_' | b'.'));
    let starts_ok = name
        .bytes()
        .next()
        .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    if (1..=MAX_NAME_LEN).contains(&name.len()) && ok_chars && starts_ok && !name.ends_with('.') {
        Ok(name.to_string())
    } else {
        Err(PynError::InvalidRepoName(name.to_string()))
    }
}

/// A fresh opaque repository id.
pub fn generate_id() -> Result<RepoId> {
    Ok(RepoId::new(format!(
        "r{}",
        crate::access::token::random_hex(12)?
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_lowercase_and_path_safe() {
        for ok in ["game", "a", "my-repo_2.0", "0day"] {
            assert!(validate_name(ok).is_ok(), "{ok}");
        }
        for bad in ["", "Game", "-x", ".x", "x.", "a/b", "a b", &"x".repeat(101)] {
            assert!(validate_name(bad).is_err(), "{bad:?}");
        }
    }
}
