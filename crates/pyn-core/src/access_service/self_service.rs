//! Account self-service (#982): the signed-in user's own email addresses and username.

use super::*;

/// Most addresses one account may hold, verified or not.
pub const MAX_ACCOUNT_EMAILS: usize = 10;

/// What a rename changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountRename {
    pub old: UserId,
    pub new: UserId,
    pub account: AccountId,
    /// The repositories now under the new name, whose old addresses redirect.
    pub repositories: Vec<String>,
}

impl AccessService {
    /// The caller's own addresses, the primary first.
    pub async fn list_emails(&self, actor: &Identity) -> Result<Vec<AccountEmail>> {
        actor.require_full_scope()?;
        self.existing_account(&actor.user).await?;
        self.store.account_emails(&actor.user).await
    }

    /// Adds an unverified address and mails its link. Another account's address, verified or pending, is refused
    /// with the one generic error so the answer never says whose it is.
    pub async fn add_email(&self, actor: &Identity, address: &str) -> Result<()> {
        actor.require_full_scope()?;
        self.existing_account(&actor.user).await?;
        let address = account::normalize_email(address)?;
        self.count_limited(
            &format!("email:add:{}", bounded_key(actor.user.as_str())),
            self.config.limits.register_per_client,
            REGISTER_WINDOW,
        )
        .await?;
        if self.store.account_emails(&actor.user).await?.len() >= MAX_ACCOUNT_EMAILS {
            return Err(PynError::EmailRefused(format!(
                "an account holds at most {MAX_ACCOUNT_EMAILS} addresses"
            )));
        }
        match self
            .store
            .add_email(&actor.user, &address, self.clock.now())
            .await?
        {
            EmailAdd::Taken => return Err(PynError::EmailUnavailable),
            EmailAdd::Own => {
                return Err(PynError::EmailRefused(
                    "that address is already on your account".into(),
                ));
            }
            EmailAdd::Added => {}
        }
        self.record_server(
            &actor.user,
            AuditAction::EmailAdded,
            format!("{} added {address}", actor.user),
        )
        .await?;
        self.mail_address_check(&actor.user, &address, true).await
    }

    /// Sends a fresh link for an address that is not verified yet.
    pub async fn resend_email(&self, actor: &Identity, address: &str) -> Result<()> {
        actor.require_full_scope()?;
        let address = account::normalize_email(address)?;
        let held = self.store.account_emails(&actor.user).await?;
        if !held
            .iter()
            .any(|e| e.address == address && e.verified_at.is_none())
        {
            return Err(PynError::EmailNotFound);
        }
        self.mail_address_check(&actor.user, &address, false).await
    }

    /// Makes a verified address the primary.
    pub async fn set_primary_email(&self, actor: &Identity, address: &str) -> Result<()> {
        actor.require_full_scope()?;
        let address = account::normalize_email(address)?;
        match self.store.set_primary_email(&actor.user, &address).await? {
            PrimaryChange::Changed => {}
            PrimaryChange::NotFound => return Err(PynError::EmailNotFound),
            PrimaryChange::Unverified => {
                return Err(PynError::EmailRefused(
                    "the primary address must be verified".into(),
                ));
            }
        }
        self.record_server(
            &actor.user,
            AuditAction::EmailPrimaryChanged,
            format!("{} made {address} the primary address", actor.user),
        )
        .await
    }

    /// Removes an address. Where sign-up needs a verified address, the last verified one stays.
    pub async fn remove_email(&self, actor: &Identity, address: &str) -> Result<()> {
        actor.require_full_scope()?;
        let address = account::normalize_email(address)?;
        let keep = self.registration_mode() == RegistrationMode::Open
            && self.config.require_email_verification;
        match self.store.remove_email(&actor.user, &address, keep).await? {
            EmailRemove::Removed => {}
            EmailRemove::NotFound => return Err(PynError::EmailNotFound),
            EmailRemove::LastVerified => {
                return Err(PynError::EmailRefused(
                    "this is your only verified address, and this server requires one".into(),
                ));
            }
        }
        self.record_server(
            &actor.user,
            AuditAction::EmailRemoved,
            format!("{} removed {address}", actor.user),
        )
        .await
    }

    /// Redeems the link for an address added to a signed-up account.
    pub(super) async fn confirm_added_email(
        &self,
        account: &AccountRecord,
        record: &VerificationRecord,
    ) -> Result<SignUp> {
        let bad = || {
            PynError::InvalidVerification("the link is not valid or has already been used".into())
        };
        if !self
            .store
            .confirm_added_email(&account.user, &record.email, self.clock.now())
            .await?
        {
            return Err(bad());
        }
        self.record_server(
            &account.user,
            AuditAction::EmailVerified,
            format!("{} verified {}", account.user, record.email),
        )
        .await?;
        Ok(SignUp {
            user: account.user.clone(),
            status: account.status(),
        })
    }

