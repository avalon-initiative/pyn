//! Host-level recovery: a way back to an administrator that needs control of the host, not an existing admin.

use super::*;

/// The audit actor of a host recovery. A user name cannot contain `@`, so no account can be mistaken for it.
pub const HOST_RECOVERY_ACTOR: &str = "@host-recovery";

/// What `recover_admin` did to the account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminRecovery {
    pub user: UserId,
    pub created: bool,
    pub reenabled: bool,
    /// Tokens the account held that are now revoked.
    pub tokens_revoked: usize,
}

impl AccessService {
    /// Makes `username` an active administrator with `password`: creates the account, or enables it, completes its
    /// sign-up and replaces its password. Its sessions and tokens end; other accounts are untouched.
    pub async fn recover_admin(&self, username: &str, password: &str) -> Result<AdminRecovery> {
        let user = account::validate_username(username)?;
        account::validate_password(password)?;
        let hash = self.passwords.hash(password).await?;
        let now = self.clock.now();
        let host = UserId::new(HOST_RECOVERY_ACTOR);

        let mut created = false;
        if self.store.account(&user).await?.is_none() {
            let id = AccountId::generate(AccountKind::User)?;
            let event = self.draft(DomainEvent::AccountCreated {
                user: user.clone(),
                user_id: id.clone(),
                pending_approval: false,
            })?;
            let new = NewAccount {
                id,
                user: user.clone(),
                email: None,
                password_hash: hash.clone(),
                signup: SignupStage::Complete,
                created_at: now,
            };
            created = self.store.create_account_with_events(new, &[event]).await?;
        }
        let existing = self.existing_account(&user).await?;
        if existing.kind != AccountKind::User {
            return Err(PynError::InvalidRequest(format!(
                "{user} is an organization, not a user account"
            )));
        }
        let mut reenabled = false;
        if existing.disabled_at.is_some() {
            let event = self.draft(DomainEvent::AccountEnabled {
                user: user.clone(),
                user_id: existing.id.clone(),
                actor: host.clone(),
            })?;
            self.store
                .set_disabled_with_events(&user, None, &[event])
                .await?;
            reenabled = true;
        }
        if existing.signup != SignupStage::Complete {
            self.store.set_signup(&user, SignupStage::Complete).await?;
        }
        self.store.set_admin(&user, true).await?;
        self.store.set_password_hash(&user, &hash).await?;

        let mut tokens_revoked = 0;
        for token in self.store.list_tokens(&user).await? {
            if token.revoked_at.is_none() {
                self.store.revoke_token(&token.id, now).await?;
                tokens_revoked += 1;
            }
        }
        self.store.delete_sessions_of(&user).await?;

        let how = match (created, reenabled) {
            (true, _) => "account created",
            (false, true) => "account enabled, password reset",
            (false, false) => "password reset",
        };
        self.record_server(
            &host,
            AuditAction::AdminGranted,
            format!("{user} made an administrator by host recovery ({how})"),
        )
        .await?;
        if reenabled {
            self.record_server(
                &host,
                AuditAction::AccountEnabled,
                format!("{user} enabled by host recovery"),
            )
            .await?;
        }
        Ok(AdminRecovery {
            user,
            created,
            reenabled,
            tokens_revoked,
        })
    }

    /// A warning when exactly one active administrator exists, which is one lost credential from a locked-out server.
    pub async fn single_admin_warning(&self) -> Result<Option<String>> {
        Ok((self.store.active_admin_count().await? == 1).then(|| {
            "only one active administrator exists; add a second so losing one account does not lock \
             the server. If none is reachable, run `pyn-server recover-admin <username>` on the host"
                .to_string()
        }))
    }
}
