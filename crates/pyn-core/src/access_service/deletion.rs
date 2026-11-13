//! Account deletion (decisions #975, #976): self-service with a recovery window, administrator deletion of a
//! disabled account, automatic deletion of long-disabled accounts, and the name reservation after a purge.

use super::*;

/// The audit actor of a deletion the server's own timers carry out. A user name cannot contain `@`.
pub const ACCOUNT_DELETION_ACTOR: &str = "@account-deletion";

const REASON_BYTES: usize = 500;
const SWEEP_BATCH: usize = 500;

fn clean_reason(reason: &str) -> Result<String> {
    let reason = reason.trim();
    if reason.is_empty() || reason.len() > REASON_BYTES || reason.chars().any(char::is_control) {
        return Err(PynError::InvalidRequest(format!(
            "deleting an account needs a reason of 1 to {REASON_BYTES} bytes"
        )));
    }
    Ok(reason.to_string())
}

impl AccessService {
    /// The schedule an owner made for the account, which is what lets it sign in to restore.
    pub(super) async fn owner_deletion(
        &self,
        account: &AccountRecord,
    ) -> Result<Option<AccountDeletion>> {
        if account.disabled_at.is_none() {
            return Ok(None);
        }
        Ok(self
            .store
            .account_deletion(&account.user)
            .await?
            .filter(|d| d.by_owner))
    }

    /// Whether `name` appears as an author in any history or as an actor in any audit log.
    async fn name_in_records(&self, name: &UserId) -> Result<bool> {
        if let Some(registry) = &self.registry
            && registry.author_appears(name).await?
        {
            return Ok(true);
        }
        match &self.audit {
            Some(audit) => audit.actor_appears(name).await,
            None => Ok(false),
        }
    }

    /// `NameReserved` while a purged account's name is held: for 90 days, and for as long as it appears in history
    /// or the audit log. An expired reservation of a name found nowhere is released here.
    pub async fn ensure_name_free(&self, name: &UserId) -> Result<()> {
        let Some(reservation) = self.store.name_reservation(name.as_str()).await? else {
            return Ok(());
        };
        if reservation.holds_at(self.clock.now()) {
            return Err(PynError::NameReserved(name.to_string()));
        }
        if self.name_in_records(name).await? {
            self.store
                .make_name_reservation_permanent(name.as_str())
                .await?;
            return Err(PynError::NameReserved(name.to_string()));
        }
        self.store.release_name_reservation(name.as_str()).await?;
        Ok(())
    }

    /// Repositories the account owns and organizations it is the only owner of.
    pub async fn deletion_blockers(&self, user: &UserId) -> Result<PurgeBlockers> {
        let mut blockers = PurgeBlockers::default();
        if let Some(registry) = &self.registry {
            blockers.repos = registry
                .list_repos(Some(user))
                .await?
                .into_iter()
                .map(|r| format!("{}/{}", r.owner, r.name))
                .collect();
        }
        for (org, role) in self.store.orgs_of(user).await? {
            if role != OrgRole::Owner {
                continue;
            }
            let owners = self
                .store
                .org_members(&org)
                .await?
                .into_iter()
                .filter(|(_, r)| *r == OrgRole::Owner)
                .count();
            if owners <= 1 {
                blockers.orgs.push(org.to_string());
            }
        }
        Ok(blockers)
    }

    async fn require_deletable(&self, account: &AccountRecord) -> Result<()> {
        if account.is_frozen() {
            return Err(PynError::AccountFrozen(account.user.to_string()));
        }
        let blockers = self.deletion_blockers(&account.user).await?;
        if !blockers.is_empty() {
            return Err(PynError::AccountOwnsResources {
                user: account.user.to_string(),
                repos: blockers.repos,
                orgs: blockers.orgs,
            });
        }
        Ok(())
    }

    /// The deletion scheduled for the signed-in account, if any.
    pub async fn deletion_status(&self, actor: &Identity) -> Result<Option<AccountDeletion>> {
        let account = self.existing_account(&actor.user).await?;
        Ok(match account.disabled_at {
            Some(_) => self.store.account_deletion(&actor.user).await?,
            None => None,
        })
    }

