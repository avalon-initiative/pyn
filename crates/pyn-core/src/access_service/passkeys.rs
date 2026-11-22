//! Passkeys and security keys (#1105, decision #1127): WebAuthn registration, passkey sign-in and the fresh check, over
//! the in-house verification in `pyn-webauthn`.

use super::*;
use crate::relying_party::{self, RelyingParty};
use crate::two_factor::{
    self, CHALLENGE_MINUTES, ChallengePurpose, ChallengeRecord, MAX_PASSKEYS,
    NOTICE_PASSKEY_CLONE_SUSPECTED, NoticeRecord, PasskeyRecord,
};
use pyn_webauthn::{CounterVerdict, Expected};

/// What a client needs to start a registration ceremony.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasskeyCreation {
    pub ceremony: String,
    pub challenge: Vec<u8>,
    pub rp_id: String,
    pub user_handle: Vec<u8>,
    pub user_name: String,
    /// Credentials the account already holds, `(credential id, transports)`, so the authenticator is not registered twice.
    pub exclude: Vec<(Vec<u8>, Vec<String>)>,
    pub expires_at: DateTime<Utc>,
}

/// What a client needs to start an authentication ceremony. `allow` is empty for a sign-in that names no account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasskeyRequest {
    pub ceremony: String,
    pub challenge: Vec<u8>,
    pub rp_id: String,
    pub allow: Vec<(Vec<u8>, Vec<String>)>,
    pub expires_at: DateTime<Utc>,
}

/// An authenticator's answer to a registration challenge.
#[derive(Debug, Clone)]
pub struct PasskeyAttestation {
    pub client_data_json: Vec<u8>,
    pub attestation_object: Vec<u8>,
    pub transports: Vec<String>,
}

/// An authenticator's answer to an authentication challenge.
#[derive(Debug, Clone)]
pub struct PasskeyAssertion {
    pub credential_id: Vec<u8>,
    pub client_data_json: Vec<u8>,
    pub authenticator_data: Vec<u8>,
    pub signature: Vec<u8>,
    pub user_handle: Option<Vec<u8>>,
}

/// A passkey just registered, with the recovery codes when it is the account's first second factor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasskeyRegistered {
    pub passkey: PasskeyRecord,
    pub recovery_codes: Option<Vec<String>>,
    pub fresh_until: DateTime<Utc>,
}

fn invalid(why: impl std::fmt::Display) -> PynError {
    PynError::PasskeyInvalid(why.to_string())
}

fn random_bytes<const N: usize>() -> Result<[u8; N]> {
    let mut raw = [0u8; N];
    getrandom::fill(&mut raw).map_err(|e| PynError::Storage(format!("random source: {e}")))?;
    Ok(raw)
}

fn descriptors(passkeys: &[PasskeyRecord]) -> Vec<(Vec<u8>, Vec<String>)> {
    passkeys
        .iter()
        .map(|p| (p.credential_id.clone(), p.transports.clone()))
        .collect()
}

impl AccessService {
    /// The relying party derived from the public address, or `None` when the server cannot offer passkeys.
    pub fn relying_party(&self) -> Option<RelyingParty> {
        relying_party::relying_party(
            &self.public_url(),
            self.config.webauthn_rp_id.as_deref(),
            &self.config.allowed_origins,
        )
    }

    pub fn passkeys_available(&self) -> bool {
        self.relying_party().is_some()
    }

    fn require_relying_party(&self) -> Result<RelyingParty> {
        self.relying_party().ok_or(PynError::PasskeysUnavailable)
    }

    pub(super) async fn passkeys(&self, user: &UserId) -> Result<Vec<PasskeyRecord>> {
        self.store.passkeys_of(user).await
    }

    pub async fn list_passkeys(&self, actor: &Identity) -> Result<Vec<PasskeyRecord>> {
        actor.require_full_scope()?;
        self.existing_account(&actor.user).await?;
        self.passkeys(&actor.user).await
    }

    async fn issue_challenge(
        &self,
        user: Option<&UserId>,
        purpose: ChallengePurpose,
    ) -> Result<ChallengeRecord> {
        let now = self.clock.now();
        let challenge = ChallengeRecord {
            id: token::random_hex(16)?,
            user: user.cloned(),
            purpose,
            challenge: random_bytes::<32>()?.to_vec(),
            expires_at: now + Duration::minutes(CHALLENGE_MINUTES),
        };
        self.store.put_challenge(challenge.clone(), now).await?;
        Ok(challenge)
    }

