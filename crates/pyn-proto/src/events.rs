//! Lifecycle event schema: the JSON body of a webhook delivery. Event types are an open set, so a receiver
//! must keep working when a type it does not know arrives.

use chrono::{DateTime, Utc};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use utoipa::ToSchema;

use crate::{Limits, OwnerKind};

/// The envelope version this build writes. Adding an event type or an optional `data` field keeps it; a breaking
/// change to the envelope or to an existing payload raises it.
pub const EVENT_VERSION: u32 = 1;

/// Which limit an owner ran into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LimitKind {
    Repositories,
    Members,
    StorageBytes,
    Locks,
}

/// `account.created`: a new account exists (sign-up, external sign-in or an administrator).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AccountCreatedData {
    pub user: String,
    /// The account's stable id.
    pub user_id: String,
    /// True when an administrator must approve the account before it can sign in.
    pub pending_approval: bool,
}

/// `account.approved`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AccountApprovedData {
    pub user: String,
    pub user_id: String,
    /// The administrator or service credential that approved it.
    pub actor: String,
}

/// `account.disabled`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AccountDisabledData {
    pub user: String,
    pub user_id: String,
    pub actor: String,
    /// The reason given when it was disabled, if any.
    #[schema(required = true)]
    pub reason: Option<String>,
}

/// `account.enabled`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AccountEnabledData {
    pub user: String,
    pub user_id: String,
    pub actor: String,
}

/// `organization.created`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OrganizationCreatedData {
    pub name: String,
    /// The organization's stable id.
    pub org_id: String,
    /// The organization's first owner.
    pub owner: String,
    pub owner_id: String,
    pub actor: String,
}

/// `organization.deleted`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OrganizationDeletedData {
    pub name: String,
    /// The deleted organization's id, never issued again.
    pub org_id: String,
    pub actor: String,
}

/// `repository.created`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RepositoryCreatedData {
    /// The repository's stable id.
    pub repo_id: String,
    pub owner: String,
    pub owner_id: String,
    pub name: String,
    pub visibility: crate::Visibility,
    pub actor: String,
}

/// `repository.renamed`: the name changed; the owner never does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RepositoryRenamedData {
    /// Unchanged by the rename: re-key on it.
    pub repo_id: String,
    pub owner: String,
    pub owner_id: String,
    pub name: String,
    pub previous_name: String,
    pub actor: String,
}

/// `repository.deleted`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RepositoryDeletedData {
    /// The deleted repository's id, never issued again.
    pub repo_id: String,
    pub owner: String,
    pub owner_id: String,
    pub name: String,
    pub actor: String,
}

/// `owner.limits_changed`: an owner's own limits were set; `limits` is the result, null meaning the server default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OwnerLimitsChangedData {
    pub owner: String,
    pub owner_id: String,
    pub kind: OwnerKind,
    pub limits: Limits,
    pub actor: String,
}

/// `owner.limit_reached`: an operation was refused because the owner is at a limit. It names no actor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OwnerLimitReachedData {
    pub owner: String,
    pub owner_id: String,
    pub limit: LimitKind,
    /// The limit in force when the operation was refused.
    pub value: u64,
}

/// `lock.acquired`: a path was locked, including when a lapsed lease was taken over. A renewal raises nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LockAcquiredData {
    pub owner: String,
    pub owner_id: String,
    pub repo: String,
    pub repo_id: String,
    pub path: String,
    pub holder: String,
    pub expires_at: DateTime<Utc>,
}

/// `lock.released`: the holder released the lock, or a check-in consumed it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LockReleasedData {
    pub owner: String,
    pub owner_id: String,
    pub repo: String,
    pub repo_id: String,
    pub path: String,
    pub holder: String,
    /// `checkin` when a revision of the path consumed the lock; null for an explicit release.
    #[schema(required = true)]
    pub reason: Option<String>,
}

