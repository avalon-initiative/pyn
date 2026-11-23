//! Server invitations and administrator-made accounts: how a person gets onto an invite-only server.

use super::*;
use crate::email::invitation_message;

/// What an administrator asks for when inviting a person to the server.
#[derive(Debug, Clone, Default)]
pub struct NewServerInvitation {
    /// Where to send the code when a mailer is configured.
    pub email: Option<String>,
    /// Days the code works; the default when none.
    pub days: Option<i64>,
    pub grant: Option<InvitationGrant>,
}

/// A created invitation with its code, which is shown once.
#[derive(Debug, Clone)]
pub struct CreatedServerInvitation {
    pub invitation: ServerInvitation,
    pub code: String,
    /// The message was handed to a configured mailer.
    pub emailed: bool,
}

impl AccessService {
    pub(super) async fn check_server_invitation(&self, code: &str) -> Result<ServerInvitation> {
        let bad = |why: &str| PynError::InvalidInvite(why.to_string());
        let (id, secret) =
            server_invite::parse(code).ok_or_else(|| bad("the code is not valid"))?;
        let record = self
            .store
            .get_server_invitation(&id)
            .await?
            .ok_or_else(|| bad("the code is not valid"))?;
        if !token::hashes_match(&record.secret_hash, &token::hash_secret(secret)) {
            return Err(bad("the code is not valid"));
        }
        if record.revoked_at.is_some() {
            return Err(bad("it has been revoked"));
        }
        if record.used_at.is_some() {
            return Err(bad("it has already been used"));
        }
        if record.expires_at <= self.clock.now() {
            return Err(bad("it has expired"));
        }
        match &record.grant {
            Some(InvitationGrant::Repository { repo, .. }) => {
                self.ensure_repo_writable(repo).await?
            }
            Some(InvitationGrant::Organization { org, .. }) => {
                self.ensure_owner_writable(org).await?
            }
            None => {}
        }
        Ok(record)
    }

    /// Records the use and applies the invitation's role to the account it just created.
    pub(super) async fn accept_server_invitation(
        &self,
        invitation: &ServerInvitation,
        user: &UserId,
    ) -> Result<()> {
        self.record_server(
            user.into(),
            AuditAction::ServerInvitationUsed,
            format!("{user} registered with invitation {}", invitation.id),
        )
        .await?;
        match &invitation.grant {
            Some(InvitationGrant::Repository { repo, role }) => {
                self.store.set_role(repo, user, *role).await?;
                self.record(
                    repo,
                    user.into(),
                    AuditAction::MemberAdded,
                    format!("{user} registered as {role}"),
                )
                .await
            }
            Some(InvitationGrant::Organization { org, role }) => {
                let max_members = self.effective_limits(org).await?.members;
                let result = self
                    .store
                    .add_org_member_capped(org, user, *role, max_members)
                    .await;
                self.limit_result(org, result, self.clock.now()).await?;
                self.record(
                    AuditScope::Org(org.clone()),
                    user.into(),
                    AuditAction::OrgMemberAdded,
                    format!("{user} joined as {role} by server invitation"),
                )
                .await
            }
            None => Ok(()),
        }
    }

    /// Creates a one-time invitation to the server. The code is returned once and not stored. Server administrators only.
    pub async fn create_server_invitation(
        &self,
        actor: &Identity,
        request: NewServerInvitation,
    ) -> Result<CreatedServerInvitation> {
        actor.require_full_scope()?;
        self.require_server_admin(actor).await?;
        let days = request.days.unwrap_or(DEFAULT_SERVER_INVITE_DAYS);
        if !(1..=MAX_SERVER_INVITE_DAYS).contains(&days) {
            return Err(PynError::InvalidRequest(format!(
                "an invitation is valid for 1 to {MAX_SERVER_INVITE_DAYS} days"
            )));
        }
        let email = request
            .email
            .as_deref()
            .map(account::normalize_email)
            .transpose()?;
        match &request.grant {
            Some(InvitationGrant::Repository { repo, .. }) => {
                self.ensure_repo_writable(repo).await?;
                if let Some(registry) = &self.registry
                    && registry.get_repo(repo).await?.is_none()
                {
                    return Err(PynError::RepoNotFound(repo.to_string()));
                }
                if self.is_org_repo(repo).await? {
                    return Err(PynError::InvalidRequest(
                        "an organization's repository cannot be granted by invitation; invite into the organization instead".into(),
                    ));
                }
            }
            Some(InvitationGrant::Organization { org, .. }) => {
                self.org(org).await?;
                self.ensure_owner_writable(org).await?;
            }
            None => {}
        }
        let sends = email.is_some() && self.email.kind() != MailerKind::None;
        if let (true, Some(email)) = (sends, &email) {
            self.count_limited(
                &format!("mail:{}", bounded_key(email)),
                self.config.limits.mail_per_email,
                MAIL_WINDOW,
            )
            .await?;
        }
        let (id, code, secret_hash) = server_invite::generate()?;
        let now = self.clock.now();
        let invitation = ServerInvitation {
            id,
            secret_hash,
            email: email.clone(),
            grant: request.grant,
            created_by: actor.user.clone(),
            created_at: now,
            expires_at: now + Duration::days(days),
            used_at: None,
            used_by: None,
            revoked_at: None,
        };
        self.store
            .create_server_invitation(invitation.clone())
            .await?;
        let detail = match &invitation.grant {
            None => format!(
                "invitation {}, expires {}",
                invitation.id,
                invitation.expires_at.to_rfc3339()
            ),
            Some(InvitationGrant::Repository { repo, role }) => format!(
                "invitation {} with role {role} in repository {repo}, expires {}",
                invitation.id,
                invitation.expires_at.to_rfc3339()
            ),
            Some(InvitationGrant::Organization { org, role }) => format!(
                "invitation {} with role {role} in organization {org}, expires {}",
                invitation.id,
                invitation.expires_at.to_rfc3339()
            ),
        };
        self.record_server(actor.into(), AuditAction::ServerInvitationCreated, detail)
            .await?;
        let emailed = match (sends, &email) {
            (true, Some(email)) => {
                self.deliver(invitation_message(email, &code, &self.public_url(), days))
                    .await
            }
            _ => false,
        };
        Ok(CreatedServerInvitation {
            invitation,
            code,
            emailed,
        })
    }

