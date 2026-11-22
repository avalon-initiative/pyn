//! Second factors (#1104, decision #1097): enrolment, the sign-in challenge, recovery codes and the fresh check.

use super::*;
use crate::totp;
use crate::two_factor::{
    self, FRESH_MINUTES, FactorKind, FactorRecord, FactorRemoval, MAX_FACTORS, PasskeyRecord,
};

/// What an authenticator-app enrolment hands back: the seed to show once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TotpEnrolment {
    pub factor: FactorRecord,
    /// The seed as base32, for manual entry.
    pub secret: String,
    /// The `otpauth://` URI a QR code carries.
    pub uri: String,
}

/// An account's confirmed factors and how many recovery codes are left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TwoFactorStatus {
    pub factors: Vec<FactorRecord>,
    pub passkeys: Vec<PasskeyRecord>,
    pub recovery_codes_left: usize,
}

/// A factor just confirmed, with the recovery codes when it is the account's first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TotpConfirmed {
    pub factor: FactorRecord,
    pub recovery_codes: Option<Vec<String>>,
    pub fresh_until: DateTime<Utc>,
}

impl AccessService {
    /// The key a credential's fresh check is kept under: a web session (by its cookie) or a sign-in token. Other
    /// credentials have none.
    fn fresh_key(who: &Identity, cookie: Option<&str>) -> Option<String> {
        match &who.credential {
            Credential::Session => cookie.map(|c| format!("session:{}", session::hash(c))),
            Credential::Token(t) if t.sign_in => Some(format!("token:{}", t.id)),
            _ => None,
        }
    }

    pub(super) async fn confirmed_factors(&self, user: &UserId) -> Result<Vec<FactorRecord>> {
        let mut factors = self.store.factors_of(user).await?;
        factors.retain(|f| f.confirmed_at.is_some());
        Ok(factors)
    }

    pub(super) async fn mark_credential_fresh(
        &self,
        who: &Identity,
        cookie: Option<&str>,
    ) -> Result<DateTime<Utc>> {
        let now = self.clock.now();
        let until = now + Duration::minutes(FRESH_MINUTES);
        if let Some(key) = Self::fresh_key(who, cookie) {
            self.store.mark_fresh(&key, &who.user, until, now).await?;
        }
        Ok(until)
    }

    /// Sensitive actions of a session or sign-in token need a recent second-factor check when the account has a
    /// factor. A fine-grained token is not challenged (decision #1097); the development identity has no account.
    pub async fn require_fresh(&self, who: &Identity, cookie: Option<&str>) -> Result<()> {
        if !matches!(&who.credential, Credential::Session)
            && !matches!(&who.credential, Credential::Token(t) if t.sign_in)
        {
            return Ok(());
        }
        if !self.has_second_factor(&who.user).await? {
            return Ok(());
        }
        let key = Self::fresh_key(who, cookie).ok_or(PynError::FreshCheckRequired)?;
        match self.store.fresh_until(&key).await? {
            Some(until) if until > self.clock.now() => Ok(()),
            _ => Err(PynError::FreshCheckRequired),
        }
    }

    /// Counts an attempt, then checks `code` as an authenticator code of any confirmed factor or an unused recovery
    /// code. A code is good once. Attempts past the free ones wait a growing delay (`429 too_many_attempts`).
    async fn check_second_factor(&self, user: &UserId, code: &str) -> Result<()> {
        let now = self.clock.now();
        if let Err(until) = self.store.count_factor_attempt(user, now).await? {
            return Err(two_factor::too_many(until, now));
        }
        if two_factor::looks_like_recovery_code(code) {
            let hash = two_factor::hash_recovery_code(code);
            if self.store.use_recovery_code(user, &hash, now).await? {
                self.store.clear_factor_attempts(user).await?;
                let left = self.store.unused_recovery_codes(user).await?;
                return self
                    .record_server(
                        user.into(),
                        AuditAction::RecoveryCodeUsed,
                        format!("{user} used a recovery code, {left} left"),
                    )
                    .await;
            }
            return Err(PynError::SecondFactorInvalid);
        }
        for factor in self.confirmed_factors(user).await? {
            if self.accepts_code(&factor, code, now).await? {
                self.store.clear_factor_attempts(user).await?;
                return Ok(());
            }
        }
        Err(PynError::SecondFactorInvalid)
    }

    async fn accepts_code(
        &self,
        factor: &FactorRecord,
        code: &str,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        let seed = two_factor::reveal_seed(self.webhook_settings.secret_key.as_ref(), factor)?;
        let Some(step) = totp::verify(&seed, code, now.timestamp(), factor.last_step) else {
            return Ok(false);
        };
        self.store
            .accept_factor_step(&factor.user, &factor.id, step)
            .await
    }

