use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Whether a newer pyn release exists, as this server last learned it (`GET /v1/admin/update`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UpdateStatus {
    /// The server runs the check (`PYN_UPDATE_CHECK=on`). When false, every other field but `current` is absent.
    pub enabled: bool,
    /// The running server's version.
    pub current: String,
    /// The newest release on this server's channel; absent when the server is up to date or has not learned it yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest: Option<String>,
    /// Whether `latest` is a pre-release; present with `latest`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prerelease: Option<bool>,
    /// The release notes page for `latest`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// When the last successful lookup finished; absent while none has. With `latest` absent, present means up to date.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<DateTime<Utc>>,
}