    /// Every server invitation, newest first, never with a code. Server administrators only.
    pub async fn list_server_invitations(&self, actor: &Identity) -> Result<Vec<ServerInvitation>> {
        actor.require_full_scope()?;
        self.require_server_admin(actor).await?;
        self.store.list_server_invitations().await
    }

    /// Revokes an unused invitation. Server administrators only.
    pub async fn revoke_server_invitation(&self, actor: &Identity, id: &InviteId) -> Result<()> {
        actor.require_full_scope()?;
        self.require_server_admin(actor).await?;
        let not_found = || PynError::ServerInvitationNotFound(id.to_string());
        let invitation = self
            .store
            .get_server_invitation(id)
            .await?
            .ok_or_else(not_found)?;
        if invitation.used_at.is_some() {
            return Err(PynError::InvalidRequest(
                "that invitation has already been used".into(),
            ));
        }
        if !self
            .store
            .revoke_server_invitation(id, self.clock.now())
            .await?
        {
            return Err(not_found());
        }
        self.record_server(
            actor.into(),
            AuditAction::ServerInvitationRevoked,
            format!("invitation {id}"),
        )
        .await
    }

    /// Creates an active account with a one-time password the person must replace at first sign-in.
    /// Server administrators only.
    pub async fn admin_add_account(
        &self,
        actor: &Identity,
        username: &str,
        password: &str,
    ) -> Result<AccountRecord> {
        actor.require_full_scope()?;
        self.require_server_admin(actor).await?;
        let user = account::validate_username(username)?;
        account::validate_password(password)?;
        self.ensure_name_free(&user).await?;
        let password_hash = self.passwords.hash(password).await?;
        let now = self.clock.now();
        let new = NewAccount {
            id: AccountId::generate(AccountKind::User)?,
            user: user.clone(),
            email: None,
            password_hash,
            signup: SignupStage::Complete,
            created_at: now,
        };
        let created = self.draft(DomainEvent::AccountCreated {
            user: user.clone(),
            user_id: new.id.clone(),
            pending_approval: false,
        })?;
        if !self
            .store
            .create_account_with_events(new, &[created])
            .await?
        {
            return Err(PynError::UserExists(user));
        }
        self.store.set_password_change_required(&user, true).await?;
        self.record_server(
            actor.into(),
            AuditAction::AccountAddedByAdmin,
            format!("{user} added with a one-time password"),
        )
        .await?;
        self.existing_account(&user).await
    }

    /// Sends a fresh confirmation link to an account still waiting on its address. Server administrators only.
    pub async fn admin_resend_confirmation(&self, actor: &Identity, user: &UserId) -> Result<()> {
        actor.require_full_scope()?;
        self.require_server_admin(actor).await?;
        let account = self.existing_account(user).await?;
        if !self.config.require_email_verification {
            return Err(PynError::EmailVerificationOff);
        }
        if account.signup != SignupStage::PendingVerification {
            return Err(PynError::EmailAlreadyConfirmed);
        }
        let email = account
            .email
            .ok_or_else(|| PynError::InvalidRequest("that account has no email address".into()))?;
        self.count_limited(
            &format!("mail:{}", bounded_key(&email)),
            self.config.limits.mail_per_email,
            MAIL_WINDOW,
        )
        .await?;
        self.send_verification(user, &email).await?;
        self.record_server(
            actor.into(),
            AuditAction::AccountConfirmationResent,
            format!("confirmation resent to {user}"),
        )
        .await
    }

    /// Whether the account's password is a one-time one that must be replaced.
    pub async fn password_change_required(&self, user: &UserId) -> Result<bool> {
        self.store.password_change_required(user).await
    }
}
