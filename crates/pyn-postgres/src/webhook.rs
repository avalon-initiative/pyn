use std::str::FromStr;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use pyn_core::audit::brief;
use pyn_core::{
    Delivery, DeliveryQuery, DeliveryStatus, DueDelivery, Jitter, MAX_ACTIVE_SECRETS, NewSecret,
    NewSubscription, OutboxEvent, PynError, Result, Subscription, SubscriptionUpdate, UserId,
    WebhookSecret, WebhookStore, retry_delay,
};
use sqlx::Row;
use sqlx::postgres::PgRow;

use crate::{PgMetadataStore, db};

const SUBSCRIPTION: &str =
    "id, name, url, event_types, enabled, created_at, updated_at, created_by";
const SECRET: &str = "id, subscription_id, secret, encrypted, key_id, created_at, retired_at";
const DELIVERY: &str = "id::text AS id, subscription_id, event_id, attempt_count, next_attempt_at, claimed_until, status, last_status_code, last_error, created_at, delivered_at, failed_at";

fn subscription_from(row: &PgRow) -> Subscription {
    Subscription {
        id: row.get("id"),
        name: row.get("name"),
        url: row.get("url"),
        event_types: row.get("event_types"),
        enabled: row.get("enabled"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
        created_by: UserId::new(row.get::<String, _>("created_by")),
    }
}

fn secret_from(row: &PgRow) -> WebhookSecret {
    WebhookSecret {
        id: row.get("id"),
        subscription_id: row.get("subscription_id"),
        secret: row.get("secret"),
        encrypted: row.get("encrypted"),
        key_id: row.get("key_id"),
        created_at: row.get("created_at"),
        retired_at: row.get("retired_at"),
    }
}

fn delivery_from(row: &PgRow) -> Result<Delivery> {
    Ok(Delivery {
        id: row.get("id"),
        subscription_id: row.get("subscription_id"),
        event_id: row.get("event_id"),
        attempt_count: row.get::<i32, _>("attempt_count") as u32,
        next_attempt_at: row.get("next_attempt_at"),
        claimed_until: row.get("claimed_until"),
        status: DeliveryStatus::from_str(row.get("status"))?,
        last_status_code: row.get("last_status_code"),
        last_error: row.get("last_error"),
        created_at: row.get("created_at"),
        delivered_at: row.get("delivered_at"),
        failed_at: row.get("failed_at"),
    })
}

/// True for the canonical 8-4-4-4-12 hex form, so a malformed id reads as absent instead of a cast error.
fn is_uuid(s: &str) -> bool {
    s.len() == 36
        && s.chars().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

async fn append_event_in(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    event_type: &str,
    payload: &str,
    now: Option<DateTime<Utc>>,
) -> Result<OutboxEvent> {
    let row = sqlx::query(
        "INSERT INTO webhook_events (event_type, payload, created_at)
             VALUES ($1, $2::text::jsonb, COALESCE($3::timestamptz, now()))
             RETURNING id, event_type, payload::text AS payload, created_at",
    )
    .bind(event_type)
    .bind(payload)
    .bind(now)
    .fetch_one(&mut **tx)
    .await
    .map_err(db)?;
    let event = OutboxEvent {
        id: row.get("id"),
        event_type: row.get("event_type"),
        payload: row.get("payload"),
        created_at: row.get("created_at"),
    };
    sqlx::query(
        "INSERT INTO webhook_deliveries (subscription_id, event_id, next_attempt_at, created_at)
             SELECT id, $1, $3, $3 FROM webhook_subscriptions
             WHERE enabled AND (cardinality(event_types) = 0 OR $2 = ANY(event_types))",
    )
    .bind(event.id)
    .bind(event_type)
    .bind(event.created_at)
    .execute(&mut **tx)
    .await
    .map_err(db)?;
    Ok(event)
}

impl PgMetadataStore {
    async fn delivery_by_id(&self, id: &str) -> Result<Option<Delivery>> {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {DELIVERY} FROM webhook_deliveries WHERE id = $1::uuid"
        )))
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?
        .map(|r| delivery_from(&r))
        .transpose()
    }
}

