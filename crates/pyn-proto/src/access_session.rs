//! Administrator access sessions (decision #1024): an administrator acts as one account for at most an hour.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Why an administrator acts as an account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AccessSessionReason {
    AbuseReport,
    SecurityIncident,
    SupportRequest,
    LegalRequest,
    /// Needs `notes`.
    Other,
}

/// Starts an access session on the account named in the path.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct StartAccessSessionRequest {
    /// One of `abuse_report`, `security_incident`, `support_request`, `legal_request` or `other`; chosen for each session.
    #[schema(value_type = AccessSessionReason)]
    pub reason: String,
    /// Required for `other`, at most 500 characters. The account sees them.
    #[serde(default)]
    pub notes: Option<String>,
}

/// A route a session called, counted. Never a body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AccessSessionReach {
    pub method: String,
    /// The route template, such as `/v1/repos/{owner}/{name}/files`.
    pub route: String,
    /// `owner/name` for a repository route, else empty.
    pub target: String,
    /// The route is not open to an access session, and the request was refused.
    pub refused: bool,
    pub count: u64,
    pub first_at: DateTime<Utc>,
    pub last_at: DateTime<Utc>,
}

/// An access session, as the administrator and the account both see it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AccessSessionInfo {
    pub id: String,
    /// The administrator, by the name written when the session started.
    pub admin: String,
    /// The account acted as, by the name written when the session started.
    pub user: String,
    #[schema(value_type = AccessSessionReason)]
    pub reason: String,
    pub notes: Option<String>,
    pub started_at: DateTime<Utc>,
    /// The session stops working at this instant; it is never extended.
    pub expires_at: DateTime<Utc>,
    /// When it stopped, once it has.
    #[schema(required = true)]
    pub ended_at: Option<DateTime<Utc>>,
    /// `active`, `ended` (by an administrator) or `expired`.
    pub state: String,
    /// The routes it called, in the order first reached; at most 200 distinct ones are listed, the rest are counted
    /// under one `(further routes)` row.
    pub reached: Vec<AccessSessionReach>,
}

/// A started session. The secret is the credential the administrator presents as a bearer token.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct StartedAccessSession {
    /// Shown once and never stored.
    pub secret: String,
    pub info: AccessSessionInfo,
}
