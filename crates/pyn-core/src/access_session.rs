//! Administrator access sessions: an administrator acts as one account for at most an hour, for a stated reason
//! (decision #1024). A distinct credential kind, never a standing grant.

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Duration, Utc};

use crate::access::{TokenId, token};
use crate::error::{PynError, Result};
use crate::types::{AccountId, UserId};

/// How long a session lasts. It never extends or renews itself.
pub const ACCESS_SESSION_MINUTES: i64 = 60;

/// The longest the free-text notes may be, in characters.
pub const ACCESS_SESSION_NOTES_CHARS: usize = 500;

/// The most distinct routes a session itemises in the audit logs; further ones are counted under one overflow entry.
pub const ACCESS_SESSION_MAX_ROUTES: usize = 200;

/// The method and route of the overflow row.
pub const OVERFLOW_ROUTE: (&str, &str) = ("-", "(further routes)");

pub fn session_length() -> Duration {
    Duration::minutes(ACCESS_SESSION_MINUTES)
}

/// Why an administrator acts as an account. A fixed list; `other` needs notes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccessSessionReason {
    AbuseReport,
    SecurityIncident,
    SupportRequest,
    LegalRequest,
    Other,
}

impl AccessSessionReason {
    pub const ALL: [Self; 5] = [
        Self::AbuseReport,
        Self::SecurityIncident,
        Self::SupportRequest,
        Self::LegalRequest,
        Self::Other,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::AbuseReport => "abuse_report",
            Self::SecurityIncident => "security_incident",
            Self::SupportRequest => "support_request",
            Self::LegalRequest => "legal_request",
            Self::Other => "other",
        }
    }
}

impl fmt::Display for AccessSessionReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for AccessSessionReason {
    type Err = PynError;
    fn from_str(s: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|r| r.as_str() == s)
            .ok_or_else(|| {
                let all: Vec<_> = Self::ALL.iter().map(|r| r.as_str()).collect();
                PynError::InvalidRequest(format!(
                    "unknown access session reason {s:?}; use one of {}",
                    all.join(", ")
                ))
            })
    }
}

/// How a session stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessSessionEnd {
    /// The administrator ended it.
    Ended,
    /// The hour ran out.
    Expired,
}

impl AccessSessionEnd {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ended => "ended",
            Self::Expired => "expired",
        }
    }
}

impl FromStr for AccessSessionEnd {
    type Err = PynError;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "ended" => Ok(Self::Ended),
            "expired" => Ok(Self::Expired),
            other => Err(PynError::Storage(format!("unknown session end {other:?}"))),
        }
    }
}

/// A stored session. Only a hash of the secret is kept. Names are as written when it started; the account ids are
/// what the session follows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessSessionRecord {
    pub id: TokenId,
    pub secret_hash: String,
    pub admin_id: AccountId,
    pub admin: UserId,
    pub account_id: AccountId,
    pub user: UserId,
    pub reason: AccessSessionReason,
    pub notes: Option<String>,
    pub started_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    /// When it stopped working: the administrator's end, or `expires_at` once the expiry is recorded.
    pub ended_at: Option<DateTime<Utc>>,
    pub end: Option<AccessSessionEnd>,
}

impl AccessSessionRecord {
    /// Whether the credential still works at `now`.
    pub fn is_active(&self, now: DateTime<Utc>) -> bool {
        self.ended_at.is_none() && self.expires_at > now
    }

    /// The audit actor of what the session does: the administrator behind it.
    pub fn actor(&self) -> UserId {
        self.admin.clone()
    }
}

/// One route a session called, counted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessSessionReach {
    pub method: String,
    pub route: String,
    /// `owner/name` for a repository route, else empty.
    pub target: String,
    pub refused: bool,
    pub count: u64,
    pub first_at: DateTime<Utc>,
    pub last_at: DateTime<Utc>,
}

/// What counting a call did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReachNote {
    /// A route the session had not called: itemised.
    First,
    Again,
    /// The bound on itemised routes was just passed: the overflow row was created.
    Overflowed,
}

/// Session credential strings look like `pyna_<12 hex id>_<64 hex secret>`.
pub mod session_credential {
    use super::*;

    const PREFIX: &str = "pyna_";

    pub fn generate() -> Result<(TokenId, String, String)> {
        let (id, secret, hash) = token::random_parts()?;
        Ok((TokenId(id.clone()), format!("{PREFIX}{id}_{secret}"), hash))
    }

    pub fn is_access_session(raw: &str) -> bool {
        raw.starts_with(PREFIX)
    }

    pub fn parse(raw: &str) -> Option<(TokenId, &str)> {
        let rest = raw.strip_prefix(PREFIX)?;
        let (id, secret) = rest.split_once('_')?;
        token::is_id_and_secret(id, secret).then(|| (TokenId(id.to_string()), secret))
    }
}

/// A session as its account sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessSessionView {
    pub record: AccessSessionRecord,
    pub reached: Vec<AccessSessionReach>,
    /// `active`, `ended` or `expired`, as of when the view was made.
    pub state: &'static str,
}