    /// Disables the signed-in account and schedules its deletion after the recovery window. The password is
    /// checked again; an account with none (external sign-in) needs a web session.
    pub async fn schedule_own_deletion(
        &self,
        actor: &Identity,
        password: &str,
    ) -> Result<AccountDeletion> {
        actor.require_full_scope()?;
        let user = &actor.user;
        let account = self.existing_account(user).await?;
        if account.status() != AccountStatus::Active {
            return Err(PynError::InvalidRequest(
                "this account is already scheduled for deletion".into(),
            ));
        }
        match self.store.password_hash(user).await? {
            Some(hash) => {
                let key = format!(
                    "password:account:{}",
                    bounded_key(&user.as_str().to_lowercase())
                );
                self.count_limited(&key, self.config.limits.sign_in_per_account, SIGN_IN_WINDOW)
                    .await?;
                if !self.passwords.verify(password, &hash).await {
                    return Err(PynError::Unauthenticated("the password is wrong".into()));
                }
                self.limits.reset(&key).await?;
            }
            None if matches!(actor.credential, Credential::Session) => {}
            None => {
                return Err(PynError::Unauthenticated(
                    "an account with no password can be deleted only from a signed-in web session"
                        .into(),
                ));
            }
        }
        self.require_deletable(&account).await?;

        let now = self.clock.now();
        let deletion = AccountDeletion {
            user: user.clone(),
            requested_by: user.clone(),
            by_owner: true,
            reason: None,
            scheduled_at: now,
            purge_at: now + Duration::days(self.config.deletion_window_days),
        };
        let event = self.draft(DomainEvent::AccountDeletionScheduled {
            user: user.clone(),
            user_id: account.id,
            actor: user.clone(),
            by_owner: true,
            reason: None,
            purge_at: deletion.purge_at,
        })?;
        match self
            .store
            .schedule_account_deletion_with_events(deletion.clone(), true, &[event])
            .await?
        {
            DeletionSchedule::Scheduled => {}
            DeletionSchedule::NoAccount => return Err(PynError::UserNotFound(user.to_string())),
            DeletionSchedule::LastAdmin => return Err(PynError::LastServerAdmin),
        }
        let detail = format!(
            "{user} scheduled their own deletion for {}",
            deletion.purge_at.to_rfc3339()
        );
        self.record_server(user, AuditAction::AccountDeletionScheduled, detail)
            .await?;
        Ok(deletion)
    }

    /// Cancels a deletion the owner scheduled, while the recovery window runs, and enables the account again.
    pub async fn restore_account(&self, actor: &Identity) -> Result<()> {
        actor.require_full_scope()?;
        let user = &actor.user;
        let account = self.existing_account(user).await?;
        let Some(deletion) = self.owner_deletion(&account).await? else {
            return Err(PynError::InvalidRequest(
                "this account is not scheduled for deletion".into(),
            ));
        };
        if deletion.purge_at <= self.clock.now() {
            return Err(PynError::InvalidRequest(
                "the recovery window has ended; the account will be deleted".into(),
            ));
        }
        let event = self.draft(DomainEvent::AccountDeletionRestored {
            user: user.clone(),
            user_id: account.id,
        })?;
        if !self
            .store
            .restore_account_with_events(user, &[event])
            .await?
        {
            return Err(PynError::InvalidRequest(
                "this account is not scheduled for deletion".into(),
            ));
        }
        self.record_server(
            user,
            AuditAction::AccountDeletionRestored,
            format!("{user} restored their account"),
        )
        .await
    }

    /// Deletes a disabled account: at once, or by scheduling it after the recovery window. A reason is recorded.
    /// Server administrators only, never one's own account. Returns the schedule, or `None` once purged.
    pub async fn admin_delete_account(
        &self,
        actor: &Identity,
        user: &UserId,
        reason: &str,
        immediately: bool,
    ) -> Result<Option<AccountDeletion>> {
        self.require_admin_scope(actor, ServiceScope::ManageAccounts)
            .await?;
        let reason = clean_reason(reason)?;
        if &actor.user == user {
            return Err(PynError::InvalidRequest(
                "you cannot delete your own account here; use the account settings".into(),
            ));
        }
        let account = self.existing_account(user).await?;
        if account.disabled_at.is_none() {
            return Err(PynError::AccountNotDisabled(user.to_string()));
        }
        self.require_deletable(&account).await?;
        if account.is_admin && self.store.active_admin_count().await? == 0 {
            return Err(PynError::LastServerAdmin);
        }
        if immediately {
            self.purge(&actor.user, &account, Some(&reason), "deleted")
                .await?;
            return Ok(None);
        }
        let now = self.clock.now();
        let deletion = AccountDeletion {
            user: user.clone(),
            requested_by: actor.user.clone(),
            by_owner: false,
            reason: Some(reason.clone()),
            scheduled_at: now,
            purge_at: now + Duration::days(self.config.deletion_window_days),
        };
        let event = self.draft(DomainEvent::AccountDeletionScheduled {
            user: user.clone(),
            user_id: account.id,
            actor: actor.user.clone(),
            by_owner: false,
            reason: Some(reason.clone()),
            purge_at: deletion.purge_at,
        })?;
        match self
            .store
            .schedule_account_deletion_with_events(deletion.clone(), false, &[event])
            .await?
        {
            DeletionSchedule::Scheduled => {}
            DeletionSchedule::NoAccount => return Err(PynError::UserNotFound(user.to_string())),
            DeletionSchedule::LastAdmin => return Err(PynError::LastServerAdmin),
        }
        let detail = format!(
            "{user} scheduled for deletion on {}: {reason}",
            deletion.purge_at.to_rfc3339()
        );
        self.record_server(&actor.user, AuditAction::AccountDeletionScheduled, detail)
            .await?;
        Ok(Some(deletion))
    }