#[async_trait]
impl WebhookStore for PgMetadataStore {
    async fn create_subscription(
        &self,
        new: NewSubscription,
        now: DateTime<Utc>,
    ) -> Result<Subscription> {
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "INSERT INTO webhook_subscriptions (name, url, event_types, enabled, created_at, updated_at, created_by)
             VALUES ($1, $2, $3, $4, COALESCE($5::timestamptz, now()), COALESCE($5::timestamptz, now()), $6)
             RETURNING {SUBSCRIPTION}"
        )))
        .bind(&new.name)
        .bind(&new.url)
        .bind(&new.event_types)
        .bind(new.enabled)
        .bind(self.bound_now(now))
        .bind(new.created_by.as_str())
        .fetch_one(&self.pool)
        .await
        .map_err(db)?;
        Ok(subscription_from(&row))
    }

    async fn get_subscription(&self, id: i64) -> Result<Option<Subscription>> {
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {SUBSCRIPTION} FROM webhook_subscriptions WHERE id = $1"
        )))
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?;
        Ok(row.as_ref().map(subscription_from))
    }

    async fn list_subscriptions(&self) -> Result<Vec<Subscription>> {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {SUBSCRIPTION} FROM webhook_subscriptions ORDER BY id"
        )))
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        Ok(rows.iter().map(subscription_from).collect())
    }

    async fn update_subscription(
        &self,
        id: i64,
        update: SubscriptionUpdate,
        now: DateTime<Utc>,
    ) -> Result<Option<Subscription>> {
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE webhook_subscriptions SET
                 name = COALESCE($2, name),
                 url = COALESCE($3, url),
                 event_types = COALESCE($4, event_types),
                 enabled = COALESCE($5, enabled),
                 updated_at = COALESCE($6::timestamptz, now())
             WHERE id = $1 RETURNING {SUBSCRIPTION}"
        )))
        .bind(id)
        .bind(&update.name)
        .bind(&update.url)
        .bind(&update.event_types)
        .bind(update.enabled)
        .bind(self.bound_now(now))
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?;
        Ok(row.as_ref().map(subscription_from))
    }

    async fn delete_subscription(&self, id: i64) -> Result<bool> {
        let done = sqlx::query("DELETE FROM webhook_subscriptions WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(done.rows_affected() > 0)
    }

    async fn add_secret(
        &self,
        subscription_id: i64,
        secret: NewSecret,
        now: DateTime<Utc>,
    ) -> Result<Option<WebhookSecret>> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        // The row lock serializes additions so the active count cannot race past the cap.
        let locked: Option<i64> =
            sqlx::query_scalar("SELECT id FROM webhook_subscriptions WHERE id = $1 FOR UPDATE")
                .bind(subscription_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(db)?;
        if locked.is_none() {
            return Ok(None);
        }
        let active: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM webhook_secrets WHERE subscription_id = $1 AND retired_at IS NULL",
        )
        .bind(subscription_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(db)?;
        if active as usize >= MAX_ACTIVE_SECRETS {
            return Err(PynError::InvalidRequest(format!(
                "a webhook has at most {MAX_ACTIVE_SECRETS} active secrets; retire one first"
            )));
        }
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "INSERT INTO webhook_secrets (subscription_id, secret, encrypted, key_id, created_at)
             VALUES ($1, $2, $3, $4, COALESCE($5::timestamptz, now())) RETURNING {SECRET}"
        )))
        .bind(subscription_id)
        .bind(&secret.secret)
        .bind(secret.encrypted)
        .bind(&secret.key_id)
        .bind(self.bound_now(now))
        .fetch_one(&mut *tx)
        .await
        .map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(Some(secret_from(&row)))
    }

    async fn retire_secret(
        &self,
        subscription_id: i64,
        secret_id: i64,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        let done = sqlx::query(
            "UPDATE webhook_secrets SET retired_at = COALESCE($3::timestamptz, now())
             WHERE id = $1 AND subscription_id = $2 AND retired_at IS NULL",
        )
        .bind(secret_id)
        .bind(subscription_id)
        .bind(self.bound_now(now))
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(done.rows_affected() > 0)
    }

    async fn list_active_secrets(&self, subscription_id: i64) -> Result<Vec<WebhookSecret>> {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {SECRET} FROM webhook_secrets
             WHERE subscription_id = $1 AND retired_at IS NULL ORDER BY id"
        )))
        .bind(subscription_id)
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        Ok(rows.iter().map(secret_from).collect())
    }

    async fn append_event(
        &self,
        event_type: &str,
        payload: &str,
        now: DateTime<Utc>,
    ) -> Result<OutboxEvent> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let event = append_event_in(&mut tx, event_type, payload, self.bound_now(now)).await?;
        tx.commit().await.map_err(db)?;
        Ok(event)
    }

    async fn append_limit_reached(
        &self,
        owner: &UserId,
        limit: &str,
        payload: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<OutboxEvent>> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let admitted = sqlx::query(
            "INSERT INTO webhook_limit_events (owner, limit_kind, last_event_at)
             VALUES ($1, $2, COALESCE($3, now()))
             ON CONFLICT (owner, limit_kind) DO UPDATE SET last_event_at = EXCLUDED.last_event_at
             WHERE webhook_limit_events.last_event_at <= EXCLUDED.last_event_at - interval '1 hour'
             RETURNING owner",
        )
        .bind(owner.as_str())
        .bind(limit)
        .bind(self.bound_now(now))
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?;
        if admitted.is_none() {
            return Ok(None);
        }
        let event =
            append_event_in(&mut tx, "owner.limit_reached", payload, self.bound_now(now)).await?;
        tx.commit().await.map_err(db)?;
        Ok(Some(event))
    }

    async fn claim_due(
        &self,
        limit: u32,
        lease: Duration,
        now: DateTime<Utc>,
    ) -> Result<Vec<DueDelivery>> {
        // SKIP LOCKED lets concurrent claimers pass over each other's rows; the lease covers a claimer that dies.
        let rows = sqlx::query(
            "WITH t AS (SELECT COALESCE($1::timestamptz, now()) AS now),
             due AS (
                 SELECT d.id FROM webhook_deliveries d
                 JOIN webhook_subscriptions s ON s.id = d.subscription_id, t
                 WHERE d.status = 'pending' AND s.enabled
                   AND d.next_attempt_at <= t.now
                   AND (d.claimed_until IS NULL OR d.claimed_until <= t.now)
                 ORDER BY d.next_attempt_at, d.event_id
                 LIMIT $2
                 FOR UPDATE OF d SKIP LOCKED
             ),
             claimed AS (
                 UPDATE webhook_deliveries d
                 SET claimed_until = t.now + make_interval(secs => $3)
                 FROM due, t WHERE d.id = due.id
                 RETURNING d.id::text AS id, d.subscription_id, d.event_id, d.attempt_count, d.next_attempt_at,
                           d.claimed_until, d.status, d.last_status_code, d.last_error, d.created_at,
                           d.delivered_at, d.failed_at
             )
             SELECT c.*, e.event_type, e.payload::text AS payload, e.created_at AS event_created_at,
                    s.name, s.url, s.event_types, s.enabled,
                    s.created_at AS sub_created_at, s.updated_at AS sub_updated_at, s.created_by
             FROM claimed c
             JOIN webhook_events e ON e.id = c.event_id
             JOIN webhook_subscriptions s ON s.id = c.subscription_id
             ORDER BY c.next_attempt_at, c.event_id"
        )
        .bind(self.bound_now(now))
        .bind(i64::from(limit))
        .bind(lease.num_milliseconds() as f64 / 1000.0)
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        rows.iter()
            .map(|r| {
                let delivery = delivery_from(r)?;
                Ok(DueDelivery {
                    event: OutboxEvent {
                        id: delivery.event_id,
                        event_type: r.get("event_type"),
                        payload: r.get("payload"),
                        created_at: r.get("event_created_at"),
                    },
                    subscription: Subscription {
                        id: delivery.subscription_id,
                        name: r.get("name"),
                        url: r.get("url"),
                        event_types: r.get("event_types"),
                        enabled: r.get("enabled"),
                        created_at: r.get("sub_created_at"),
                        updated_at: r.get("sub_updated_at"),
                        created_by: UserId::new(r.get::<String, _>("created_by")),
                    },
                    delivery,
                })
            })
            .collect()
    }

    async fn mark_delivered(
        &self,
        delivery_id: &str,
        status_code: Option<i32>,
        now: DateTime<Utc>,
    ) -> Result<Option<Delivery>> {
        if !is_uuid(delivery_id) {
            return Ok(None);
        }
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE webhook_deliveries SET status = 'delivered', attempt_count = attempt_count + 1,
                 last_status_code = $2, last_error = NULL, claimed_until = NULL,
                 delivered_at = COALESCE($3::timestamptz, now())
             WHERE id = $1::uuid AND status = 'pending' RETURNING {DELIVERY}"
        )))
        .bind(delivery_id)
        .bind(status_code)
        .bind(self.bound_now(now))
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?;
        match row {
            Some(r) => Ok(Some(delivery_from(&r)?)),
            None => self.delivery_by_id(delivery_id).await,
        }
    }

    async fn record_failure(
        &self,
        delivery_id: &str,
        status_code: Option<i32>,
        error: &str,
        now: DateTime<Utc>,
        jitter: &dyn Jitter,
    ) -> Result<Option<Delivery>> {
        if !is_uuid(delivery_id) {
            return Ok(None);
        }
        let mut tx = self.pool.begin().await.map_err(db)?;
        let current: Option<(i32, String)> = sqlx::query_as(
            "SELECT attempt_count, status FROM webhook_deliveries WHERE id = $1::uuid FOR UPDATE",
        )
        .bind(delivery_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?;
        let Some((attempts, status)) = current else {
            return Ok(None);
        };
        if status != DeliveryStatus::Pending.as_str() {
            tx.rollback().await.map_err(db)?;
            return self.delivery_by_id(delivery_id).await;
        }
        let attempt = attempts as u32 + 1;
        let wait = retry_delay(attempt, jitter.unit());
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE webhook_deliveries SET attempt_count = $2, last_status_code = $3, last_error = $4,
                 claimed_until = NULL,
                 status = CASE WHEN $5::float8 IS NULL THEN 'failed' ELSE 'pending' END,
                 failed_at = CASE WHEN $5::float8 IS NULL THEN COALESCE($6::timestamptz, now()) END,
                 next_attempt_at = CASE WHEN $5::float8 IS NULL THEN next_attempt_at
                                        ELSE COALESCE($6::timestamptz, now()) + make_interval(secs => $5::float8) END
             WHERE id = $1::uuid RETURNING {DELIVERY}"
        )))
        .bind(delivery_id)
        .bind(attempt as i32)
        .bind(status_code)
        .bind(brief(error))
        .bind(wait.map(|w| w.num_milliseconds() as f64 / 1000.0))
        .bind(self.bound_now(now))
        .fetch_one(&mut *tx)
        .await
        .map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(Some(delivery_from(&row)?))
    }

    async fn list_deliveries(
        &self,
        subscription_id: i64,
        query: &DeliveryQuery,
    ) -> Result<Vec<Delivery>> {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {DELIVERY} FROM webhook_deliveries
             WHERE subscription_id = $1
               AND ($2::text IS NULL OR status = $2)
               AND ($3::bigint IS NULL OR event_id < $3)
             ORDER BY event_id DESC LIMIT $4"
        )))
        .bind(subscription_id)
        .bind(query.status.map(DeliveryStatus::as_str))
        .bind(query.before_event)
        .bind(i64::from(query.limit))
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        rows.iter().map(delivery_from).collect()
    }

    async fn redeliver(&self, delivery_id: &str, now: DateTime<Utc>) -> Result<Option<Delivery>> {
        if !is_uuid(delivery_id) {
            return Ok(None);
        }
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "UPDATE webhook_deliveries SET status = 'pending', attempt_count = 0,
                 next_attempt_at = COALESCE($2::timestamptz, now()), claimed_until = NULL,
                 last_status_code = NULL, last_error = NULL, delivered_at = NULL, failed_at = NULL
             WHERE id = $1::uuid RETURNING {DELIVERY}"
        )))
        .bind(delivery_id)
        .bind(self.bound_now(now))
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?;
        row.as_ref().map(delivery_from).transpose()
    }

    async fn purge(&self, retention: Duration, now: DateTime<Utc>) -> Result<u64> {
        let done = sqlx::query(
            "DELETE FROM webhook_events
             WHERE created_at < COALESCE($1::timestamptz, now()) - make_interval(secs => $2)",
        )
        .bind(self.bound_now(now))
        .bind(retention.num_milliseconds() as f64 / 1000.0)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(done.rows_affected())
    }
}