/// `lock.expired`: a lease had lapsed when the path was locked again; observed lazily, not at the expiry instant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LockExpiredData {
    pub owner: String,
    pub owner_id: String,
    pub repo: String,
    pub repo_id: String,
    pub path: String,
    /// Who held the lapsed lease.
    pub holder: String,
    /// When the lease ran out.
    pub expires_at: DateTime<Utc>,
}

/// `lock.forced`: a live lock was removed without its holder, by a forced unlock or a policy change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LockForcedData {
    pub owner: String,
    pub owner_id: String,
    pub repo: String,
    pub repo_id: String,
    pub path: String,
    /// Whose lock was removed.
    pub holder: String,
    pub actor: String,
    #[schema(required = true)]
    pub reason: Option<String>,
}

/// The typed `data` of an event, chosen by its `type`.
#[derive(Debug, Clone, PartialEq)]
pub enum EventData {
    AccountCreated(AccountCreatedData),
    AccountApproved(AccountApprovedData),
    AccountDisabled(AccountDisabledData),
    AccountEnabled(AccountEnabledData),
    OrganizationCreated(OrganizationCreatedData),
    OrganizationDeleted(OrganizationDeletedData),
    RepositoryCreated(RepositoryCreatedData),
    RepositoryRenamed(RepositoryRenamedData),
    RepositoryDeleted(RepositoryDeletedData),
    OwnerLimitsChanged(OwnerLimitsChangedData),
    OwnerLimitReached(OwnerLimitReachedData),
    LockAcquired(LockAcquiredData),
    LockReleased(LockReleasedData),
    LockExpired(LockExpiredData),
    LockForced(LockForcedData),
    /// A type this build does not know, kept verbatim so it round-trips.
    Other {
        event_type: String,
        data: Value,
    },
}

/// Every event type this build knows.
pub const KNOWN_EVENT_TYPES: [&str; 15] = [
    "account.created",
    "account.approved",
    "account.disabled",
    "account.enabled",
    "organization.created",
    "organization.deleted",
    "repository.created",
    "repository.renamed",
    "repository.deleted",
    "owner.limits_changed",
    "owner.limit_reached",
    "lock.acquired",
    "lock.released",
    "lock.expired",
    "lock.forced",
];

impl EventData {
    /// The event's `type` string.
    pub fn event_type(&self) -> &str {
        match self {
            Self::AccountCreated(_) => "account.created",
            Self::AccountApproved(_) => "account.approved",
            Self::AccountDisabled(_) => "account.disabled",
            Self::AccountEnabled(_) => "account.enabled",
            Self::OrganizationCreated(_) => "organization.created",
            Self::OrganizationDeleted(_) => "organization.deleted",
            Self::RepositoryCreated(_) => "repository.created",
            Self::RepositoryRenamed(_) => "repository.renamed",
            Self::RepositoryDeleted(_) => "repository.deleted",
            Self::OwnerLimitsChanged(_) => "owner.limits_changed",
            Self::OwnerLimitReached(_) => "owner.limit_reached",
            Self::LockAcquired(_) => "lock.acquired",
            Self::LockReleased(_) => "lock.released",
            Self::LockExpired(_) => "lock.expired",
            Self::LockForced(_) => "lock.forced",
            Self::Other { event_type, .. } => event_type,
        }
    }

    fn to_value(&self) -> Result<Value, serde_json::Error> {
        match self {
            Self::AccountCreated(d) => serde_json::to_value(d),
            Self::AccountApproved(d) => serde_json::to_value(d),
            Self::AccountDisabled(d) => serde_json::to_value(d),
            Self::AccountEnabled(d) => serde_json::to_value(d),
            Self::OrganizationCreated(d) => serde_json::to_value(d),
            Self::OrganizationDeleted(d) => serde_json::to_value(d),
            Self::RepositoryCreated(d) => serde_json::to_value(d),
            Self::RepositoryRenamed(d) => serde_json::to_value(d),
            Self::RepositoryDeleted(d) => serde_json::to_value(d),
            Self::OwnerLimitsChanged(d) => serde_json::to_value(d),
            Self::OwnerLimitReached(d) => serde_json::to_value(d),
            Self::LockAcquired(d) => serde_json::to_value(d),
            Self::LockReleased(d) => serde_json::to_value(d),
            Self::LockExpired(d) => serde_json::to_value(d),
            Self::LockForced(d) => serde_json::to_value(d),
            Self::Other { data, .. } => Ok(data.clone()),
        }
    }

