use std::fmt;
use std::str::FromStr;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{PynError, Result};
use crate::types::UserId;

pub(crate) fn event_id() -> Result<String> {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).map_err(|e| PynError::Storage(format!("no randomness: {e}")))?;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = hex::encode(b);
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    ))
}

/// Attempts made before a delivery is marked failed.
pub const MAX_ATTEMPTS: u32 = 8;

/// Active secrets a subscription may hold at once, so one can be rotated in before the other retires.
pub const MAX_ACTIVE_SECRETS: usize = 2;

/// Days delivery and outbox rows are kept.
pub const RETENTION_DAYS: i64 = 30;

/// Spread applied to a delay: `1 - JITTER_SPREAD / 2` to `1 + JITTER_SPREAD / 2` of the base.
const JITTER_SPREAD: f64 = 0.2;

/// Base wait in seconds after the nth failed attempt; the 7 waits add up to about 22 hours.
const BACKOFF_SECONDS: [i64; 7] = [60, 300, 900, 3_600, 10_800, 21_600, 43_200];

/// Retention as a duration.
pub fn retention() -> Duration {
    Duration::days(RETENTION_DAYS)
}

/// A source of numbers in `[0, 1)`; injected so backoff is deterministic in tests.
pub trait Jitter: Send + Sync {
    fn unit(&self) -> f64;
}

/// Always returns the same value.
#[derive(Debug, Clone, Copy)]
pub struct FixedJitter(pub f64);

impl Jitter for FixedJitter {
    fn unit(&self) -> f64 {
        self.0
    }
}

/// Jitter from the operating system's randomness.
#[derive(Debug, Default, Clone, Copy)]
pub struct RandomJitter;

