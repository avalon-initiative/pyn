use std::sync::Mutex;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::{PynError, Result, UserId};

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

/// Records limit refusals; the implementation decides how they are delivered.
#[async_trait]
pub trait LimitEvents: Send + Sync {
    async fn record(&self, event: LimitReached, now: DateTime<Utc>) -> Result<()>;
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