    /// Removes the account and reserves its name; `how` words the audit entry.
    async fn purge(
        &self,
        actor: &UserId,
        account: &AccountRecord,
        reason: Option<&str>,
        how: &str,
    ) -> Result<()> {
        let user = &account.user;
        let now = self.clock.now();
        let event = self.draft(DomainEvent::AccountDeleted {
            user: user.clone(),
            user_id: account.id.clone(),
            actor: actor.clone(),
            reason: reason.map(str::to_string),
        })?;
        let until = now + Duration::days(NAME_RESERVATION_DAYS);
        match self
            .store
            .purge_account_with_events(user, now, until, &[event])
            .await?
        {
            AccountPurge::Purged(_) => {}
            AccountPurge::NoAccount => return Err(PynError::UserNotFound(user.to_string())),
            AccountPurge::NotDisabled => {
                return Err(PynError::AccountNotDisabled(user.to_string()));
            }
        }
        let detail = match reason {
            Some(reason) => format!("{user} {how}: {reason}"),
            None => format!("{user} {how}"),
        };
        self.record_server(actor, AuditAction::AccountDeleted, detail)
            .await
    }

    /// One pass of the server's deletion task: purges accounts whose recovery window ended and accounts disabled
    /// longer than the retention, and settles expired name reservations. Safe to repeat and to run after a restart.
    pub async fn run_deletion_sweep(&self) -> Result<DeletionSweep> {
        let now = self.clock.now();
        let system = UserId::new(ACCOUNT_DELETION_ACTOR);
        let mut sweep = DeletionSweep::default();

        for due in self.store.deletions_due(now, SWEEP_BATCH).await? {
            let account = self.store.account(&due.user).await?;
            let Some(account) = account.filter(|a| a.disabled_at.is_some()) else {
                self.store.clear_account_deletion(&due.user).await?;
                sweep.dropped.push(due.user);
                continue;
            };
            let how = if due.by_owner {
                "deleted when the recovery window ended"
            } else {
                "deleted as scheduled"
            };
            match self
                .purge_checked(&system, &account, due.reason.as_deref(), how)
                .await
            {
                Ok(()) => sweep.purged.push(due.user),
                Err(e) => sweep.blocked.push((due.user, e.to_string())),
            }
        }

        let days = self.config.disabled_retention_days;
        if days > 0 {
            let cutoff = now - Duration::days(days);
            for account in self
                .store
                .disabled_accounts_before(cutoff, SWEEP_BATCH)
                .await?
            {
                let how = format!("deleted after {days} days disabled");
                match self.purge_checked(&system, &account, None, &how).await {
                    Ok(()) => sweep.purged.push(account.user),
                    Err(e) => sweep.blocked.push((account.user, e.to_string())),
                }
            }
        }

        for reservation in self
            .store
            .expired_name_reservations(now, SWEEP_BATCH)
            .await?
        {
            let name = UserId::new(reservation.name.clone());
            if self.name_in_records(&name).await? {
                self.store
                    .make_name_reservation_permanent(&reservation.name)
                    .await?;
            } else if self
                .store
                .release_name_reservation(&reservation.name)
                .await?
            {
                sweep.names_released += 1;
            }
        }
        Ok(sweep)
    }

    /// `purge` after the rules an account must pass first.
    async fn purge_checked(
        &self,
        actor: &UserId,
        account: &AccountRecord,
        reason: Option<&str>,
        how: &str,
    ) -> Result<()> {
        self.require_deletable(account).await?;
        if account.is_admin && self.store.active_admin_count().await? == 0 {
            return Err(PynError::LastServerAdmin);
        }
        self.purge(actor, account, reason, how).await
    }
}