    /// Mails the link; past the per-recipient limit a first mail is skipped quietly and a resend is refused.
    async fn mail_address_check(&self, user: &UserId, address: &str, quiet: bool) -> Result<()> {
        let allowed = self
            .count_limited(
                &format!("mail:{}", bounded_key(address)),
                self.config.limits.mail_per_email,
                MAIL_WINDOW,
            )
            .await;
        match allowed {
            Ok(()) => {}
            Err(_) if quiet => return Ok(()),
            Err(e) => return Err(e),
        }
        let (raw, token_hash) = verification::generate()?;
        self.store
            .put_verification(VerificationRecord {
                token_hash,
                user: user.clone(),
                email: address.to_string(),
                expires_at: self.clock.now() + self.config.verification_ttl,
            })
            .await?;
        let link = format!(
            "{}/verify-email?token={raw}",
            self.public_url().trim_end_matches('/')
        );
        self.email
            .send(EmailMessage {
                to: address.to_string(),
                subject: "Confirm your email address".into(),
                body: format!(
                    "Confirm this address for the account {user} by opening this link:\n\n{link}\n\n\
                     It works once and expires in {} hours. If you did not ask for it, ignore this message.",
                    self.config.verification_ttl.num_hours()
                ),
            })
            .await
    }

    /// Whether `name` can be given to an account. Every way a name becomes taken is checked here.
    pub(crate) async fn name_is_free(&self, name: &UserId) -> Result<bool> {
        Ok(self.store.account(name).await?.is_none())
    }

    /// Renames the caller's account, re-entering the password. Its repositories move with it and their old
    /// addresses redirect; the old name is not reserved. Refused while the account is frozen.
    pub async fn rename_account(
        &self,
        actor: &Identity,
        new_name: &str,
        password: Option<&str>,
    ) -> Result<AccountRename> {
        actor.require_full_scope()?;
        let account = self.existing_account(&actor.user).await?;
        let new = account::validate_username(new_name)?;
        if new == account.user {
            return Err(PynError::InvalidRequest("that is already your name".into()));
        }
        self.ensure_owner_writable(&account.user).await?;
        self.reauthenticate(actor, password, "a rename").await?;
        if !self.name_is_free(&new).await? {
            return Err(PynError::UserExists(new));
        }
        let at = self.clock.now();
        let event = self.draft(DomainEvent::AccountRenamed {
            old_name: account.user.clone(),
            new_name: new.clone(),
            user_id: account.id.clone(),
            actor: account.user.clone(),
        })?;
        match self
            .store
            .rename_account(&account.user, &new, at, &[event])
            .await?
        {
            RenameOutcome::Renamed { .. } => {}
            RenameOutcome::NameTaken => return Err(PynError::UserExists(new)),
            RenameOutcome::NoAccount => {
                return Err(PynError::UserNotFound(account.user.to_string()));
            }
        }
        let moved = match &self.registry {
            Some(registry) => match registry.rename_owner(&account.user, &new, at).await {
                Ok(moved) => moved,
                Err(e) => {
                    let _ = self
                        .store
                        .rename_account(&new, &account.user, at, &[])
                        .await;
                    return Err(e);
                }
            },
            None => Vec::new(),
        };
        self.record_server(
            &account.user,
            AuditAction::AccountRenamed,
            format!("{} renamed to {new}", account.user),
        )
        .await?;
        Ok(AccountRename {
            old: account.user,
            new,
            account: account.id,
            repositories: moved.into_iter().map(|r| r.name).collect(),
        })
    }

    /// For each author name and time, the current name of the account that held it then, when that changed.
    pub async fn current_names(
        &self,
        authors: &[(UserId, DateTime<Utc>)],
    ) -> Result<Vec<Option<UserId>>> {
        self.store.current_names(authors).await
    }

    /// Checks the account's current password, rate limited like sign-in. An account with none can
    /// confirm only from a signed-in web session.
    pub(super) async fn reauthenticate(
        &self,
        actor: &Identity,
        current: Option<&str>,
        what: &str,
    ) -> Result<()> {
        if let Some(hash) = self.store.password_hash(&actor.user).await? {
            let key = format!(
                "password:account:{}",
                bounded_key(&actor.user.as_str().to_lowercase())
            );
            self.count_limited(&key, self.config.limits.sign_in_per_account, SIGN_IN_WINDOW)
                .await?;
            let ok = match current {
                Some(current) => self.passwords.verify(current, &hash).await,
                None => false,
            };
            if !ok {
                return Err(PynError::Unauthenticated(
                    "the current password is wrong".into(),
                ));
            }
            self.limits.reset(&key).await
        } else if !matches!(actor.credential, Credential::Session) {
            Err(PynError::Unauthenticated(format!(
                "{what} needs a password or a signed-in web session"
            )))
        } else {
            Ok(())
        }
    }
}
