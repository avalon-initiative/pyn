//! Tags: immutable names that point at a commit. A tag never moves; deleting it frees the name.

use chrono::{DateTime, Utc};
use pyn_object::ObjectId;

use crate::branch::validate_branch_name;
use crate::error::{PynError, Result};
use crate::types::UserId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    pub name: String,
    pub commit: ObjectId,
    pub message: Option<String>,
    pub created_at: DateTime<Utc>,
    pub created_by: UserId,
}

/// Tag names follow the same ref rules as branch names.
pub fn validate_tag_name(name: &str) -> Result<()> {
    validate_branch_name(name).map_err(|_| PynError::InvalidTagName(name.to_string()))
}