impl Jitter for RandomJitter {
    fn unit(&self) -> f64 {
        let mut buf = [0u8; 8];
        if getrandom::fill(&mut buf).is_err() {
            return 0.5;
        }
        (u64::from_le_bytes(buf) >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// The wait before the next try after `attempt` attempts have failed (1-based), or `None` once the attempts are spent.
pub fn retry_delay(attempt: u32, jitter: f64) -> Option<Duration> {
    if attempt == 0 || attempt >= MAX_ATTEMPTS {
        return None;
    }
    let base = BACKOFF_SECONDS[attempt as usize - 1] as f64;
    let factor = 1.0 - JITTER_SPREAD / 2.0 + JITTER_SPREAD * jitter.clamp(0.0, 1.0);
    Some(Duration::milliseconds((base * factor * 1000.0) as i64))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryStatus {
    Pending,
    Delivered,
    Failed,
}

impl DeliveryStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Delivered => "delivered",
            Self::Failed => "failed",
        }
    }
}

impl fmt::Display for DeliveryStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for DeliveryStatus {
    type Err = PynError;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "pending" => Ok(Self::Pending),
            "delivered" => Ok(Self::Delivered),
            "failed" => Ok(Self::Failed),
            other => Err(PynError::Storage(format!(
                "unknown delivery status {other}"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subscription {
    pub id: i64,
    pub name: String,
    pub url: String,
    /// Event types delivered; empty means every type.
    pub event_types: Vec<String>,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub created_by: UserId,
}

impl Subscription {
    pub fn wants(&self, event_type: &str) -> bool {
        self.enabled
            && (self.event_types.is_empty() || self.event_types.iter().any(|t| t == event_type))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSubscription {
    pub name: String,
    pub url: String,
    pub event_types: Vec<String>,
    pub enabled: bool,
    pub created_by: UserId,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SubscriptionUpdate {
    pub name: Option<String>,
    pub url: Option<String>,
    pub event_types: Option<Vec<String>>,
    pub enabled: Option<bool>,
}

/// A signing secret as stored: plaintext, or ciphertext when `encrypted`.
#[derive(Clone, PartialEq, Eq)]
pub struct WebhookSecret {
    pub id: i64,
    pub subscription_id: i64,
    pub secret: Vec<u8>,
    pub encrypted: bool,
    pub key_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub retired_at: Option<DateTime<Utc>>,
}

impl fmt::Debug for WebhookSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WebhookSecret")
            .field("id", &self.id)
            .field("subscription_id", &self.subscription_id)
            .field("encrypted", &self.encrypted)
            .field("key_id", &self.key_id)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct NewSecret {
    pub secret: Vec<u8>,
    pub encrypted: bool,
    pub key_id: Option<String>,
}

impl fmt::Debug for NewSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NewSecret")
            .field("encrypted", &self.encrypted)
            .field("key_id", &self.key_id)
            .finish_non_exhaustive()
    }
}

/// An event as queued. `event_type` is an open string and `payload` is JSON text, both opaque here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxEvent {
    pub id: i64,
    pub event_type: String,
    pub payload: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivery {
    /// A UUID, stable across retries.
    pub id: String,
    pub subscription_id: i64,
    pub event_id: i64,
    pub attempt_count: u32,
    pub next_attempt_at: DateTime<Utc>,
    pub claimed_until: Option<DateTime<Utc>>,
    pub status: DeliveryStatus,
    pub last_status_code: Option<i32>,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub delivered_at: Option<DateTime<Utc>>,
    pub failed_at: Option<DateTime<Utc>>,
}

/// A claimed delivery with what a sender needs to attempt it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DueDelivery {
    pub delivery: Delivery,
    pub event: OutboxEvent,
    pub subscription: Subscription,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeliveryQuery {
    pub status: Option<DeliveryStatus>,
    /// Only deliveries of events with an id below this.
    pub before_event: Option<i64>,
    pub limit: u32,
}

/// Subscriptions, signing secrets, the event outbox and per-subscription deliveries.
///
/// `now` comes from the caller's `Clock`; a store backed by a shared database may ignore it and use the database clock.
#[async_trait]
pub trait WebhookStore: Send + Sync {
    async fn create_subscription(
        &self,
        new: NewSubscription,
        now: DateTime<Utc>,
    ) -> Result<Subscription>;

    async fn get_subscription(&self, id: i64) -> Result<Option<Subscription>>;

    async fn list_subscriptions(&self) -> Result<Vec<Subscription>>;

    /// `None` when the subscription does not exist.
    async fn update_subscription(
        &self,
        id: i64,
        update: SubscriptionUpdate,
        now: DateTime<Utc>,
    ) -> Result<Option<Subscription>>;

    /// Removes the subscription with its secrets and deliveries; false when it does not exist.
    async fn delete_subscription(&self, id: i64) -> Result<bool>;

    /// `None` when the subscription does not exist; fails when it already has `MAX_ACTIVE_SECRETS` active secrets.
    async fn add_secret(
        &self,
        subscription_id: i64,
        secret: NewSecret,
        now: DateTime<Utc>,
    ) -> Result<Option<WebhookSecret>>;

    /// False when the secret is not an active one of that subscription.
    async fn retire_secret(
        &self,
        subscription_id: i64,
        secret_id: i64,
        now: DateTime<Utc>,
    ) -> Result<bool>;

    /// Active secrets, oldest first.
    async fn list_active_secrets(&self, subscription_id: i64) -> Result<Vec<WebhookSecret>>;

    /// Queues the event and one pending delivery per enabled subscription whose filter matches, atomically.
    async fn append_event(
        &self,
        event_type: &str,
        payload: &str,
        now: DateTime<Utc>,
    ) -> Result<OutboxEvent>;

    /// Queues `owner.limit_reached` and its deliveries unless this owner and limit were admitted
    /// less than one hour ago. Admission and append are atomic; `now` always uses the caller's clock.
    /// Returns `None` when suppressed, without extending the window.
    async fn append_limit_reached(
        &self,
        owner: &UserId,
        limit: &str,
        payload: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<OutboxEvent>>;

    /// Takes up to `limit` pending deliveries that are due, unleased and of enabled subscriptions, leasing them for
    /// `lease`. No row goes to two concurrent claimers; a lapsed lease makes the row claimable again.
    async fn claim_due(
        &self,
        limit: u32,
        lease: Duration,
        now: DateTime<Utc>,
    ) -> Result<Vec<DueDelivery>>;

    /// Records a delivered attempt. `None` when the delivery does not exist; a delivery no longer pending is returned unchanged.
    async fn mark_delivered(
        &self,
        delivery_id: &str,
        status_code: Option<i32>,
        now: DateTime<Utc>,
    ) -> Result<Option<Delivery>>;

    /// Records a failed attempt and schedules the next with `retry_delay`, or marks the delivery failed once spent.
    async fn record_failure(
        &self,
        delivery_id: &str,
        status_code: Option<i32>,
        error: &str,
        now: DateTime<Utc>,
        jitter: &dyn Jitter,
    ) -> Result<Option<Delivery>>;

    /// Newest event first.
    async fn list_deliveries(
        &self,
        subscription_id: i64,
        query: &DeliveryQuery,
    ) -> Result<Vec<Delivery>>;

    /// Resets a delivery to pending with no attempts, due now. `None` when it does not exist.
    async fn redeliver(&self, delivery_id: &str, now: DateTime<Utc>) -> Result<Option<Delivery>>;

    /// Removes events older than `retention` with their deliveries; returns the events removed.
    async fn purge(&self, retention: Duration, now: DateTime<Utc>) -> Result<u64>;
}
