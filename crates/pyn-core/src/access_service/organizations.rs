//! Organizations: namespaces that own repositories and are run by their owners.

use super::*;

impl AccessService {
    /// Whether `name` is an organization.
    pub async fn is_org(&self, name: &UserId) -> Result<bool> {
        Ok(self
            .store
            .account(name)
            .await?
            .is_some_and(|a| a.kind == AccountKind::Org))
    }

    /// The organization `name`; `OrgNotFound` if there is none, or the name belongs to a user.
    pub async fn org(&self, name: &UserId) -> Result<AccountRecord> {
        self.store
            .account(name)
            .await?
            .filter(|a| a.kind == AccountKind::Org)
            .ok_or_else(|| PynError::OrgNotFound(name.to_string()))
    }

    pub async fn org_role(&self, org: &UserId, user: &UserId) -> Result<Option<OrgRole>> {
        self.store.org_role(org, user).await
    }

    /// The organizations the user belongs to with their role, ordered by name.
    pub async fn orgs_of(&self, user: &UserId) -> Result<Vec<(AccountRecord, OrgRole)>> {
        let mut out = Vec::new();
        for (name, role) in self.store.orgs_of(user).await? {
            out.push((self.org(&name).await?, role));
        }
        Ok(out)
    }

    /// `NotOrgOwner` unless `user` owns the organization.
    pub async fn require_org_owner(&self, org: &UserId, user: &UserId) -> Result<()> {
        match self.store.org_role(org, user).await? {
            Some(OrgRole::Owner) => Ok(()),
            _ => Err(PynError::NotOrgOwner(org.to_string())),
        }
    }

    /// Creates an organization with the actor as its first owner. The server may limit this to administrators.
    pub async fn create_org(&self, actor: &Identity, name: &str) -> Result<AccountRecord> {
        actor.require_namespace_management()?;
        if self.config.org_creation == OrgCreation::AdminsOnly {
            self.require_server_admin(actor).await?;
        }
        let org = account::validate_org_name(name)?;
        let now = self.clock.now();
        self.store.ensure_user(&actor.user, now).await?;
        if !self.store.create_org(&org, &actor.user, now).await? {
            return Err(PynError::UserExists(org));
        }
        let detail = format!("{org} created, owner {}", actor.user);
        self.record(
            AuditScope::Org(org.clone()),
            &actor.user,
            AuditAction::OrgCreated,
            detail,
        )
        .await?;
        self.org(&org).await
    }

    /// Deletes an organization that owns no repositories. Owners only; its audit log stays.
    pub async fn delete_org(&self, actor: &Identity, name: &UserId) -> Result<()> {
        actor.require_namespace_management()?;
        self.org(name).await?;
        self.require_org_owner(name, &actor.user).await?;
        if let Some(registry) = &self.registry
            && !registry.list_repos(Some(name)).await?.is_empty()
        {
            return Err(PynError::OrgNotEmpty(name.to_string()));
        }
        let detail = format!("{name} deleted");
        self.record(
            AuditScope::Org(name.clone()),
            &actor.user,
            AuditAction::OrgDeleted,
            detail,
        )
        .await?;
        self.store.delete_org(name).await?;
        Ok(())
    }

    /// The organization's audit log, newest first. Owners only.
    pub async fn org_audit(
        &self,
        actor: &Identity,
        name: &UserId,
        query: &AuditQuery,
    ) -> Result<Vec<AuditEvent>> {
        self.org(name).await?;
        self.require_org_owner(name, &actor.user).await?;
        match &self.audit {
            Some(store) => store.list(&AuditScope::Org(name.clone()), query).await,
            None => Ok(Vec::new()),
        }
    }
}