    /// Takes the challenge behind `ceremony`, which is spent whatever happens next.
    async fn take_challenge(
        &self,
        ceremony: &str,
        purpose: ChallengePurpose,
        user: Option<&UserId>,
    ) -> Result<ChallengeRecord> {
        let taken = self
            .store
            .take_challenge(ceremony, self.clock.now())
            .await?;
        taken
            .filter(|c| c.purpose == purpose && c.user.as_ref() == user)
            .ok_or_else(|| invalid("the ceremony expired or was already used; start again"))
    }

    /// Starts registering a passkey. An account without a second factor proves its password (one with none, a signed-in
    /// web session); one that has a factor needs a fresh check.
    pub async fn begin_passkey_registration(
        &self,
        actor: &Identity,
        cookie: Option<&str>,
        password: Option<&str>,
    ) -> Result<PasskeyCreation> {
        actor.require_full_scope()?;
        let rp = self.require_relying_party()?;
        self.existing_account(&actor.user).await?;
        self.require_enrolment_proof(actor, cookie, password)
            .await?;
        let existing = self.passkeys(&actor.user).await?;
        if existing.len() >= MAX_PASSKEYS {
            return Err(PynError::InvalidRequest(format!(
                "an account holds at most {MAX_PASSKEYS} passkeys"
            )));
        }
        let user_handle = match existing.first() {
            Some(p) => p.user_handle.clone(),
            None => random_bytes::<32>()?.to_vec(),
        };
        let challenge = self
            .issue_challenge(Some(&actor.user), ChallengePurpose::Register)
            .await?;
        Ok(PasskeyCreation {
            ceremony: challenge.id,
            challenge: challenge.challenge,
            rp_id: rp.id,
            user_handle,
            user_name: actor.user.to_string(),
            exclude: descriptors(&existing),
            expires_at: challenge.expires_at,
        })
    }

    /// Verifies the authenticator's answer and stores the passkey. The account's first second factor also gets its
    /// recovery codes, shown once, and the credential counts as freshly checked.
    pub async fn finish_passkey_registration(
        &self,
        actor: &Identity,
        cookie: Option<&str>,
        ceremony: &str,
        name: Option<&str>,
        answer: &PasskeyAttestation,
    ) -> Result<PasskeyRegistered> {
        actor.require_full_scope()?;
        let rp = self.require_relying_party()?;
        let user = &actor.user;
        self.existing_account(user).await?;
        let challenge = self
            .take_challenge(ceremony, ChallengePurpose::Register, Some(user))
            .await?;
        let origins: Vec<&str> = rp.origins.iter().map(String::as_str).collect();
        let transports: Vec<&str> = answer.transports.iter().map(String::as_str).collect();
        let registration = pyn_webauthn::verify_registration(
            &answer.client_data_json,
            &answer.attestation_object,
            &transports,
            &Expected {
                challenge: &challenge.challenge,
                rp_id: &rp.id,
                origins: &origins,
                require_user_verification: true,
            },
        )
        .map_err(invalid)?;
        let name = two_factor::validate_label(name.unwrap_or("Passkey"))?;
        let existing = self.passkeys(user).await?;
        if existing.len() >= MAX_PASSKEYS {
            return Err(PynError::InvalidRequest(format!(
                "an account holds at most {MAX_PASSKEYS} passkeys"
            )));
        }
        let first = !self.has_second_factor(user).await?;
        let now = self.clock.now();
        let passkey = PasskeyRecord {
            id: token::random_hex(6)?,
            user: user.clone(),
            name,
            credential_id: registration.credential_id,
            public_key: registration.cose_key,
            user_handle: match existing.first() {
                Some(p) => p.user_handle.clone(),
                None => random_bytes::<32>()?.to_vec(),
            },
            sign_count: registration.sign_count,
            transports: registration.transports,
            backup_eligible: registration.backup_eligible,
            backup_state: registration.backup_state,
            created_at: now,
            last_used_at: None,
        };
        if !self.store.create_passkey(passkey.clone()).await? {
            return Err(invalid("that authenticator is already registered"));
        }
        let recovery_codes = if first {
            Some(self.new_recovery_codes(user).await?)
        } else {
            None
        };
        let fresh_until = self.mark_credential_fresh(actor, cookie).await?;
        self.record_server(
            user.into(),
            AuditAction::PasskeyAdded,
            format!("{user} added the passkey {:?}", passkey.name),
        )
        .await?;
        Ok(PasskeyRegistered {
            passkey,
            recovery_codes,
            fresh_until,
        })
    }

