//! The readiness report of `GET /readyz`.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Whether this server copy should receive traffic. Carries no secrets: check names and short fixed details only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ReadinessReport {
    pub status: ReadinessStatus,
    /// Every check that ran; empty while `draining`.
    pub checks: Vec<ReadinessCheck>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessStatus {
    /// Every check passed (HTTP 200).
    Ok,
    /// At least one check failed (HTTP 503).
    Unavailable,
    /// The server is shutting down and has stopped taking new work (HTTP 503).
    Draining,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ReadinessCheck {
    /// `metadata_store` or `object_store`.
    pub name: String,
    pub status: ReadinessCheckStatus,
    /// Why the check failed; absent when it passed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessCheckStatus {
    Ok,
    Failing,
}
