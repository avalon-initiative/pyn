use std::fmt;
use std::str::FromStr;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{PynError, Result};
use crate::types::{RepoId, RepoPath, UserId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditAction {
    Checkout,
    Release,
    Checkin,
    Restore,
    ForceUnlock,
    MemberAdded,
    RoleChanged,
    RolePermissionsChanged,
    TokenCreated,
    TokenRevoked,
    RepoCreated,
    RepoUpdated,
    RepoDeleted,
}

impl AuditAction {
    pub const ALL: [AuditAction; 13] = [
        AuditAction::Checkout,
        AuditAction::Release,
        AuditAction::Checkin,
        AuditAction::Restore,
        AuditAction::ForceUnlock,
        AuditAction::MemberAdded,
        AuditAction::RoleChanged,
        AuditAction::RolePermissionsChanged,
        AuditAction::TokenCreated,
        AuditAction::TokenRevoked,
        AuditAction::RepoCreated,
        AuditAction::RepoUpdated,
        AuditAction::RepoDeleted,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Checkout => "checkout",
            Self::Release => "release",
            Self::Checkin => "checkin",
            Self::Restore => "restore",
            Self::ForceUnlock => "force_unlock",
            Self::MemberAdded => "member_added",
            Self::RoleChanged => "role_changed",
            Self::RolePermissionsChanged => "role_permissions_changed",
            Self::TokenCreated => "token_created",
            Self::TokenRevoked => "token_revoked",
            Self::RepoCreated => "repo_created",
            Self::RepoUpdated => "repo_updated",
            Self::RepoDeleted => "repo_deleted",
        }
    }
}

impl fmt::Display for AuditAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for AuditAction {
    type Err = PynError;
    fn from_str(s: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|a| a.as_str() == s)
            .ok_or_else(|| PynError::InvalidRequest(format!("unknown audit action {s:?}")))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEvent {
    pub id: i64,
    pub at: DateTime<Utc>,
    pub actor: UserId,
    pub action: AuditAction,
    pub path: Option<RepoPath>,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAuditEvent {
    pub at: DateTime<Utc>,
    pub actor: UserId,
    pub action: AuditAction,
    pub path: Option<RepoPath>,
    pub detail: String,
}

/// Which events to list: all matching filters, newest first, starting below `before`.
#[derive(Debug, Clone, Default)]
pub struct AuditQuery {
    pub path: Option<RepoPath>,
    pub actor: Option<UserId>,
    pub action: Option<AuditAction>,
    pub before: Option<i64>,
    pub limit: usize,
}

/// Append-only record of administrative and locking events. Events are never changed or removed.
#[async_trait]
pub trait AuditStore: Send + Sync {
    async fn record(&self, repo: &RepoId, event: NewAuditEvent) -> Result<()>;

    async fn list(&self, repo: &RepoId, query: &AuditQuery) -> Result<Vec<AuditEvent>>;
}