    /// Passes a sign-in's second step. `Ok(false)` when the account has no factor, so nothing was asked.
    async fn sign_in_second_factor(&self, user: &UserId, code: Option<&str>) -> Result<bool> {
        if !self.has_second_factor(user).await? {
            return Ok(false);
        }
        let code = code.ok_or(PynError::SecondFactorRequired)?;
        self.check_second_factor(user, code).await?;
        Ok(true)
    }

    /// Like `login`, with the authenticator or recovery code an account with a second factor must send.
    pub async fn login_with(
        &self,
        username: &str,
        password: &str,
        code: Option<&str>,
        client: Option<&str>,
    ) -> Result<(TokenRecord, String)> {
        let user = self.verify_sign_in(username, password, client).await?;
        let passed = self.sign_in_second_factor(&user, code).await?;
        let (record, token) = self.issue_sign_in_token(&user).await?;
        if passed {
            let now = self.clock.now();
            let until = now + Duration::minutes(FRESH_MINUTES);
            self.store
                .mark_fresh(&format!("token:{}", record.id), &user, until, now)
                .await?;
        }
        Ok((record, token))
    }

    /// Like `start_session`, with the second factor's code.
    pub async fn start_session_with(
        &self,
        username: &str,
        password: &str,
        code: Option<&str>,
        client: Option<&str>,
    ) -> Result<(SessionRecord, String)> {
        let user = self.verify_sign_in(username, password, client).await?;
        let passed = self.sign_in_second_factor(&user, code).await?;
        let (record, cookie) = self.open_session(user.clone()).await?;
        if passed {
            let now = self.clock.now();
            let until = now + Duration::minutes(FRESH_MINUTES);
            self.store
                .mark_fresh(&format!("session:{}", record.id_hash), &user, until, now)
                .await?;
        }
        Ok((record, cookie))
    }

    pub async fn two_factor_status(&self, actor: &Identity) -> Result<TwoFactorStatus> {
        actor.require_full_scope()?;
        self.existing_account(&actor.user).await?;
        Ok(TwoFactorStatus {
            factors: self.confirmed_factors(&actor.user).await?,
            passkeys: self.passkeys(&actor.user).await?,
            recovery_codes_left: self.store.unused_recovery_codes(&actor.user).await?,
        })
    }

    /// What enrolling a factor asks: the password while the account has no second factor, a fresh check once it has one.
    pub(super) async fn require_enrolment_proof(
        &self,
        actor: &Identity,
        cookie: Option<&str>,
        password: Option<&str>,
    ) -> Result<()> {
        if self.has_second_factor(&actor.user).await? {
            return self.require_fresh(actor, cookie).await;
        }
        if let Some(hash) = self.store.password_hash(&actor.user).await? {
            self.check_current_password(actor, &hash, password).await?;
        }
        Ok(())
    }

    /// Starts an authenticator-app enrolment. An account that has a factor needs a fresh check; one that has none
    /// proves its password instead (an account with none, a signed-in web session).
    pub async fn begin_totp(
        &self,
        actor: &Identity,
        cookie: Option<&str>,
        label: Option<&str>,
        password: Option<&str>,
    ) -> Result<TotpEnrolment> {
        actor.require_full_scope()?;
        self.existing_account(&actor.user).await?;
        self.require_enrolment_proof(actor, cookie, password)
            .await?;
        let confirmed = self.confirmed_factors(&actor.user).await?;
        if confirmed.len() >= MAX_FACTORS {
            return Err(PynError::InvalidRequest(format!(
                "an account holds at most {MAX_FACTORS} second factors"
            )));
        }
        let label = two_factor::validate_label(label.unwrap_or("Authenticator app"))?;
        let seed = totp::generate_secret()?;
        let sealed = two_factor::seal_seed(self.webhook_settings.secret_key.as_ref(), &seed)?;
        let factor = FactorRecord {
            id: token::random_hex(6)?,
            user: actor.user.clone(),
            kind: FactorKind::Totp,
            label,
            secret: sealed.secret,
            encrypted: sealed.encrypted,
            key_id: sealed.key_id,
            created_at: self.clock.now(),
            confirmed_at: None,
            last_step: None,
        };
        self.store.create_factor(factor.clone()).await?;
        let issuer = match self.store.server_settings().await? {
            Some(ServerSettings {
                server_name: Some(name),
                ..
            }) if !name.trim().is_empty() => format!("pyn {name}"),
            _ => "pyn".to_string(),
        };
        Ok(TotpEnrolment {
            uri: totp::otpauth_uri(&issuer, actor.user.as_str(), &seed),
            secret: totp::base32(&seed),
            factor,
        })
    }

