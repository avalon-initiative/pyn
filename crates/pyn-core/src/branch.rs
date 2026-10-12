//! Branches: names that point at commits, and the rules for what a name may be.

use chrono::{DateTime, Utc};
use pyn_object::ObjectId;

use crate::error::{PynError, Result};
use crate::types::UserId;

/// The branch every repository starts with; its `.pyn/pyn.toml` is the repository's policy.
pub const DEFAULT_BRANCH: &str = "main";

pub const MAX_BRANCH_NAME_LEN: usize = 200;

/// A name pointing at a commit; `head` is `None` only for the default branch of a repository with no commits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Branch {
    pub name: String,
    pub head: Option<ObjectId>,
    pub default: bool,
    pub created_at: DateTime<Utc>,
    pub created_by: UserId,
}

/// Git's ref-name rules, in the subset pyn uses.
pub fn validate_branch_name(name: &str) -> Result<()> {
    let bad_char = |c: char| {
        c.is_control() || c.is_whitespace() || matches!(c, '~' | '^' | ':' | '?' | '*' | '[' | '\\')
    };
    let invalid = name.is_empty()
        || name.len() > MAX_BRANCH_NAME_LEN
        || name.chars().any(bad_char)
        || name.contains("..")
        || name.contains("@{")
        || name.contains("//")
        || name.starts_with(['-', '/'])
        || name.ends_with(['/', '.'])
        || name.ends_with(".lock")
        || name == "@"
        || name == "HEAD"
        || name
            .split('/')
            .any(|part| part.starts_with('.') || part.ends_with(".lock"));
    if invalid {
        return Err(PynError::InvalidBranchName(name.to_string()));
    }
    Ok(())
}

/// Whether `name` and `other` cannot both exist, because one is a folder of the other.
pub fn conflicts(name: &str, other: &str) -> bool {
    name == other
        || name.strip_prefix(other).is_some_and(|r| r.starts_with('/'))
        || other.strip_prefix(name).is_some_and(|r| r.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_git_ref_rules() {
        for ok in ["main", "feature/a", "fix-1.2", "a_b", "x/y/z", "ünï"] {
            assert!(validate_branch_name(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "-x",
            "/x",
            "x/",
            "x.",
            "a..b",
            "a b",
            "a~1",
            "a^",
            "a:b",
            "a?",
            "a*",
            "a[",
            "a\\b",
            "a@{b",
            "a//b",
            "x.lock",
            "a/.b",
            "a/b.lock/c",
            "@",
            "HEAD",
            "a\tb",
        ] {
            assert!(validate_branch_name(bad).is_err(), "{bad:?}");
        }
        assert!(validate_branch_name(&"x".repeat(201)).is_err());
    }

    #[test]
    fn a_name_cannot_be_a_folder_of_another() {
        assert!(conflicts("a", "a/b"));
        assert!(conflicts("a/b", "a"));
        assert!(conflicts("a", "a"));
        assert!(!conflicts("a", "ab"));
        assert!(!conflicts("a/b", "a/c"));
    }
}
