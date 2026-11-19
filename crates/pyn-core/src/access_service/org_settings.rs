//! An organization's repository defaults, read and changed by its owners.

use super::*;
use crate::org_settings::ReadOnlyDefault;

impl AccessService {
    /// The organization's repository defaults. Owners only.
    pub async fn org_settings(&self, actor: &Identity, org: &UserId) -> Result<OrgSettings> {
        actor.require_org_grant(org, ORG_ADMINISTRATION_READ)?;
        self.org(org).await?;
        self.require_org_owner(org, &actor.user).await?;
        self.store.org_settings(org).await
    }

    /// Replaces the organization's repository defaults. Owners only; a change is recorded in the organization log.
    pub async fn set_org_settings(
        &self,
        actor: &Identity,
        org: &UserId,
        settings: OrgSettings,
    ) -> Result<OrgSettings> {
        actor.require_org_grant(org, ORG_ADMINISTRATION_WRITE)?;
        self.org(org).await?;
        self.require_org_owner(org, &actor.user).await?;
        self.ensure_owner_writable(org).await?;
        let old = self.store.set_org_settings(org, settings).await?;
        if old.exclusive_read_only != settings.exclusive_read_only {
            let show = |v: Option<bool>| v.map_or("unset".to_string(), |b| b.to_string());
            let detail = format!(
                "exclusive_read_only: {} -> {}",
                show(old.exclusive_read_only),
                show(settings.exclusive_read_only)
            );
            self.record(
                AuditScope::Org(org.clone()),
                &actor.user,
                AuditAction::OrgSettingChanged,
                detail,
            )
            .await?;
        }
        Ok(settings)
    }

    /// The effective read-only default for a repository owned by `owner`, given its policy file's setting.
    pub async fn read_only_default(
        &self,
        owner: &UserId,
        file: Option<bool>,
    ) -> Result<ReadOnlyDefault> {
        let org = match file {
            Some(_) => None,
            None => self.store.org_settings(owner).await?.exclusive_read_only,
        };
        Ok(ReadOnlyDefault::resolve(file, org))
    }
}