    /// Confirms the pending factor with a code from the app. The account's first factor also gets its recovery codes,
    /// shown once, and the credential counts as freshly checked.
    pub async fn confirm_totp(
        &self,
        actor: &Identity,
        cookie: Option<&str>,
        id: &str,
        code: &str,
    ) -> Result<TotpConfirmed> {
        actor.require_full_scope()?;
        let user = &actor.user;
        let factors = self.store.factors_of(user).await?;
        let pending = factors
            .iter()
            .find(|f| f.id == id && f.confirmed_at.is_none())
            .ok_or_else(|| PynError::FactorNotFound(id.to_string()))?;
        let first = !self.has_second_factor(user).await?;
        let now = self.clock.now();
        if let Err(until) = self.store.count_factor_attempt(user, now).await? {
            return Err(two_factor::too_many(until, now));
        }
        if !self.accepts_code(pending, code, now).await? {
            return Err(PynError::SecondFactorInvalid);
        }
        if !self.store.confirm_factor(user, id, now).await? {
            return Err(PynError::FactorNotFound(id.to_string()));
        }
        self.store.clear_factor_attempts(user).await?;
        let recovery_codes = if first {
            Some(self.new_recovery_codes(user).await?)
        } else {
            None
        };
        let fresh_until = self.mark_credential_fresh(actor, cookie).await?;
        self.record_server(
            user.into(),
            AuditAction::TwoFactorAdded,
            format!("{user} added the authenticator app {:?}", pending.label),
        )
        .await?;
        let mut factor = pending.clone();
        factor.confirmed_at = Some(now);
        Ok(TotpConfirmed {
            factor,
            recovery_codes,
            fresh_until,
        })
    }

    pub(super) async fn new_recovery_codes(&self, user: &UserId) -> Result<Vec<String>> {
        let codes = two_factor::generate_recovery_codes()?;
        let hashes: Vec<String> = codes
            .iter()
            .map(|c| two_factor::hash_recovery_code(c))
            .collect();
        self.store
            .replace_recovery_codes(user, &hashes, self.clock.now())
            .await?;
        Ok(codes)
    }

    /// Replaces the recovery codes with ten new ones, shown once. Needs a fresh check.
    pub async fn regenerate_recovery_codes(
        &self,
        actor: &Identity,
        cookie: Option<&str>,
    ) -> Result<Vec<String>> {
        actor.require_full_scope()?;
        self.existing_account(&actor.user).await?;
        if !self.has_second_factor(&actor.user).await? {
            return Err(PynError::InvalidRequest(
                "two-factor authentication is not on for this account".into(),
            ));
        }
        self.require_fresh(actor, cookie).await?;
        let codes = self.new_recovery_codes(&actor.user).await?;
        self.record_server(
            actor.into(),
            AuditAction::RecoveryCodesRegenerated,
            format!("{} made new recovery codes", actor.user),
        )
        .await?;
        Ok(codes)
    }

    /// Removes a factor (or cancels a pending enrolment). Removing the last one turns two-factor authentication off.
    pub async fn remove_factor(
        &self,
        actor: &Identity,
        cookie: Option<&str>,
        id: &str,
    ) -> Result<()> {
        actor.require_full_scope()?;
        self.existing_account(&actor.user).await?;
        self.require_fresh(actor, cookie).await?;
        match self.store.remove_factor(&actor.user, id).await? {
            FactorRemoval::NotFound => Err(PynError::FactorNotFound(id.to_string())),
            FactorRemoval::Removed { second_factor_left } => {
                let user = &actor.user;
                let detail = if second_factor_left {
                    format!("{user} removed a second factor")
                } else {
                    format!("{user} removed a second factor, so two-factor authentication is off")
                };
                self.record_server(user.into(), AuditAction::TwoFactorRemoved, detail)
                    .await
            }
        }
    }

    /// Checks a code (authenticator or recovery) and opens sensitive actions on the calling credential for
    /// `FRESH_MINUTES`. Returns when that ends.
    pub async fn verify_second_factor(
        &self,
        actor: &Identity,
        cookie: Option<&str>,
        code: &str,
    ) -> Result<DateTime<Utc>> {
        actor.require_full_scope()?;
        self.existing_account(&actor.user).await?;
        if !self.has_second_factor(&actor.user).await? {
            return Err(PynError::InvalidRequest(
                "two-factor authentication is not on for this account".into(),
            ));
        }
        self.check_second_factor(&actor.user, code).await?;
        self.mark_credential_fresh(actor, cookie).await
    }
}
