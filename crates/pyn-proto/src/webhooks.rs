//! Webhook subscription administration (`/v1/admin/webhooks`). The delivery payload is in `events`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// An active signing secret. Its value is shown once, when it is created, and never again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct WebhookSecretInfo {
    pub id: i64,
    pub created_at: DateTime<Utc>,
    /// Whether the server stores it encrypted (`PYN_SECRET_KEY` is set).
    pub encrypted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Webhook {
    pub id: i64,
    pub name: String,
    pub url: String,
    /// Event types delivered; empty means every type.
    pub event_types: Vec<String>,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// The account that created it.
    pub created_by: String,
    /// Active secrets, oldest first: one, or two while a rotation is in progress.
    pub secrets: Vec<WebhookSecretInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateWebhookRequest {
    /// 1 to 256 bytes, no control characters.
    pub name: String,
    /// `https` only (`http` when the operator allows it), no user name or password, no fragment, at most 2048 bytes.
    pub url: String,
    /// Event types to deliver, such as `repository.created`; empty or left out means every type.
    #[serde(default)]
    pub event_types: Vec<String>,
    /// Defaults to true.
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
}

fn enabled_by_default() -> bool {
    true
}

/// Fields left out stay as they are.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct UpdateWebhookRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    /// Replaces the list; an empty list means every type.
    #[serde(default)]
    pub event_types: Option<Vec<String>>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

#[derive(Clone, Serialize, Deserialize, ToSchema)]
pub struct CreatedWebhook {
    pub webhook: Webhook,
    /// The signing secret: `whsec_` and 64 hex characters. Shown once and never returned again.
    pub secret: String,
}

impl std::fmt::Debug for CreatedWebhook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CreatedWebhook")
            .field("webhook", &self.webhook)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Serialize, Deserialize, ToSchema)]
pub struct RotatedWebhookSecret {
    pub secret_id: i64,
    pub created_at: DateTime<Utc>,
    /// The new signing secret. Shown once and never returned again.
    pub secret: String,
}

impl std::fmt::Debug for RotatedWebhookSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RotatedWebhookSecret")
            .field("secret_id", &self.secret_id)
            .field("created_at", &self.created_at)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WebhookDeliveryStatus {
    Pending,
    Delivered,
    Failed,
}

/// One event's delivery to one subscription. The `id` is the `Pyn-Delivery` header and stays the same across retries and redeliveries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct WebhookDelivery {
    pub id: String,
    /// The queued event's id; the paging cursor.
    pub event_id: i64,
    pub status: WebhookDeliveryStatus,
    /// Attempts made since the last queueing or redelivery.
    pub attempts: u32,
    /// When the next attempt is due; meaningful while pending.
    pub next_attempt_at: DateTime<Utc>,
    /// The receiver's status code on the last attempt, if it answered.
    pub last_status_code: Option<i32>,
    /// Why the last attempt failed.
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub delivered_at: Option<DateTime<Utc>>,
    pub failed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct WebhookDeliveryPage {
    /// Newest event first.
    pub deliveries: Vec<WebhookDelivery>,
    /// Pass as `before` to fetch the next, older page; null on the last page.
    #[schema(required = true)]
    pub next_before: Option<i64>,
}
