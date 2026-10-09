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

    /// The organization's members with their role. Any member may list them.
    pub async fn org_members(
        &self,
        actor: &Identity,
        org: &UserId,
    ) -> Result<Vec<(UserId, OrgRole)>> {
        self.org(org).await?;
        if self.store.org_role(org, &actor.user).await?.is_none() {
            return Err(PynError::NotOrgMember(org.to_string()));
        }
        self.store.org_members(org).await
    }

    /// Adds an existing user account to the organization. Owners only.
    pub async fn add_org_member(
        &self,
        actor: &Identity,
        org: &UserId,
        user: &UserId,
        role: OrgRole,
    ) -> Result<()> {
        actor.require_namespace_management()?;
        self.org(org).await?;
        self.require_org_owner(org, &actor.user).await?;
        match self.store.account(user).await? {
            None => return Err(PynError::UserNotFound(user.to_string())),
            Some(a) if a.kind == AccountKind::Org => {
                return Err(PynError::InvalidRequest(format!(
                    "{user} is an organization; only user accounts can be members"
                )));
            }
            Some(_) => {}
        }
        if !self.store.add_org_member(org, user, role).await? {
            return Err(PynError::AlreadyOrgMember {
                org: org.to_string(),
                user: user.to_string(),
            });
        }
        let detail = format!("{user} added as {role}");
        self.record(
            AuditScope::Org(org.clone()),
            &actor.user,
            AuditAction::OrgMemberAdded,
            detail,
        )
        .await
    }

    /// Changes a member's role. Owners only; the last owner cannot be demoted.
    pub async fn set_org_member_role(
        &self,
        actor: &Identity,
        org: &UserId,
        user: &UserId,
        role: OrgRole,
    ) -> Result<()> {
        actor.require_namespace_management()?;
        self.org(org).await?;
        self.require_org_owner(org, &actor.user).await?;
        let old = self.member_change(org, user, self.store.set_org_role(org, user, role).await?)?;
        if old != role {
            let detail = format!("{user}: {old} -> {role}");
            self.record(
                AuditScope::Org(org.clone()),
                &actor.user,
                AuditAction::OrgMemberRoleChanged,
                detail,
            )
            .await?;
        }
        Ok(())
    }

    /// Removes a member and their direct grants on the organization's repositories. A member may remove
    /// themselves; otherwise owners only. The last owner cannot leave.
    pub async fn remove_org_member(
        &self,
        actor: &Identity,
        org: &UserId,
        user: &UserId,
    ) -> Result<()> {
        actor.require_namespace_management()?;
        self.org(org).await?;
        if actor.user != *user {
            self.require_org_owner(org, &actor.user).await?;
        }
        let mut granted = Vec::new();
        if let Some(registry) = &self.registry {
            for repo in registry.list_repos(Some(org)).await? {
                if self.store.role_of(&repo.id, user).await?.is_some() {
                    granted.push(repo);
                }
            }
        }
        let ids: Vec<_> = granted.iter().map(|r| r.id.clone()).collect();
        let change = self.store.remove_org_member(org, user, &ids).await?;
        self.member_change(org, user, change)?;
        let mut detail = if actor.user == *user {
            format!("{user} left")
        } else {
            format!("{user} removed")
        };
        if !granted.is_empty() {
            let repos = join(granted.iter().map(|r| r.address()));
            detail.push_str(&format!("; direct access dropped in {repos}"));
        }
        self.record(
            AuditScope::Org(org.clone()),
            &actor.user,
            AuditAction::OrgMemberRemoved,
            detail,
        )
        .await
    }

    fn member_change(
        &self,
        org: &UserId,
        user: &UserId,
        change: OrgMemberChange,
    ) -> Result<OrgRole> {
        match change {
            OrgMemberChange::Done(old) => Ok(old),
            OrgMemberChange::NotMember => Err(PynError::OrgMemberNotFound {
                org: org.to_string(),
                user: user.to_string(),
            }),
            OrgMemberChange::LastOwner => Err(PynError::LastOrgOwner(org.to_string())),
        }
    }

    /// A direct grant on an organization's repository goes only to a member of that organization.
    pub(super) async fn require_grantable(&self, repo: &RepoId, user: &UserId) -> Result<()> {
        let Some(registry) = &self.registry else {
            return Ok(());
        };
        let Some(record) = registry.get_repo(repo).await? else {
            return Ok(());
        };
        if self.is_org(&record.owner).await?
            && self.store.org_role(&record.owner, user).await?.is_none()
        {
            return Err(PynError::UserNotOrgMember {
                org: record.owner.to_string(),
                user: user.to_string(),
            });
        }
        Ok(())
    }

    pub(super) async fn is_org_repo(&self, repo: &RepoId) -> Result<bool> {
        let Some(registry) = &self.registry else {
            return Ok(false);
        };
        match registry.get_repo(repo).await? {
            Some(record) => self.is_org(&record.owner).await,
            None => Ok(false),
        }
    }
}
