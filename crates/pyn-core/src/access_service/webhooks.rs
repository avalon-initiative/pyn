//! Webhook subscription administration: server administrators and `manage_webhooks` credentials.

use super::*;
use crate::webhook::{
    MAX_ACTIVE_SECRETS, NewSubscription, Subscription, SubscriptionUpdate, WebhookSecret,
    WebhookStore,
};
use crate::webhook_secret::{SecretKey, generate_secret, store_secret};
use crate::webhook_url::{origin_of, validate_event_types, validate_url};

/// Operator settings for webhook subscriptions.
#[derive(Debug, Clone, Default)]
pub struct WebhookSettings {
    /// Encrypts signing secrets at rest when set.
    pub secret_key: Option<SecretKey>,
    /// Accept `http` subscription URLs.
    pub allow_http: bool,
}

/// What an administrator may know of a stored secret: never the value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookSecretInfo {
    pub id: i64,
    pub created_at: DateTime<Utc>,
    pub encrypted: bool,
}

impl From<&WebhookSecret> for WebhookSecretInfo {
    fn from(s: &WebhookSecret) -> Self {
        Self {
            id: s.id,
            created_at: s.created_at,
            encrypted: s.encrypted,
        }
    }
}

/// A subscription with its active secrets, oldest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookDetail {
    pub subscription: Subscription,
    pub secrets: Vec<WebhookSecretInfo>,
}

/// What an administrator supplies to create a subscription.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateWebhook {
    pub name: String,
    pub url: String,
    /// Empty means every event type.
    pub event_types: Vec<String>,
    pub enabled: bool,
}

/// A secret just generated: the only time its value is available.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuedSecret {
    pub info: WebhookSecretInfo,
    pub value: String,
}

impl AccessService {
    /// Where subscriptions and secrets live; the default is in memory.
    pub fn with_webhooks(
        mut self,
        store: Arc<dyn WebhookStore>,
        settings: WebhookSettings,
    ) -> Self {
        self.webhooks = store;
        self.webhook_settings = settings;
        self
    }

    pub fn webhook_secret_key(&self) -> Option<&SecretKey> {
        self.webhook_settings.secret_key.as_ref()
    }

    async fn issue_secret(&self, subscription_id: i64) -> Result<Option<IssuedSecret>> {
        let value = generate_secret()?;
        let stored = store_secret(self.webhook_secret_key(), &value)?;
        Ok(self
            .webhooks
            .add_secret(subscription_id, stored, self.clock.now())
            .await?
            .map(|row| IssuedSecret {
                info: (&row).into(),
                value,
            }))
    }

    async fn detail(&self, subscription: Subscription) -> Result<WebhookDetail> {
        let secrets = self.webhooks.list_active_secrets(subscription.id).await?;
        Ok(WebhookDetail {
            subscription,
            secrets: secrets.iter().map(Into::into).collect(),
        })
    }

    async fn require_webhook(&self, id: i64) -> Result<Subscription> {
        self.webhooks
            .get_subscription(id)
            .await?
            .ok_or(PynError::WebhookNotFound(id))
    }

    /// Creates a subscription with its first secret, returned here and never again.
    pub async fn create_webhook(
        &self,
        actor: &Identity,
        req: CreateWebhook,
    ) -> Result<(WebhookDetail, IssuedSecret)> {
        self.require_admin_scope(actor, ServiceScope::ManageWebhooks)
            .await?;
        let new = NewSubscription {
            name: validate_label("webhook name", &req.name)?,
            url: validate_url(&req.url, self.webhook_settings.allow_http)?,
            event_types: validate_event_types(&req.event_types)?,
            enabled: req.enabled,
            created_by: actor.user.clone(),
        };
        let sub = self
            .webhooks
            .create_subscription(new, self.clock.now())
            .await?;
        let issued = match self.issue_secret(sub.id).await {
            Ok(Some(issued)) => issued,
            failed => {
                self.webhooks.delete_subscription(sub.id).await?;
                return Err(failed.err().unwrap_or(PynError::WebhookNotFound(sub.id)));
            }
        };
        self.record_server(
            &actor.user,
            AuditAction::WebhookCreated,
            format!("webhook {} ({}): {}", sub.id, sub.name, origin_of(&sub.url)),
        )
        .await?;
        let detail = self.detail(sub).await?;
        Ok((detail, issued))
    }

    /// Subscriptions in creation order.
    pub async fn list_webhooks(&self, actor: &Identity) -> Result<Vec<WebhookDetail>> {
        self.require_admin_scope(actor, ServiceScope::ManageWebhooks)
            .await?;
        let mut out = Vec::new();
        for sub in self.webhooks.list_subscriptions().await? {
            out.push(self.detail(sub).await?);
        }
        Ok(out)
    }

