use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use pyn_core::{LimitEvents, LimitKind, LimitReached, PynError, Result, WebhookStore};
use pyn_proto::{Event, EventData, LimitKind as WireLimitKind, OwnerLimitReachedData};

/// Builds webhook envelopes for the domain's limit refusals.
pub struct WebhookLimitEvents {
    store: Arc<dyn WebhookStore>,
    public_url: String,
}

impl WebhookLimitEvents {
    pub fn new(store: Arc<dyn WebhookStore>, public_url: String) -> Self {
        Self { store, public_url }
    }
}

#[async_trait]
impl LimitEvents for WebhookLimitEvents {
    async fn record(&self, fact: LimitReached, now: DateTime<Utc>) -> Result<()> {
        let (limit, kind) = match fact.kind {
            LimitKind::Repositories => ("repositories", WireLimitKind::Repositories),
            LimitKind::Members => ("members", WireLimitKind::Members),
            LimitKind::StorageBytes => ("storage_bytes", WireLimitKind::StorageBytes),
            LimitKind::Locks => ("locks", WireLimitKind::Locks),
        };
        let event = Event::new(
            pyn_core::webhook::event_id()?,
            now,
            &self.public_url,
            EventData::OwnerLimitReached(OwnerLimitReachedData {
                owner: fact.owner.to_string(),
                limit: kind,
                value: fact.value,
            }),
        );
        let payload =
            serde_json::to_string(&event).map_err(|e| PynError::Storage(e.to_string()))?;
        self.store
            .append_limit_reached(&fact.owner, limit, &payload, now)
            .await?;
        Ok(())
    }
}