    fn from_parts(event_type: &str, data: Value) -> Result<Self, serde_json::Error> {
        Ok(match event_type {
            "account.created" => Self::AccountCreated(serde_json::from_value(data)?),
            "account.approved" => Self::AccountApproved(serde_json::from_value(data)?),
            "account.disabled" => Self::AccountDisabled(serde_json::from_value(data)?),
            "account.enabled" => Self::AccountEnabled(serde_json::from_value(data)?),
            "organization.created" => Self::OrganizationCreated(serde_json::from_value(data)?),
            "organization.deleted" => Self::OrganizationDeleted(serde_json::from_value(data)?),
            "repository.created" => Self::RepositoryCreated(serde_json::from_value(data)?),
            "repository.renamed" => Self::RepositoryRenamed(serde_json::from_value(data)?),
            "repository.deleted" => Self::RepositoryDeleted(serde_json::from_value(data)?),
            "owner.limits_changed" => Self::OwnerLimitsChanged(serde_json::from_value(data)?),
            "owner.limit_reached" => Self::OwnerLimitReached(serde_json::from_value(data)?),
            "lock.acquired" => Self::LockAcquired(serde_json::from_value(data)?),
            "lock.released" => Self::LockReleased(serde_json::from_value(data)?),
            "lock.expired" => Self::LockExpired(serde_json::from_value(data)?),
            "lock.forced" => Self::LockForced(serde_json::from_value(data)?),
            other => Self::Other {
                event_type: other.to_string(),
                data,
            },
        })
    }
}

/// The body of one webhook delivery. `type` and `data` go together: see the payload schemas named
/// `*Data` for each known type. A receiver ignores types it does not know.
#[derive(Debug, Clone, PartialEq, ToSchema)]
pub struct Event {
    /// Unique per event; a redelivery keeps it.
    pub id: String,
    /// The event type, a dotted name such as `repository.created`. An open set: new types are added without a
    /// new `version`.
    #[serde(rename = "type")]
    #[schema(rename = "type", value_type = String, examples("repository.created"))]
    pub event_type: String,
    pub created_at: DateTime<Utc>,
    /// The server's public base URL.
    pub server: String,
    /// The envelope version, `1` today.
    pub version: u32,
    /// The payload for `type`.
    #[schema(value_type = Object)]
    pub data: EventData,
}

impl Event {
    pub fn new(
        id: impl Into<String>,
        created_at: DateTime<Utc>,
        server: impl Into<String>,
        data: EventData,
    ) -> Self {
        Self {
            id: id.into(),
            event_type: data.event_type().to_string(),
            created_at,
            server: server.into(),
            version: EVENT_VERSION,
            data,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Wire {
    id: String,
    #[serde(rename = "type")]
    event_type: String,
    created_at: DateTime<Utc>,
    server: String,
    version: u32,
    data: Value,
}

impl Serialize for Event {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        Wire {
            id: self.id.clone(),
            event_type: self.data.event_type().to_string(),
            created_at: self.created_at,
            server: self.server.clone(),
            version: self.version,
            data: self.data.to_value().map_err(serde::ser::Error::custom)?,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Event {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = Wire::deserialize(deserializer)?;
        let data = EventData::from_parts(&wire.event_type, wire.data)
            .map_err(|e| D::Error::custom(format!("{}: {e}", wire.event_type)))?;
        Ok(Self {
            id: wire.id,
            event_type: wire.event_type,
            created_at: wire.created_at,
            server: wire.server,
            version: wire.version,
            data,
        })
    }
}
