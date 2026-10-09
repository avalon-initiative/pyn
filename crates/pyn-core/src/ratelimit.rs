use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};

use crate::error::Result;

/// Events counted under one key in the current window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateState {
    pub count: u32,
    pub resets_at: DateTime<Utc>,
}

/// Fixed-window counters keyed by string. `now` comes from the caller's `Clock`.
#[async_trait]
pub trait RateLimitStore: Send + Sync {
    /// Counts one event; a window that has ended starts over at one. Returns the state including this event.
    async fn hit(&self, key: &str, window: Duration, now: DateTime<Utc>) -> Result<RateState>;

    /// The live window for the key, if any, without counting.
    async fn state(&self, key: &str, now: DateTime<Utc>) -> Result<Option<RateState>>;

    async fn reset(&self, key: &str) -> Result<()>;

    /// Removes every window that ended at or before `now`.
    async fn sweep(&self, now: DateTime<Utc>) -> Result<()>;
}
