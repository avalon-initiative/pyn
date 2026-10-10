//! Per-owner limits: set by server administrators and service credentials, visible to the owner.

use super::*;

/// An owner's own limits and the ones in force once the server default fills the gaps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OwnerLimits {
    pub kind: AccountKind,
    pub own: Limits,
    pub effective: Limits,
}

fn show(value: Option<u64>) -> String {
    value.map_or_else(|| "default".to_string(), |v| v.to_string())
}

impl AccessService {
    pub fn default_limits(&self) -> Limits {
        self.config.default_limits
    }

    /// The limits in force for the owner: its own, else the server default; all unset means unlimited.
    pub async fn effective_limits(&self, owner: &UserId) -> Result<Limits> {
        Ok(self
            .store
            .owner_limits(owner)
            .await?
            .or(self.config.default_limits))
    }

    /// The number of members of an organization.
    pub async fn member_count(&self, org: &UserId) -> Result<u64> {
        Ok(self.store.org_members(org).await?.len() as u64)
    }

    async fn limits_of(&self, account: &AccountRecord) -> Result<OwnerLimits> {
        let own = self.store.owner_limits(&account.user).await?;
        let mut effective = own.or(self.config.default_limits);
        if account.kind == AccountKind::User {
            effective.members = None;
        }
        Ok(OwnerLimits {
            kind: account.kind,
            own,
            effective,
        })
    }

    async fn limits_account(&self, owner: &UserId) -> Result<AccountRecord> {
        self.store
            .account(owner)
            .await?
            .ok_or_else(|| PynError::UserNotFound(owner.to_string()))
    }

    /// Server administrators and service credentials with `manage_limits` may see any owner; otherwise only the
    /// user themself or an owner of the organization.
    pub async fn require_limits_view(
        &self,
        actor: &Identity,
        owner: &UserId,
    ) -> Result<AccountRecord> {
        let account = self.limits_account(owner).await?;
        if matches!(actor.credential, Credential::Service(_)) {
            self.require_admin_scope(actor, ServiceScope::ManageLimits)
                .await?;
        } else if !self.is_server_admin(actor).await? {
            match account.kind {
                AccountKind::User if &actor.user != owner => {
                    return Err(PynError::NotNamespaceOwner(owner.to_string()));
                }
                AccountKind::Org => self.require_org_owner(owner, &actor.user).await?,
                AccountKind::User => {}
            }
        }
        Ok(account)
    }

    /// The owner's limits, for the owner or an administrator.
    pub async fn owner_limits(&self, actor: &Identity, owner: &UserId) -> Result<OwnerLimits> {
        let account = self.require_limits_view(actor, owner).await?;
        self.limits_of(&account).await
    }

    /// Every owner with limits of their own. Server administrators and `manage_limits` only.
    pub async fn list_owner_limits(&self, actor: &Identity) -> Result<Vec<(UserId, OwnerLimits)>> {
        self.require_admin_scope(actor, ServiceScope::ManageLimits)
            .await?;
        let mut out = Vec::new();
        for (owner, _) in self.store.list_owner_limits().await? {
            if let Some(account) = self.store.account(&owner).await? {
                out.push((owner, self.limits_of(&account).await?));
            }
        }
        Ok(out)
    }

    /// Changes the owner's own limits. Lowering one below current use deletes nothing; it only stops growth.
    /// Server administrators and `manage_limits` only; an organization's log and the server's record it.
    pub async fn set_owner_limits(
        &self,
        actor: &Identity,
        owner: &UserId,
        change: LimitsChange,
    ) -> Result<OwnerLimits> {
        self.require_admin_scope(actor, ServiceScope::ManageLimits)
            .await?;
        let account = self.limits_account(owner).await?;
        if account.kind == AccountKind::User && change.members.is_some_and(|m| m.is_some()) {
            return Err(PynError::InvalidRequest(
                "only an organization has a member limit".into(),
            ));
        }
        let change = change.validate()?;
        let own = self.store.update_owner_limits(owner, change).await?;
        let detail = format!(
            "{owner}: repositories {}, members {}, storage bytes {}",
            show(own.repositories),
            show(own.members),
            show(own.storage_bytes)
        );
        if account.kind == AccountKind::Org {
            self.record(
                AuditScope::Org(owner.clone()),
                &actor.user,
                AuditAction::OwnerLimitsChanged,
                detail.clone(),
            )
            .await?;
        }
        self.record_server(&actor.user, AuditAction::OwnerLimitsChanged, detail)
            .await?;
        self.limits_of(&account).await
    }
}

#[async_trait]
impl StorageCaps for AccessService {
    async fn storage_cap(&self, owner: &UserId) -> Result<Option<u64>> {
        Ok(self.effective_limits(owner).await?.storage_bytes)
    }
}
