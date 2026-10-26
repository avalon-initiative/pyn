//! Delivery of queued webhook events: claims due rows, signs and sends them, and records the outcome.

use std::sync::Arc;
use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, Utc};
use futures_util::StreamExt;
use pyn_core::{
    Clock, DueDelivery, Jitter, PynError, Result, SecretKey, WebhookStore, reveal_secret,
};

mod addresses;
mod sender;
mod signing;

pub use addresses::is_blocked;
pub use sender::{HttpSender, OutgoingRequest, SafeSender};
pub use signing::signature_header;

/// Largest error text kept on a delivery.
const MAX_ERROR_CHARS: usize = 200;

#[derive(Debug, Clone)]
pub struct WorkerConfig {
    /// Deliveries claimed per pass.
    pub batch: u32,
    /// Sends in flight at once in this process.
    pub concurrency: usize,
    /// How long a claim holds a delivery; longer than a send can take.
    pub lease: Duration,
    /// Pause between passes when nothing is due.
    pub poll: StdDuration,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            batch: 64,
            concurrency: 16,
            lease: Duration::minutes(2),
            poll: StdDuration::from_secs(5),
        }
    }
}

/// What one pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RunStats {
    pub claimed: usize,
    pub delivered: usize,
    pub failed: usize,
}

pub struct DeliveryWorker {
    store: Arc<dyn WebhookStore>,
    clock: Arc<dyn Clock>,
    sender: Arc<dyn HttpSender>,
    jitter: Arc<dyn Jitter>,
    key: Option<SecretKey>,
    server: String,
    config: WorkerConfig,
}

impl DeliveryWorker {
    /// `server` is the public base URL sent as `Pyn-Server`.
    pub fn new(
        store: Arc<dyn WebhookStore>,
        clock: Arc<dyn Clock>,
        sender: Arc<dyn HttpSender>,
        jitter: Arc<dyn Jitter>,
        key: Option<SecretKey>,
        server: String,
        config: WorkerConfig,
    ) -> Self {
        Self {
            store,
            clock,
            sender,
            jitter,
            key,
            server,
            config,
        }
    }

    /// One pass at `now`: claims due deliveries and attempts each, recording success or failure.
    pub async fn run_once(&self, now: DateTime<Utc>) -> Result<RunStats> {
        let due = self
            .store
            .claim_due(self.config.batch, self.config.lease, now)
            .await?;
        let claimed = due.len();
        let outcomes: Vec<bool> = futures_util::stream::iter(due)
            .map(|d| self.attempt(d, now))
            .buffer_unordered(self.config.concurrency.max(1))
            .collect()
            .await;
        let delivered = outcomes.iter().filter(|ok| **ok).count();
        Ok(RunStats {
            claimed,
            delivered,
            failed: claimed - delivered,
        })
    }

    async fn attempt(&self, due: DueDelivery, now: DateTime<Utc>) -> bool {
        let id = due.delivery.id.clone();
        let result = match self.build(&due, now).await {
            Ok(request) => self.sender.send(request).await,
            Err(e) => Err(e.to_string()),
        };
        let recorded = match result {
            Ok(status) if (200..300).contains(&status) => self
                .store
                .mark_delivered(&id, Some(i32::from(status)), now)
                .await
                .map(|_| true),
            Ok(status) => self
                .fail(
                    &id,
                    Some(status),
                    format!("the receiver answered {status}"),
                    now,
                )
                .await
                .map(|_| false),
            Err(error) => self.fail(&id, None, error, now).await.map(|_| false),
        };
        match recorded {
            Ok(delivered) => delivered,
            Err(error) => {
                tracing::warn!(delivery = %id, %error, "could not record a webhook delivery outcome");
                false
            }
        }
    }

    async fn fail(
        &self,
        id: &str,
        status: Option<u16>,
        error: String,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let error: String = error.chars().take(MAX_ERROR_CHARS).collect();
        self.store
            .record_failure(id, status.map(i32::from), &error, now, self.jitter.as_ref())
            .await?;
        Ok(())
    }

    /// The signed request for this attempt; the timestamp is fresh each time and the delivery id stays.
    async fn build(&self, due: &DueDelivery, now: DateTime<Utc>) -> Result<OutgoingRequest> {
        let stored = self.store.list_active_secrets(due.subscription.id).await?;
        let keys = stored
            .iter()
            .map(|s| reveal_secret(self.key.as_ref(), s))
            .collect::<Result<Vec<_>>>()?;
        if keys.is_empty() {
            return Err(PynError::Storage(
                "the subscription has no active signing secret".into(),
            ));
        }
        let body = due.event.payload.clone().into_bytes();
        let signature = signature_header(&keys, now.timestamp(), &body);
        Ok(OutgoingRequest {
            url: due.subscription.url.clone(),
            headers: vec![
                ("Pyn-Signature", signature),
                ("Pyn-Delivery", due.delivery.id.clone()),
                ("Pyn-Event", due.event.event_type.clone()),
                ("Pyn-Server", self.server.clone()),
            ],
            body,
        })
    }

    /// Runs passes forever on this process. Several server copies may each run one: claims are exclusive.
    pub fn spawn(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut next_purge = self.clock.now();
            loop {
                let now = self.clock.now();
                let mut busy = false;
                match self.run_once(now).await {
                    Ok(stats) => busy = stats.claimed >= self.config.batch as usize,
                    Err(error) => tracing::warn!(%error, "webhook delivery pass failed"),
                }
                if now >= next_purge {
                    if let Err(error) = self.store.purge(pyn_core::webhook::retention(), now).await
                    {
                        tracing::warn!(%error, "webhook retention purge failed");
                    }
                    next_purge = now + Duration::hours(1);
                }
                if !busy {
                    tokio::time::sleep(self.config.poll).await;
                }
            }
        })
    }
}
