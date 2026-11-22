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
    /// The recovery window in days: how long after scheduling a deletion can still be undone.
    pub window_days: i64,
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

/// What stops an account from being deleted until it is transferred or deleted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DeletionBlockers {
    /// Repositories the account owns, as `owner/name`.
    pub repositories: Vec<String>,
    /// Organizations the account is the only owner of.
    pub organizations: Vec<String>,
}

/// The server's account-deletion settings, set by the operator and read-only here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AccountDeletionSettings {
    /// Days a self-service or administrator deletion can be undone (`PYN_ACCOUNT_DELETION_WINDOW_DAYS`); 0 purges at the next pass.
    pub window_days: u32,
    /// Days a disabled account is kept before the server deletes it (`PYN_DISABLED_ACCOUNT_RETENTION_DAYS`); 0 means never.
    pub disabled_retention_days: u32,
}

/// Server settings an administrator can read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AdminSettings {
    pub account_deletion: AccountDeletionSettings,
    pub administrators: AdministratorSummary,
    pub storage: StorageSettings,
}

/// Where file content is kept (`PYN_S3_BUCKET`, `PYN_DATA_DIR` or neither) and whether it answered the last check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct StorageSettings {
    pub kind: StorageKind,
    /// The bucket name for `s3`; null otherwise. The endpoint and the credentials are never reported.
    pub bucket: Option<String>,
    /// `degraded` when the store failed its last probe (an S3 store is probed about every 30 seconds).
    pub status: StorageStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum StorageKind {
    /// Lost at restart; only for trying pyn out.
    Memory,
    Filesystem,
    S3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum StorageStatus {
    Ok,
    Degraded,
}

/// How many active server administrators exist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AdministratorSummary {
    /// Active accounts holding the administrator flag.
    pub active: u32,
    /// True when exactly one does: one lost credential from needing host recovery.
    pub single: bool,
}
