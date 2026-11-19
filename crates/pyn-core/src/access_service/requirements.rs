//! Required second factors (#1106, decision #1097): the server's and organizations' requirements, the grace period,
//! enforcement after it, and the administrator's reset.

use super::*;
use crate::two_factor::{self, NOTICE_TWO_FACTOR_RESET, NoticeRecord, RequiredBy, Requirement};

impl AccessService {
    /// Whether the account holds a second factor that counts at sign-in. The one place enforcement asks, so a new kind
    /// of factor joins by being counted here.
    pub async fn has_second_factor(&self, user: &UserId) -> Result<bool> {
        Ok(!self.confirmed_factors(user).await?.is_empty())
    }

    /// The requirement with the earliest deadline that applies to the account, whether or not it is met.
    pub async fn two_factor_requirement(&self, user: &UserId) -> Result<Option<Requirement>> {
        let server = self.server_two_factor();
        let orgs = self.store.member_two_factor(user).await?;
        if server.is_none() && orgs.is_empty() {
            return Ok(None);
        }
        let created = self.existing_account(user).await?.created_at;
        let from_server = server.map(|p| Requirement {
            by: RequiredBy::Server,
            deadline: p.deadline(created),
        });
        let from_orgs = orgs.into_iter().map(|(org, p)| Requirement {
            by: RequiredBy::Org(org),
            deadline: p.deadline(created),
        });
        Ok(from_server
            .into_iter()
            .chain(from_orgs)
            .min_by_key(|r| r.deadline))
    }

    /// Whether `requirement` has reached its deadline now.
    pub fn two_factor_enforced(&self, requirement: &Requirement) -> bool {
        requirement.enforced(self.clock.now())
    }

    /// The requirement the account still has to meet: none once it holds a second factor.
    pub async fn unmet_two_factor_requirement(&self, user: &UserId) -> Result<Option<Requirement>> {
        let Some(requirement) = self.two_factor_requirement(user).await? else {
            return Ok(None);
        };
        Ok((!self.has_second_factor(user).await?).then_some(requirement))
    }

    /// Past the grace period, a session or sign-in token of an account that must have a second factor and has none
    /// reaches only the enrolment routes. Other tokens, SSH keys and service credentials are automation and are not
    /// challenged (decision #1097).
    pub async fn enforce_two_factor(&self, who: &Identity) -> Result<()> {
        let challenged = match &who.credential {
            Credential::Session => true,
            Credential::Token(t) => t.sign_in,
            _ => false,
        };
        if !challenged {
            return Ok(());
        }
        match self.unmet_two_factor_requirement(&who.user).await? {
            Some(r) if self.two_factor_enforced(&r) => Err(PynError::TwoFactorEnrolmentRequired),
            _ => Ok(()),
        }
    }

    /// The organization's requirement. Owners only.
    pub async fn org_two_factor_policy(
        &self,
        actor: &Identity,
        org: &UserId,
    ) -> Result<Option<TwoFactorPolicy>> {
        actor.require_org_grant(org, ORG_ADMINISTRATION_READ)?;
        self.org(org).await?;
        self.require_org_owner(org, &actor.user).await?;
        self.store.org_two_factor(org).await
    }

    /// Requires a second factor of the organization's members, or lifts it. Owners only; setting the same
    /// requirement again changes nothing.
    pub async fn set_org_two_factor(
        &self,
        actor: &Identity,
        org: &UserId,
        required: bool,
        grace_days: Option<u32>,
    ) -> Result<Option<TwoFactorPolicy>> {
        actor.require_org_grant(org, ORG_ADMINISTRATION_WRITE)?;
        self.org(org).await?;
        self.require_org_owner(org, &actor.user).await?;
        self.ensure_owner_writable(org).await?;
        let grace = two_factor::validate_grace_days(grace_days)?;
        let previous = self.store.org_two_factor(org).await?;
        if previous.is_some() == required && previous.is_none_or(|p| p.grace_days == grace) {
            return Ok(previous);
        }
        let policy = required.then(|| TwoFactorPolicy {
            since: self.clock.now(),
            grace_days: grace,
        });
        self.store.set_org_two_factor(org, policy).await?;
        let detail = match policy {
            Some(p) => format!(
                "two-factor authentication required of members, grace {} days",
                p.grace_days
            ),
            None => "two-factor authentication no longer required of members".to_string(),
        };
        self.record(
            AuditScope::Org(org.clone()),
            &actor.user,
            AuditAction::OrgTwoFactorRequirementChanged,
            detail,
        )
        .await?;
        Ok(policy)
    }

    /// Removes every second factor and recovery code of `user`, records it and leaves a notice for the account. A
    /// server administrator only, on a fresh check; an administrator's own account is reset on the host instead.
    pub async fn reset_two_factor(
        &self,
        actor: &Identity,
        cookie: Option<&str>,
        user: &UserId,
    ) -> Result<usize> {
        self.require_server_admin(actor).await?;
        self.require_fresh(actor, cookie).await?;
        let target = self.existing_account(user).await?;
        if target.is_admin {
            return Err(PynError::InvalidRequest(
                "an administrator's factors are reset on the host with `pyn-server recover-admin`"
                    .into(),
            ));
        }
        let removed = self.store.clear_two_factor(user).await?;
        let now = self.clock.now();
        self.store
            .add_notice(NoticeRecord {
                id: token::random_hex(6)?,
                user: user.clone(),
                kind: NOTICE_TWO_FACTOR_RESET.to_string(),
                detail: format!("{} reset your second factors", actor.user),
                created_at: now,
            })
            .await?;
        self.record_server(
            &actor.user,
            AuditAction::TwoFactorReset,
            format!("{user}'s second factors reset, {removed} removed"),
        )
        .await?;
        Ok(removed)
    }

    pub async fn notices(&self, actor: &Identity) -> Result<Vec<NoticeRecord>> {
        actor.require_full_scope()?;
        self.store.notices_of(&actor.user).await
    }

    pub async fn dismiss_notice(&self, actor: &Identity, id: &str) -> Result<()> {
        actor.require_full_scope()?;
        match self.store.dismiss_notice(&actor.user, id).await? {
            true => Ok(()),
            false => Err(PynError::NoticeNotFound(id.to_string())),
        }
    }
}
