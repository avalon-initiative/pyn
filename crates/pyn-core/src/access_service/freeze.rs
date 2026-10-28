//! Owner freeze: set by server administrators and service credentials, visible to the owner's members.

use super::*;
use crate::freeze::{FREEZE_REASON_BYTES, OwnerWrites};

/// Whether an owner is frozen, since when and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerFreezeState {
    pub id: AccountId,
    pub kind: AccountKind,
    pub frozen_at: Option<DateTime<Utc>>,
    pub reason: Option<String>,
}

impl From<&AccountRecord> for OwnerFreezeState {
    fn from(a: &AccountRecord) -> Self {
        Self {
            id: a.id.clone(),
            kind: a.kind,
            frozen_at: a.frozen_at,
            reason: a.frozen_reason.clone(),
        }
    }
}

fn frozen_error(account: &AccountRecord) -> PynError {
    PynError::OwnerFrozen {
        owner: account.user.to_string(),
        reason: account.frozen_reason.clone(),
    }
}

impl AccessService {
    async fn freeze_account(&self, owner: &UserId) -> Result<AccountRecord> {
        self.store
            .account(owner)
            .await?
            .ok_or_else(|| PynError::UserNotFound(owner.to_string()))
    }

    /// `OwnerFrozen` when the owner is frozen. An owner with no account record is a development identity and never is.
    pub async fn ensure_owner_writable(&self, owner: &UserId) -> Result<()> {
        match self.store.account(owner).await? {
            Some(account) if account.is_frozen() => Err(frozen_error(&account)),
            _ => Ok(()),
        }
    }

    /// Like `ensure_owner_writable`, for the owner of `repo`.
    pub(crate) async fn ensure_repo_writable(&self, repo: &RepoId) -> Result<()> {
        let Some(registry) = &self.registry else {
            return Ok(());
        };
        match registry.get_repo(repo).await? {
            Some(record) => self.ensure_owner_writable(&record.owner).await,
            None => Ok(()),
        }
    }

    /// Server administrators and service credentials with `manage_freeze` may see any owner; otherwise the user
    /// themself or a member of the organization. A name the caller may not see answers alike whether or not it exists.
    pub async fn require_freeze_view(
        &self,
        actor: &Identity,
        owner: &UserId,
    ) -> Result<AccountRecord> {
        if matches!(actor.credential, Credential::Service(_)) {
            self.require_admin_scope(actor, ServiceScope::ManageFreeze)
                .await?;
        } else if !self.is_server_admin(actor).await?
            && &actor.user != owner
            && self.store.org_role(owner, &actor.user).await?.is_none()
        {
            return Err(PynError::NotNamespaceOwner(owner.to_string()));
        }
        self.freeze_account(owner).await
    }

    pub async fn owner_freeze(&self, actor: &Identity, owner: &UserId) -> Result<OwnerFreezeState> {
        let account = self.require_freeze_view(actor, owner).await?;
        Ok((&account).into())
    }

    /// Freezing an owner that is already frozen replaces the reason and time. Administrators and `manage_freeze` only.
    pub async fn freeze_owner(
        &self,
        actor: &Identity,
        owner: &UserId,
        reason: &str,
    ) -> Result<OwnerFreezeState> {
        self.require_admin_scope(actor, ServiceScope::ManageFreeze)
            .await?;
        let reason = reason.trim();
        if reason.is_empty() || reason.len() > FREEZE_REASON_BYTES {
            return Err(PynError::InvalidRequest(format!(
                "a freeze needs a reason of 1 to {FREEZE_REASON_BYTES} bytes"
            )));
        }
        let account = self.freeze_account(owner).await?;
        let event = self.draft(DomainEvent::OwnerFrozen {
            owner: owner.clone(),
            owner_id: account.id.clone(),
            kind: account.kind,
            reason: reason.to_string(),
            actor: actor.user.clone(),
        })?;
        let frozen = Some((self.clock.now(), reason.to_string()));
        if !self
            .store
            .set_frozen_with_events(owner, frozen, &[event])
            .await?
        {
            return Err(PynError::UserNotFound(owner.to_string()));
        }
        self.record_freeze(
            &account,
            actor,
            AuditAction::OwnerFrozen,
            format!("{owner} frozen: {reason}"),
        )
        .await?;
        self.owner_freeze(actor, owner).await
    }

    /// Lifts a freeze; an owner that is not frozen is left as it is, with no audit entry or event.
    pub async fn unfreeze_owner(
        &self,
        actor: &Identity,
        owner: &UserId,
    ) -> Result<OwnerFreezeState> {
        self.require_admin_scope(actor, ServiceScope::ManageFreeze)
            .await?;
        let account = self.freeze_account(owner).await?;
        if !account.is_frozen() {
            return Ok((&account).into());
        }
        let event = self.draft(DomainEvent::OwnerUnfrozen {
            owner: owner.clone(),
            owner_id: account.id.clone(),
            kind: account.kind,
            actor: actor.user.clone(),
        })?;
        self.store
            .set_frozen_with_events(owner, None, &[event])
            .await?;
        self.record_freeze(
            &account,
            actor,
            AuditAction::OwnerUnfrozen,
            format!("{owner} unfrozen"),
        )
        .await?;
        self.owner_freeze(actor, owner).await
    }

    async fn record_freeze(
        &self,
        account: &AccountRecord,
        actor: &Identity,
        action: AuditAction,
        detail: String,
    ) -> Result<()> {
        if account.kind == AccountKind::Org {
            self.record(
                AuditScope::Org(account.user.clone()),
                &actor.user,
                action,
                detail.clone(),
            )
            .await?;
        }
        self.record_server(&actor.user, action, detail).await
    }
}

#[async_trait]
impl OwnerWrites for AccessService {
    async fn ensure_writable(&self, owner: &UserId) -> Result<()> {
        self.ensure_owner_writable(owner).await
    }
}
