use std::fmt;
use std::str::FromStr;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::access_service::SERVER_AUDIT_ID;
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
    PolicyChanged,
    AccountApproved,
    AccountDisabled,
    AccountEnabled,
    OrgCreated,
    OrgDeleted,
    OrgMemberAdded,
    OrgMemberRemoved,
    OrgMemberRoleChanged,
}

impl AuditAction {
    pub const ALL: [AuditAction; 22] = [
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
        AuditAction::PolicyChanged,
        AuditAction::AccountApproved,
        AuditAction::AccountDisabled,
        AuditAction::AccountEnabled,
        AuditAction::OrgCreated,
        AuditAction::OrgDeleted,
        AuditAction::OrgMemberAdded,
        AuditAction::OrgMemberRemoved,
        AuditAction::OrgMemberRoleChanged,
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
            Self::PolicyChanged => "policy_changed",
            Self::AccountApproved => "account_approved",
            Self::AccountDisabled => "account_disabled",
            Self::AccountEnabled => "account_enabled",
            Self::OrgCreated => "org_created",
            Self::OrgDeleted => "org_deleted",
            Self::OrgMemberAdded => "org_member_added",
            Self::OrgMemberRemoved => "org_member_removed",
            Self::OrgMemberRoleChanged => "org_member_role_changed",
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

/// What a log belongs to: a repository, an organization, or the server itself.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AuditScope {
    Repo(RepoId),
    Org(UserId),
    Server,
}

impl AuditScope {
    /// The stable storage key. Neither `@` form can collide with a repository id or a user name.
    pub fn key(&self) -> String {
        match self {
            Self::Repo(id) => id.to_string(),
            Self::Org(name) => format!("@org:{name}"),
            Self::Server => SERVER_AUDIT_ID.to_string(),
        }
    }
}

impl From<&RepoId> for AuditScope {
    fn from(id: &RepoId) -> Self {
        Self::Repo(id.clone())
    }
}

/// Append-only record of administrative and locking events. Events are never changed or removed.
#[async_trait]
pub trait AuditStore: Send + Sync {
    async fn record(&self, scope: &AuditScope, event: NewAuditEvent) -> Result<()>;

    async fn list(&self, scope: &AuditScope, query: &AuditQuery) -> Result<Vec<AuditEvent>>;
}
