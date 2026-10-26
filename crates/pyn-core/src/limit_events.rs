use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::access::AccountKind;
use crate::repo::Visibility;
use crate::{EventDraft, Limits, PynError, Result, UserId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitKind {
    Repositories,
    Members,
    StorageBytes,
    Locks,
}

/// An operation refused at the resource owner's limit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LimitReached {
    pub owner: UserId,
    pub kind: LimitKind,
    pub value: u64,
}

/// A change to an account, organization, repository or owner limits, as a plain fact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DomainEvent {
    AccountCreated {
        user: UserId,
        pending_approval: bool,
    },
    AccountApproved {
        user: UserId,
        actor: UserId,
    },
    AccountDisabled {
        user: UserId,
        actor: UserId,
        reason: Option<String>,
    },
    AccountEnabled {
        user: UserId,
        actor: UserId,
    },
    OrganizationCreated {
        name: UserId,
        owner: UserId,
        actor: UserId,
    },
    OrganizationDeleted {
        name: UserId,
        actor: UserId,
    },
    RepositoryCreated {
        owner: UserId,
        name: String,
        visibility: Visibility,
        actor: UserId,
    },
    RepositoryRenamed {
        owner: UserId,
        name: String,
        previous_name: String,
        actor: UserId,
    },
    RepositoryDeleted {
        owner: UserId,
        name: String,
        actor: UserId,
    },
    OwnerLimitsChanged {
        owner: UserId,
        kind: AccountKind,
        limits: Limits,
        actor: UserId,
    },
}

impl DomainEvent {
    pub fn event_type(&self) -> &'static str {
        match self {
            Self::AccountCreated { .. } => "account.created",
            Self::AccountApproved { .. } => "account.approved",
            Self::AccountDisabled { .. } => "account.disabled",
            Self::AccountEnabled { .. } => "account.enabled",
            Self::OrganizationCreated { .. } => "organization.created",
            Self::OrganizationDeleted { .. } => "organization.deleted",
            Self::RepositoryCreated { .. } => "repository.created",
            Self::RepositoryRenamed { .. } => "repository.renamed",
            Self::RepositoryDeleted { .. } => "repository.deleted",
            Self::OwnerLimitsChanged { .. } => "owner.limits_changed",
        }
    }
}

/// Builds the owner's changed limits into an event inside the store's transaction, from the stored result.
pub type LimitsEvent<'a> = &'a (dyn Fn(&Limits) -> Result<EventDraft> + Send + Sync);

/// Records limit refusals and turns domain facts into queueable events; the implementation owns the wire format.
#[async_trait]
pub trait LimitEvents: Send + Sync {
    async fn record(&self, event: LimitReached, now: DateTime<Utc>) -> Result<()>;

    fn draft(&self, event: DomainEvent, now: DateTime<Utc>) -> Result<EventDraft>;
}

/// Keeps the domain facts, for tests.
#[derive(Debug, Default)]
pub struct MemoryLimitEvents {
    recorded: Mutex<Vec<LimitReached>>,
}

impl MemoryLimitEvents {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn recorded(&self) -> Vec<LimitReached> {
        self.recorded.lock().unwrap().clone()
    }
}

#[async_trait]
impl LimitEvents for MemoryLimitEvents {
    async fn record(&self, event: LimitReached, _now: DateTime<Utc>) -> Result<()> {
        self.recorded.lock().unwrap().push(event);
        Ok(())
    }

    fn draft(&self, event: DomainEvent, now: DateTime<Utc>) -> Result<EventDraft> {
        let event_type = event.event_type();
        Ok(EventDraft {
            event_type: event_type.into(),
            payload: format!("{{\"type\":\"{event_type}\"}}"),
            at: now,
        })
    }
}

pub(crate) async fn limit_result<T>(
    events: &dyn LimitEvents,
    owner: &UserId,
    result: Result<T>,
    now: DateTime<Utc>,
) -> Result<T> {
    let (kind, value) = match &result {
        Err(PynError::RepoLimitReached { limit, .. }) => (LimitKind::Repositories, *limit),
        Err(PynError::MemberLimitReached { limit, .. }) => (LimitKind::Members, *limit),
        Err(PynError::StorageLimitReached { limit, .. }) => (LimitKind::StorageBytes, *limit),
        Err(PynError::LockLimitReached { limit }) => (LimitKind::Locks, u64::from(*limit)),
        _ => return result,
    };
    if let Err(error) = events
        .record(
            LimitReached {
                owner: owner.clone(),
                kind,
                value,
            },
            now,
        )
        .await
    {
        tracing::warn!(%owner, ?kind, %error, "failed to record limit reached event");
    }
    result
}
