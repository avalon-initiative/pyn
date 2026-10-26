use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use pyn_core::{
    AccountKind, DomainEvent, EventDraft, LimitEvents, LimitKind, LimitReached, PynError, Result,
    Visibility, WebhookStore,
};
use pyn_proto::{
    AccountApprovedData, AccountCreatedData, AccountDisabledData, AccountEnabledData, Event,
    EventData, LimitKind as WireLimitKind, Limits, OrganizationCreatedData,
    OrganizationDeletedData, OwnerKind, OwnerLimitReachedData, OwnerLimitsChangedData,
    RepositoryCreatedData, RepositoryDeletedData, RepositoryRenamedData,
};

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

impl WebhookLimitEvents {
    fn envelope(&self, data: EventData, now: DateTime<Utc>) -> Result<EventDraft> {
        let event_type = data.event_type().to_string();
        let event = Event::new(pyn_core::webhook::event_id()?, now, &self.public_url, data);
        let payload =
            serde_json::to_string(&event).map_err(|e| PynError::Storage(e.to_string()))?;
        Ok(EventDraft {
            event_type,
            payload,
            at: now,
        })
    }
}

fn name(user: pyn_core::UserId) -> String {
    user.to_string()
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
        let draft = self.envelope(
            EventData::OwnerLimitReached(OwnerLimitReachedData {
                owner: fact.owner.to_string(),
                limit: kind,
                value: fact.value,
            }),
            now,
        )?;
        self.store
            .append_limit_reached(&fact.owner, limit, &draft.payload, now)
            .await?;
        Ok(())
    }

    fn draft(&self, event: DomainEvent, now: DateTime<Utc>) -> Result<EventDraft> {
        let data = match event {
            DomainEvent::AccountCreated {
                user,
                pending_approval,
            } => EventData::AccountCreated(AccountCreatedData {
                user: name(user),
                pending_approval,
            }),
            DomainEvent::AccountApproved { user, actor } => {
                EventData::AccountApproved(AccountApprovedData {
                    user: name(user),
                    actor: name(actor),
                })
            }
            DomainEvent::AccountDisabled {
                user,
                actor,
                reason,
            } => EventData::AccountDisabled(AccountDisabledData {
                user: name(user),
                actor: name(actor),
                reason,
            }),
            DomainEvent::AccountEnabled { user, actor } => {
                EventData::AccountEnabled(AccountEnabledData {
                    user: name(user),
                    actor: name(actor),
                })
            }
            DomainEvent::OrganizationCreated {
                name: org,
                owner,
                actor,
            } => EventData::OrganizationCreated(OrganizationCreatedData {
                name: name(org),
                owner: name(owner),
                actor: name(actor),
            }),
            DomainEvent::OrganizationDeleted { name: org, actor } => {
                EventData::OrganizationDeleted(OrganizationDeletedData {
                    name: name(org),
                    actor: name(actor),
                })
            }
            DomainEvent::RepositoryCreated {
                owner,
                name: repo,
                visibility,
                actor,
            } => EventData::RepositoryCreated(RepositoryCreatedData {
                owner: name(owner),
                name: repo,
                visibility: match visibility {
                    Visibility::Public => pyn_proto::Visibility::Public,
                    Visibility::Private => pyn_proto::Visibility::Private,
                },
                actor: name(actor),
            }),
            DomainEvent::RepositoryRenamed {
                owner,
                name: repo,
                previous_name,
                actor,
            } => EventData::RepositoryRenamed(RepositoryRenamedData {
                owner: name(owner),
                name: repo,
                previous_name,
                actor: name(actor),
            }),
            DomainEvent::RepositoryDeleted {
                owner,
                name: repo,
                actor,
            } => EventData::RepositoryDeleted(RepositoryDeletedData {
                owner: name(owner),
                name: repo,
                actor: name(actor),
            }),
            DomainEvent::OwnerLimitsChanged {
                owner,
                kind,
                limits,
                actor,
            } => EventData::OwnerLimitsChanged(OwnerLimitsChangedData {
                owner: name(owner),
                kind: match kind {
                    AccountKind::User => OwnerKind::User,
                    AccountKind::Org => OwnerKind::Org,
                },
                limits: Limits {
                    repositories: limits.repositories,
                    members: limits.members,
                    storage_bytes: limits.storage_bytes,
                },
                actor: name(actor),
            }),
        };
        self.envelope(data, now)
    }
}
