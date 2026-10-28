use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::access::AccountKind;
use crate::repo::Visibility;
use crate::{AccountId, EventDraft, Limits, Lock, PynError, RepoId, RepoPath, Result, UserId};

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
    pub owner_id: AccountId,
    pub kind: LimitKind,
    pub value: u64,
}

/// A change to an account, organization, repository or owner limits, as a plain fact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DomainEvent {
    AccountCreated {
        user: UserId,
        user_id: AccountId,
        pending_approval: bool,
    },
    AccountApproved {
        user: UserId,
        user_id: AccountId,
        actor: UserId,
    },
    AccountDisabled {
        user: UserId,
        user_id: AccountId,
        actor: UserId,
        reason: Option<String>,
    },
    AccountEnabled {
        user: UserId,
        user_id: AccountId,
        actor: UserId,
    },
    OrganizationCreated {
        name: UserId,
        org_id: AccountId,
        owner: UserId,
        owner_id: AccountId,
        actor: UserId,
    },
    OrganizationDeleted {
        name: UserId,
        org_id: AccountId,
        actor: UserId,
    },
    RepositoryCreated {
        repo_id: RepoId,
        owner: UserId,
        owner_id: AccountId,
        name: String,
        visibility: Visibility,
        actor: UserId,
    },
    RepositoryRenamed {
        repo_id: RepoId,
        owner: UserId,
        owner_id: AccountId,
        name: String,
        previous_name: String,
        actor: UserId,
    },
    RepositoryDeleted {
        repo_id: RepoId,
        owner: UserId,
        owner_id: AccountId,
        name: String,
        actor: UserId,
    },
    OwnerLimitsChanged {
        owner: UserId,
        owner_id: AccountId,
        kind: AccountKind,
        limits: Limits,
        actor: UserId,
    },
    LockAcquired {
        owner: UserId,
        owner_id: AccountId,
        repo: String,
        repo_id: RepoId,
        path: RepoPath,
        holder: UserId,
        expires_at: DateTime<Utc>,
    },
    LockReleased {
        owner: UserId,
        owner_id: AccountId,
        repo: String,
        repo_id: RepoId,
        path: RepoPath,
        holder: UserId,
        /// `checkin` when a revision consumed the lock; none for an explicit release.
        reason: Option<String>,
    },
    /// A lease that lapsed and was taken over; `expires_at` is when it ran out.
    LockExpired {
        owner: UserId,
        owner_id: AccountId,
        repo: String,
        repo_id: RepoId,
        path: RepoPath,
        holder: UserId,
        expires_at: DateTime<Utc>,
    },
    LockForced {
        owner: UserId,
        owner_id: AccountId,
        repo: String,
        repo_id: RepoId,
        path: RepoPath,
        holder: UserId,
        actor: UserId,
        reason: Option<String>,
    },
}

/// What a store did to a lock, handed to the builder that turns it into a queued event.
#[derive(Debug, Clone, Copy)]
pub enum LockChange<'a> {
    Acquired(&'a Lock),
    Released(&'a Lock),
    /// Consumed by a revision of its path.
    CheckedIn(&'a Lock),
    Expired(&'a Lock),
    Forced(&'a Lock),
}

/// Builds the event for a lock change inside the store's transaction, once the change is known.
pub type LockEventBuilder<'a> = &'a (dyn Fn(LockChange<'_>) -> Result<EventDraft> + Send + Sync);

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
            Self::LockAcquired { .. } => "lock.acquired",
            Self::LockReleased { .. } => "lock.released",
            Self::LockExpired { .. } => "lock.expired",
            Self::LockForced { .. } => "lock.forced",
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
    owner_id: impl std::future::Future<Output = Result<AccountId>>,
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
    let recorded = match owner_id.await {
        Ok(owner_id) => {
            events
                .record(
                    LimitReached {
                        owner: owner.clone(),
                        owner_id,
                        kind,
                        value,
                    },
                    now,
                )
                .await
        }
        Err(error) => Err(error),
    };
    if let Err(error) = recorded {
        tracing::warn!(%owner, ?kind, %error, "failed to record limit reached event");
    }
    result
}
