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

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreatedWebhook {
    pub webhook: Webhook,
    /// The signing secret: `whsec_` and 64 hex characters. Shown once and never returned again.
    pub secret: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RotatedWebhookSecret {
    pub secret_id: i64,
    pub created_at: DateTime<Utc>,
    /// The new signing secret. Shown once and never returned again.
    pub secret: String,
}