    pub async fn rename_passkey(
        &self,
        actor: &Identity,
        id: &str,
        name: &str,
    ) -> Result<PasskeyRecord> {
        actor.require_full_scope()?;
        self.existing_account(&actor.user).await?;
        let name = two_factor::validate_label(name)?;
        if !self.store.rename_passkey(&actor.user, id, &name).await? {
            return Err(PynError::PasskeyNotFound(id.to_string()));
        }
        self.passkeys(&actor.user)
            .await?
            .into_iter()
            .find(|p| p.id == id)
            .ok_or_else(|| PynError::PasskeyNotFound(id.to_string()))
    }

    /// Removes a passkey on a fresh check. Removing the last second factor turns two-factor authentication off, which a
    /// requirement then holds the account to after its grace period like any other removal.
    pub async fn remove_passkey(
        &self,
        actor: &Identity,
        cookie: Option<&str>,
        id: &str,
    ) -> Result<()> {
        actor.require_full_scope()?;
        self.existing_account(&actor.user).await?;
        self.require_fresh(actor, cookie).await?;
        match self.store.remove_passkey(&actor.user, id).await? {
            FactorRemoval::NotFound => Err(PynError::PasskeyNotFound(id.to_string())),
            FactorRemoval::Removed { second_factor_left } => {
                let user = &actor.user;
                let detail = if second_factor_left {
                    format!("{user} removed a passkey")
                } else {
                    format!("{user} removed a passkey, so two-factor authentication is off")
                };
                self.record_server(user.into(), AuditAction::PasskeyRemoved, detail)
                    .await
            }
        }
    }

    /// Starts a passkey sign-in. With a user name the account's passkeys are offered to the browser; without one the
    /// browser picks a discoverable passkey. An unknown name answers like a discoverable sign-in.
    pub async fn begin_passkey_sign_in(
        &self,
        username: Option<&str>,
        client: Option<&str>,
    ) -> Result<PasskeyRequest> {
        let rp = self.require_relying_party()?;
        if let Some(c) = client {
            let key = format!("passkey-begin:client:{}", bounded_key(c));
            self.count_limited(&key, self.config.limits.sign_in_per_client, SIGN_IN_WINDOW)
                .await?;
        }
        let allow = match username.filter(|u| !u.is_empty()) {
            Some(name) => descriptors(&self.passkeys(&UserId::new(name)).await?),
            None => Vec::new(),
        };
        let challenge = self.issue_challenge(None, ChallengePurpose::SignIn).await?;
        Ok(PasskeyRequest {
            ceremony: challenge.id,
            challenge: challenge.challenge,
            rp_id: rp.id,
            allow,
            expires_at: challenge.expires_at,
        })
    }

    /// Completes a passkey sign-in and starts a web session. A user-verified passkey stands for the password and the
    /// second factor, so the session starts with its fresh check open.
    pub async fn finish_passkey_sign_in(
        &self,
        ceremony: &str,
        answer: &PasskeyAssertion,
        client: Option<&str>,
    ) -> Result<(SessionRecord, String)> {
        let rp = self.require_relying_party()?;
        let key = client.map(|c| format!("passkey:client:{}", bounded_key(c)));
        if let Some(key) = &key {
            self.count_limited(key, self.config.limits.sign_in_per_client, SIGN_IN_WINDOW)
                .await?;
        }
        let challenge = self
            .take_challenge(ceremony, ChallengePurpose::SignIn, None)
            .await?;
        let passkey = self.check_assertion(&rp, &challenge, answer, None).await?;
        let user = passkey.user;
        self.require_may_sign_in(&user).await?;
        if let Some(key) = &key {
            self.limits.refund(key, self.clock.now()).await?;
        }
        let (record, cookie) = self.open_session(user.clone()).await?;
        let now = self.clock.now();
        let until = now + Duration::minutes(two_factor::FRESH_MINUTES);
        self.store
            .mark_fresh(&format!("session:{}", record.id_hash), &user, until, now)
            .await?;
        Ok((record, cookie))
    }

