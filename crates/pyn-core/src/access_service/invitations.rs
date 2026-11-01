//! Organization invitations: membership is offered to a user, who accepts or declines.

use super::*;

/// What adding a member did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrgAdd {
    Added,
    Invited(OrgInvitation),
}

impl AccessService {
    /// Adds the user to the organization, or invites them. Owners only. A server administrator skips the
    /// invitation when the server's `org_member_add` setting is `direct`.
    pub async fn add_org_member(
        &self,
        actor: &Identity,
        org: &UserId,
        user: &UserId,
        role: OrgRole,
    ) -> Result<OrgAdd> {
        actor.require_namespace_management()?;
        self.org(org).await?;
        self.require_org_owner(org, &actor.user).await?;
        self.ensure_owner_writable(org).await?;
        match self.store.account(user).await? {
            None => return Err(PynError::UserNotFound(user.to_string())),
            Some(a) if a.kind == AccountKind::Org => {
                return Err(PynError::InvalidRequest(format!(
                    "{user} is an organization; only user accounts can be members"
                )));
            }
            Some(_) => {}
        }
        self.expire_org_invitations(Some(org), None).await?;
        if self.config.org_member_add == OrgMemberAdd::Direct && self.is_server_admin(actor).await?
        {
            self.add_member_directly(actor, org, user, role).await?;
            return Ok(OrgAdd::Added);
        }
        let now = self.clock.now();
        let invitation = OrgInvitation {
            org: org.clone(),
            user: user.clone(),
            role,
            invited_by: actor.user.clone(),
            created_at: now,
            expires_at: now + self.config.org_invitation_ttl,
        };
        match self.store.create_org_invitation(&invitation).await? {
            InvitationCreate::Created => {}
            InvitationCreate::AlreadyMember => {
                return Err(PynError::AlreadyOrgMember {
                    org: org.to_string(),
                    user: user.to_string(),
                });
            }
            InvitationCreate::AlreadyInvited => {
                return Err(PynError::AlreadyInvited {
                    org: org.to_string(),
                    user: user.to_string(),
                });
            }
        }
        let detail = format!(
            "{user} invited as {role}, expires {}",
            invitation.expires_at.format("%Y-%m-%d")
        );
        self.record(
            AuditScope::Org(org.clone()),
            &actor.user,
            AuditAction::OrgMemberInvited,
            detail,
        )
        .await?;
        Ok(OrgAdd::Invited(invitation))
    }

    async fn add_member_directly(
        &self,
        actor: &Identity,
        org: &UserId,
        user: &UserId,
        role: OrgRole,
    ) -> Result<()> {
        let max_members = self.effective_limits(org).await?.members;
        let result = self
            .store
            .add_org_member_capped(org, user, role, max_members)
            .await;
        if !self.limit_result(org, result, self.clock.now()).await? {
            return Err(PynError::AlreadyOrgMember {
                org: org.to_string(),
                user: user.to_string(),
            });
        }
        self.store.delete_org_invitation(org, user).await?;
        let detail = format!("{user} added as {role}");
        self.record(
            AuditScope::Org(org.clone()),
            &actor.user,
            AuditAction::OrgMemberAdded,
            detail,
        )
        .await
    }

    /// The organization's pending invitations, ordered by user name. Owners only.
    pub async fn org_invitations(
        &self,
        actor: &Identity,
        org: &UserId,
    ) -> Result<Vec<OrgInvitation>> {
        actor.require_full_scope()?;
        self.org(org).await?;
        self.require_org_owner(org, &actor.user).await?;
        self.expire_org_invitations(Some(org), None).await?;
        self.store.org_invitations(org).await
    }

    /// The actor's own pending invitations, ordered by organization name. Full-scope credentials only.
    pub async fn my_org_invitations(&self, actor: &Identity) -> Result<Vec<OrgInvitation>> {
        actor.require_full_scope()?;
        self.expire_org_invitations(None, Some(&actor.user)).await?;
        self.store.org_invitations_for(&actor.user).await
    }

    /// Withdraws a pending invitation. Owners only.
    pub async fn cancel_org_invitation(
        &self,
        actor: &Identity,
        org: &UserId,
        user: &UserId,
    ) -> Result<()> {
        actor.require_namespace_management()?;
        self.org(org).await?;
        self.require_org_owner(org, &actor.user).await?;
        self.ensure_owner_writable(org).await?;
        self.expire_org_invitations(Some(org), Some(user)).await?;
        if self.store.delete_org_invitation(org, user).await?.is_none() {
            return Err(not_invited(org, user));
        }
        self.record(
            AuditScope::Org(org.clone()),
            &actor.user,
            AuditAction::OrgInvitationCancelled,
            format!("invitation of {user} cancelled"),
        )
        .await
    }

    /// Accepts the actor's invitation to `org`, making them a member with the offered role.
    pub async fn accept_org_invitation(
        &self,
        actor: &Identity,
        org: &UserId,
    ) -> Result<(AccountRecord, OrgRole)> {
        actor.require_namespace_management()?;
        let record = self.org(org).await?;
        self.ensure_owner_writable(org).await?;
        self.expire_org_invitations(Some(org), Some(&actor.user))
            .await?;
        let max_members = self.effective_limits(org).await?.members;
        let now = self.clock.now();
        let result = self
            .store
            .accept_org_invitation(org, &actor.user, now, max_members)
            .await;
        let role = match self.limit_result(org, result, now).await? {
            InvitationAccept::Joined(role) => role,
            InvitationAccept::Missing => return Err(not_invited(org, &actor.user)),
        };
        self.record(
            AuditScope::Org(org.clone()),
            &actor.user,
            AuditAction::OrgInvitationAccepted,
            format!("{} joined as {role}", actor.user),
        )
        .await?;
        Ok((record, role))
    }

    /// Declines the actor's invitation to `org`.
    pub async fn decline_org_invitation(&self, actor: &Identity, org: &UserId) -> Result<()> {
        actor.require_namespace_management()?;
        self.org(org).await?;
        self.expire_org_invitations(Some(org), Some(&actor.user))
            .await?;
        if self
            .store
            .delete_org_invitation(org, &actor.user)
            .await?
            .is_none()
        {
            return Err(not_invited(org, &actor.user));
        }
        self.record(
            AuditScope::Org(org.clone()),
            &actor.user,
            AuditAction::OrgInvitationDeclined,
            format!("{} declined", actor.user),
        )
        .await
    }

    /// Drops invitations past their expiry and logs each; the organization is the actor of the event.
    async fn expire_org_invitations(
        &self,
        org: Option<&UserId>,
        user: Option<&UserId>,
    ) -> Result<()> {
        let now = self.clock.now();
        for i in self
            .store
            .take_expired_org_invitations(org, user, now)
            .await?
        {
            let detail = format!(
                "invitation of {} as {} by {} expired",
                i.user, i.role, i.invited_by
            );
            self.record(
                AuditScope::Org(i.org.clone()),
                &i.org,
                AuditAction::OrgInvitationExpired,
                detail,
            )
            .await?;
        }
        Ok(())
    }
}

fn not_invited(org: &UserId, user: &UserId) -> PynError {
    PynError::OrgInvitationNotFound {
        org: org.to_string(),
        user: user.to_string(),
    }
}
