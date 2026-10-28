use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use pyn_core::*;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Default)]
pub struct FailingWebhooks {
    pub attempts: AtomicUsize,
}

#[async_trait]
impl WebhookStore for FailingWebhooks {
    async fn create_subscription(
        &self,
        _new: NewSubscription,
        _now: DateTime<Utc>,
    ) -> Result<Subscription> {
        panic!("unexpected WebhookStore::create_subscription")
    }

    async fn get_subscription(&self, _id: i64) -> Result<Option<Subscription>> {
        panic!("unexpected WebhookStore::get_subscription")
    }

    async fn list_subscriptions(&self) -> Result<Vec<Subscription>> {
        panic!("unexpected WebhookStore::list_subscriptions")
    }

    async fn update_subscription(
        &self,
        _id: i64,
        _update: SubscriptionUpdate,
        _now: DateTime<Utc>,
    ) -> Result<Option<Subscription>> {
        panic!("unexpected WebhookStore::update_subscription")
    }

    async fn delete_subscription(&self, _id: i64) -> Result<bool> {
        panic!("unexpected WebhookStore::delete_subscription")
    }

    async fn add_secret(
        &self,
        _subscription_id: i64,
        _secret: NewSecret,
        _now: DateTime<Utc>,
    ) -> Result<Option<WebhookSecret>> {
        panic!("unexpected WebhookStore::add_secret")
    }

    async fn retire_secret(
        &self,
        _subscription_id: i64,
        _secret_id: i64,
        _now: DateTime<Utc>,
    ) -> Result<bool> {
        panic!("unexpected WebhookStore::retire_secret")
    }

    async fn list_active_secrets(&self, _subscription_id: i64) -> Result<Vec<WebhookSecret>> {
        panic!("unexpected WebhookStore::list_active_secrets")
    }

    async fn append_event(
        &self,
        _event_type: &str,
        _payload: &str,
        _now: DateTime<Utc>,
    ) -> Result<OutboxEvent> {
        panic!("unexpected WebhookStore::append_event")
    }

    async fn append_limit_reached(
        &self,
        owner: &UserId,
        limit: &str,
        payload: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<OutboxEvent>> {
        assert_eq!(owner.as_str(), "alice");
        assert_eq!(limit, "repositories");
        let event: pyn_proto::Event = serde_json::from_str(payload).unwrap();
        assert_eq!(event.created_at, now);
        let pyn_proto::EventData::OwnerLimitReached(reached) = &event.data else {
            panic!("not a limit event")
        };
        assert_eq!(reached.owner_id.len(), 25);
        assert_eq!(
            event.data,
            pyn_proto::EventData::OwnerLimitReached(pyn_proto::OwnerLimitReachedData {
                owner: "alice".into(),
                owner_id: reached.owner_id.clone(),
                limit: pyn_proto::LimitKind::Repositories,
                value: 0,
            })
        );
        self.attempts.fetch_add(1, Ordering::SeqCst);
        Err(PynError::Storage("outbox unavailable".into()))
    }

    async fn claim_due(
        &self,
        _limit: u32,
        _lease: Duration,
        _now: DateTime<Utc>,
    ) -> Result<Vec<DueDelivery>> {
        panic!("unexpected WebhookStore::claim_due")
    }

    async fn mark_delivered(
        &self,
        _delivery_id: &str,
        _status_code: Option<i32>,
        _now: DateTime<Utc>,
    ) -> Result<Option<Delivery>> {
        panic!("unexpected WebhookStore::mark_delivered")
    }

    async fn record_failure(
        &self,
        _delivery_id: &str,
        _status_code: Option<i32>,
        _error: &str,
        _now: DateTime<Utc>,
        _jitter: &dyn Jitter,
    ) -> Result<Option<Delivery>> {
        panic!("unexpected WebhookStore::record_failure")
    }

    async fn list_deliveries(
        &self,
        _subscription_id: i64,
        _query: &DeliveryQuery,
    ) -> Result<Vec<Delivery>> {
        panic!("unexpected WebhookStore::list_deliveries")
    }

    async fn get_delivery(&self, _delivery_id: &str) -> Result<Option<Delivery>> {
        panic!("unexpected WebhookStore::get_delivery")
    }

    async fn redeliver(&self, _delivery_id: &str, _now: DateTime<Utc>) -> Result<Option<Delivery>> {
        panic!("unexpected WebhookStore::redeliver")
    }

    async fn purge(&self, _retention: Duration, _now: DateTime<Utc>) -> Result<u64> {
        panic!("unexpected WebhookStore::purge")
    }
}
