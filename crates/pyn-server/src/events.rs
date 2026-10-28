use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use pyn_core::{
    AccountKind, DomainEvent, EventDraft, LimitEvents, LimitKind, LimitReached, PynError, Result,
    Visibility, WebhookStore,
};
use pyn_proto::{
    AccountApprovedData, AccountCreatedData, AccountDisabledData, AccountEnabledData, Event,
    EventData, LimitKind as WireLimitKind, Limits, LockAcquiredData, LockExpiredData,
    LockForcedData, LockReleasedData, OrganizationCreatedData, OrganizationDeletedData,
    OwnerFrozenData, OwnerKind, OwnerLimitReachedData, OwnerLimitsChangedData, OwnerUnfrozenData,
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

fn owner_kind(kind: AccountKind) -> OwnerKind {
    match kind {
        AccountKind::User => OwnerKind::User,
        AccountKind::Org => OwnerKind::Org,
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
                owner_id: fact.owner_id.to_string(),
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
                user_id,
                pending_approval,
            } => EventData::AccountCreated(AccountCreatedData {
                user: name(user),
                user_id: user_id.to_string(),
                pending_approval,
            }),
            DomainEvent::AccountApproved {
                user,
                user_id,
                actor,
            } => EventData::AccountApproved(AccountApprovedData {
                user: name(user),
                user_id: user_id.to_string(),
                actor: name(actor),
            }),
            DomainEvent::AccountDisabled {
                user,
                user_id,
                actor,
                reason,
            } => EventData::AccountDisabled(AccountDisabledData {
                user: name(user),
                user_id: user_id.to_string(),
                actor: name(actor),
                reason,
            }),
            DomainEvent::AccountEnabled {
                user,
                user_id,
                actor,
            } => EventData::AccountEnabled(AccountEnabledData {
                user: name(user),
                user_id: user_id.to_string(),
                actor: name(actor),
            }),
            DomainEvent::OrganizationCreated {
                name: org,
                org_id,
                owner,
                owner_id,
                actor,
            } => EventData::OrganizationCreated(OrganizationCreatedData {
                name: name(org),
                org_id: org_id.to_string(),
                owner: name(owner),
                owner_id: owner_id.to_string(),
                actor: name(actor),
            }),
            DomainEvent::OrganizationDeleted {
                name: org,
                org_id,
                actor,
            } => EventData::OrganizationDeleted(OrganizationDeletedData {
                name: name(org),
                org_id: org_id.to_string(),
                actor: name(actor),
            }),
            DomainEvent::RepositoryCreated {
                repo_id,
                owner,
                owner_id,
                name: repo,
                visibility,
                actor,
            } => EventData::RepositoryCreated(RepositoryCreatedData {
                repo_id: repo_id.to_string(),
                owner: name(owner),
                owner_id: owner_id.to_string(),
                name: repo,
                visibility: match visibility {
                    Visibility::Public => pyn_proto::Visibility::Public,
                    Visibility::Private => pyn_proto::Visibility::Private,
                },
                actor: name(actor),
            }),
            DomainEvent::RepositoryRenamed {
                repo_id,
                owner,
                owner_id,
                name: repo,
                previous_name,
                actor,
            } => EventData::RepositoryRenamed(RepositoryRenamedData {
                repo_id: repo_id.to_string(),
                owner: name(owner),
                owner_id: owner_id.to_string(),
                name: repo,
                previous_name,
                actor: name(actor),
            }),
            DomainEvent::RepositoryDeleted {
                repo_id,
                owner,
                owner_id,
                name: repo,
                actor,
            } => EventData::RepositoryDeleted(RepositoryDeletedData {
                repo_id: repo_id.to_string(),
                owner: name(owner),
                owner_id: owner_id.to_string(),
                name: repo,
                actor: name(actor),
            }),
            DomainEvent::OwnerLimitsChanged {
                owner,
                owner_id,
                kind,
                limits,
                actor,
            } => EventData::OwnerLimitsChanged(OwnerLimitsChangedData {
                owner: name(owner),
                owner_id: owner_id.to_string(),
                kind: owner_kind(kind),
                limits: Limits {
                    repositories: limits.repositories,
                    members: limits.members,
                    storage_bytes: limits.storage_bytes,
                },
                actor: name(actor),
            }),
            DomainEvent::OwnerFrozen {
                owner,
                owner_id,
                kind,
                reason,
                actor,
            } => EventData::OwnerFrozen(OwnerFrozenData {
                owner: name(owner),
                owner_id: owner_id.to_string(),
                kind: owner_kind(kind),
                reason,
                actor: name(actor),
            }),
            DomainEvent::OwnerUnfrozen {
                owner,
                owner_id,
                kind,
                actor,
            } => EventData::OwnerUnfrozen(OwnerUnfrozenData {
                owner: name(owner),
                owner_id: owner_id.to_string(),
                kind: owner_kind(kind),
                actor: name(actor),
            }),
            DomainEvent::LockAcquired {
                owner,
                owner_id,
                repo,
                repo_id,
                path,
                holder,
                expires_at,
            } => EventData::LockAcquired(LockAcquiredData {
                owner: name(owner),
                owner_id: owner_id.to_string(),
                repo,
                repo_id: repo_id.to_string(),
                path: path.to_string(),
                holder: name(holder),
                expires_at,
            }),
            DomainEvent::LockReleased {
                owner,
                owner_id,
                repo,
                repo_id,
                path,
                holder,
                reason,
            } => EventData::LockReleased(LockReleasedData {
                owner: name(owner),
                owner_id: owner_id.to_string(),
                repo,
                repo_id: repo_id.to_string(),
                path: path.to_string(),
                holder: name(holder),
                reason,
            }),
            DomainEvent::LockExpired {
                owner,
                owner_id,
                repo,
                repo_id,
                path,
                holder,
                expires_at,
            } => EventData::LockExpired(LockExpiredData {
                owner: name(owner),
                owner_id: owner_id.to_string(),
                repo,
                repo_id: repo_id.to_string(),
                path: path.to_string(),
                holder: name(holder),
                expires_at,
            }),
            DomainEvent::LockForced {
                owner,
                owner_id,
                repo,
                repo_id,
                path,
                holder,
                actor,
                reason,
            } => EventData::LockForced(LockForcedData {
                owner: name(owner),
                owner_id: owner_id.to_string(),
                repo,
                repo_id: repo_id.to_string(),
                path: path.to_string(),
                holder: name(holder),
                actor: name(actor),
                reason,
            }),
        };
        self.envelope(data, now)
    }
}
