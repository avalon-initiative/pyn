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
    pub certificate: CertificateSettings,
}

/// What the server's periodic TLS handshake against its own public address (`PYN_PUBLIC_URL`) saw.
/// Only public certificate facts and the configured host are reported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CertificateSettings {
    pub state: CertificateState,
    /// The host the handshake targets; null while `unchecked`.
    pub host: Option<String>,
    /// Whether the chain verified against the system roots for `host`; null until a certificate was read.
    /// An untrusted certificate (a private authority) still has its expiry reported.
    pub trusted: Option<bool>,
    pub not_before: Option<DateTime<Utc>>,
    pub not_after: Option<DateTime<Utc>>,
    /// Whole days until `not_after`, rounded down; negative once expired. Null until a certificate was read.
    pub days_left: Option<i64>,
    /// The DNS and IP names in the certificate (its subject name when it has none).
    pub names: Vec<String>,
    /// The issuer's common name, when it has one.
    pub issuer: Option<String>,
    pub last_checked: Option<DateTime<Utc>>,
    /// Why the last check failed (null when it succeeded); `state` is `unreachable` then, and the certificate fields keep the last good read.
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CertificateState {
    Ok,
    /// Less remaining than the smaller of 14 days and a third of the certificate's lifetime.
    Expiring,
    Expired,
    /// The last handshake failed.
    Unreachable,
    /// Not checked: `PYN_CERT_CHECK_INTERVAL_SECS=0`, an http public URL or a loopback host, or the first check has not run yet.
    Unchecked,
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