    /// Starts a fresh check with one of the caller's passkeys.
    pub async fn begin_passkey_verify(&self, actor: &Identity) -> Result<PasskeyRequest> {
        actor.require_full_scope()?;
        let rp = self.require_relying_party()?;
        self.existing_account(&actor.user).await?;
        let passkeys = self.passkeys(&actor.user).await?;
        if passkeys.is_empty() {
            return Err(PynError::InvalidRequest(
                "this account has no passkey".into(),
            ));
        }
        let challenge = self
            .issue_challenge(Some(&actor.user), ChallengePurpose::Verify)
            .await?;
        Ok(PasskeyRequest {
            ceremony: challenge.id,
            challenge: challenge.challenge,
            rp_id: rp.id,
            allow: descriptors(&passkeys),
            expires_at: challenge.expires_at,
        })
    }

    /// Checks a passkey assertion and opens sensitive actions on the calling credential for `FRESH_MINUTES`. Failures
    /// count toward the account's second-factor delay.
    pub async fn finish_passkey_verify(
        &self,
        actor: &Identity,
        cookie: Option<&str>,
        ceremony: &str,
        answer: &PasskeyAssertion,
    ) -> Result<DateTime<Utc>> {
        actor.require_full_scope()?;
        let rp = self.require_relying_party()?;
        let user = &actor.user;
        self.existing_account(user).await?;
        let now = self.clock.now();
        if let Err(until) = self.store.count_factor_attempt(user, now).await? {
            return Err(two_factor::too_many(until, now));
        }
        let challenge = self
            .take_challenge(ceremony, ChallengePurpose::Verify, Some(user))
            .await?;
        self.check_assertion(&rp, &challenge, answer, Some(user))
            .await?;
        self.store.clear_factor_attempts(user).await?;
        self.mark_credential_fresh(actor, cookie).await
    }

    /// Verifies an assertion against the stored passkey and its counter and records the use. `owner` is the account the
    /// passkey must belong to, when the caller is already known.
    async fn check_assertion(
        &self,
        rp: &RelyingParty,
        challenge: &ChallengeRecord,
        answer: &PasskeyAssertion,
        owner: Option<&UserId>,
    ) -> Result<PasskeyRecord> {
        let unknown = || invalid("that passkey is not registered");
        let passkey = self
            .store
            .passkey_by_credential(&answer.credential_id)
            .await?
            .filter(|p| owner.is_none_or(|o| &p.user == o))
            .ok_or_else(unknown)?;
        if answer
            .user_handle
            .as_ref()
            .is_some_and(|h| !h.is_empty() && *h != passkey.user_handle)
        {
            return Err(unknown());
        }
        let origins: Vec<&str> = rp.origins.iter().map(String::as_str).collect();
        let assertion = pyn_webauthn::verify_assertion(
            &answer.client_data_json,
            &answer.authenticator_data,
            &answer.signature,
            &passkey.public_key,
            passkey.sign_count,
            &Expected {
                challenge: &challenge.challenge,
                rp_id: &rp.id,
                origins: &origins,
                require_user_verification: true,
            },
        )
        .map_err(invalid)?;
        if assertion.counter == CounterVerdict::Regressed {
            self.report_clone(&passkey).await?;
            return Err(invalid(
                "its signature counter did not increase, so it may have been cloned",
            ));
        }
        let recorded = self
            .store
            .record_passkey_use(
                &passkey.user,
                &passkey.id,
                passkey.sign_count,
                assertion.sign_count,
                assertion.backup_state,
                self.clock.now(),
            )
            .await?;
        if !recorded {
            return Err(invalid("that sign-in was already used; start again"));
        }
        Ok(passkey)
    }

    async fn report_clone(&self, passkey: &PasskeyRecord) -> Result<()> {
        let user = &passkey.user;
        self.store
            .add_notice(NoticeRecord {
                id: token::random_hex(6)?,
                user: user.clone(),
                kind: NOTICE_PASSKEY_CLONE_SUSPECTED.to_string(),
                detail: format!(
                    "the passkey {:?} reported a signature counter that did not increase and was refused; remove it if it is not yours or was copied",
                    passkey.name
                ),
                created_at: self.clock.now(),
            })
            .await?;
        self.record_server(
            user.into(),
            AuditAction::PasskeyCloneSuspected,
            format!(
                "the passkey {:?} of {user} was refused for a counter that did not increase",
                passkey.name
            ),
        )
        .await
    }
}
