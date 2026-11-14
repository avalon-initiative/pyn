//! Account deletion: the signed-in user's own request and status, and an administrator's deletion of a disabled account.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Schedules the deletion of the signed-in account after the recovery window.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DeleteOwnAccountRequest {
    /// The account's password, entered again. An account with no password needs a web session and may leave it empty.
    #[serde(default)]
    pub password: String,
}

/// Whether the signed-in account is scheduled for deletion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AccountDeletionStatus {
    pub user: String,
    /// True while the account is disabled and waiting to be deleted.
    pub pending: bool,
    /// When the deletion was requested; null while nothing is scheduled.
    #[schema(required = true)]
    pub scheduled_at: Option<DateTime<Utc>>,
    /// When the account is deleted unless it is restored first; null while nothing is scheduled.
    #[schema(required = true)]
    pub purge_at: Option<DateTime<Utc>>,
}

/// Deletes a disabled account.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AdminDeleteAccountRequest {
    /// Recorded in the audit log; required.
    pub reason: String,
    /// Delete now instead of after the recovery window.
    #[serde(default)]
    pub immediately: bool,
}

/// What an administrator's deletion did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AdminDeleteAccountResult {
    pub user: String,
    /// True when the account is gone; false when its deletion is scheduled.
    pub deleted: bool,
    /// When a scheduled account is deleted; null once it is gone.
    #[schema(required = true)]
    pub purge_at: Option<DateTime<Utc>>,
}