    pub async fn get_webhook(&self, actor: &Identity, id: i64) -> Result<WebhookDetail> {
        self.require_admin_scope(actor, ServiceScope::ManageWebhooks)
            .await?;
        let sub = self.require_webhook(id).await?;
        self.detail(sub).await
    }

    /// Changes the fields set in `update`; the audit entry names the fields, never a value.
    pub async fn update_webhook(
        &self,
        actor: &Identity,
        id: i64,
        update: SubscriptionUpdate,
    ) -> Result<WebhookDetail> {
        self.require_admin_scope(actor, ServiceScope::ManageWebhooks)
            .await?;
        let mut changed = Vec::new();
        let update = SubscriptionUpdate {
            name: update
                .name
                .map(|n| validate_label("webhook name", &n))
                .transpose()?,
            url: update
                .url
                .map(|u| validate_url(&u, self.webhook_settings.allow_http))
                .transpose()?,
            event_types: update
                .event_types
                .map(|t| validate_event_types(&t))
                .transpose()?,
            enabled: update.enabled,
        };
        for (field, set) in [
            ("name", update.name.is_some()),
            ("url", update.url.is_some()),
            ("event_types", update.event_types.is_some()),
            ("enabled", update.enabled.is_some()),
        ] {
            if set {
                changed.push(field);
            }
        }
        if changed.is_empty() {
            return Err(PynError::InvalidRequest(
                "nothing to change: send at least one of name, url, event_types, enabled".into(),
            ));
        }
        let sub = self
            .webhooks
            .update_subscription(id, update, self.clock.now())
            .await?
            .ok_or(PynError::WebhookNotFound(id))?;
        self.record_server(
            &actor.user,
            AuditAction::WebhookUpdated,
            format!(
                "webhook {} ({}): changed {}",
                sub.id,
                sub.name,
                changed.join(", ")
            ),
        )
        .await?;
        self.detail(sub).await
    }

    /// Deletes the subscription with its secrets and deliveries.
    pub async fn delete_webhook(&self, actor: &Identity, id: i64) -> Result<()> {
        self.require_admin_scope(actor, ServiceScope::ManageWebhooks)
            .await?;
        let sub = self.require_webhook(id).await?;
        if !self.webhooks.delete_subscription(id).await? {
            return Err(PynError::WebhookNotFound(id));
        }
        self.record_server(
            &actor.user,
            AuditAction::WebhookDeleted,
            format!("webhook {} ({}): {}", sub.id, sub.name, origin_of(&sub.url)),
        )
        .await
    }

    /// Adds a second active secret, returned here and never again; the old one keeps signing until it is retired.
    pub async fn rotate_webhook_secret(&self, actor: &Identity, id: i64) -> Result<IssuedSecret> {
        self.require_admin_scope(actor, ServiceScope::ManageWebhooks)
            .await?;
        let sub = self.require_webhook(id).await?;
        if self.webhooks.list_active_secrets(id).await?.len() >= MAX_ACTIVE_SECRETS {
            return Err(PynError::WebhookSecretLimit(MAX_ACTIVE_SECRETS));
        }
        let issued = self
            .issue_secret(id)
            .await?
            .ok_or(PynError::WebhookNotFound(id))?;
        self.record_server(
            &actor.user,
            AuditAction::WebhookSecretRotated,
            format!(
                "webhook {} ({}): secret {} added",
                sub.id, sub.name, issued.info.id
            ),
        )
        .await?;
        Ok(issued)
    }

    /// Stops signing with one active secret; the last active one cannot be retired.
    pub async fn retire_webhook_secret(
        &self,
        actor: &Identity,
        id: i64,
        secret_id: i64,
    ) -> Result<()> {
        self.require_admin_scope(actor, ServiceScope::ManageWebhooks)
            .await?;
        let sub = self.require_webhook(id).await?;
        let active = self.webhooks.list_active_secrets(id).await?;
        if !active.iter().any(|s| s.id == secret_id) {
            return Err(PynError::WebhookSecretNotFound {
                webhook: id,
                secret: secret_id,
            });
        }
        if active.len() < 2 {
            return Err(PynError::WebhookLastSecret);
        }
        if !self
            .webhooks
            .retire_secret(id, secret_id, self.clock.now())
            .await?
        {
            return Err(PynError::WebhookSecretNotFound {
                webhook: id,
                secret: secret_id,
            });
        }
        self.record_server(
            &actor.user,
            AuditAction::WebhookSecretRetired,
            format!(
                "webhook {} ({}): secret {} retired",
                sub.id, sub.name, secret_id
            ),
        )
        .await
    }
}
