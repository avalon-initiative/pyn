//! Sign-in and account linking through an OpenID Connect provider. The provider's proof of identity ends in the
//! same web session a password sign-in opens.

use super::*;
use crate::oidc::{
    AuthorizationRequest, CodeExchange, ExternalClaims, FlowSecrets, username_candidates,
    validate_return_to,
};

const FLOW_TTL: Duration = Duration::minutes(10);

/// A request to start a sign-in, or with `link` to attach the provider to the signed-in account.
#[derive(Debug, Clone, Copy, Default)]
pub struct OidcStart<'a> {
    pub link: Option<&'a Identity>,
    /// A path in the web app to come back to.
    pub return_to: Option<&'a str>,
    /// The caller's network address, for rate limits.
    pub client: Option<&'a str>,
}

/// Where to send the browser, and the value to keep in a cookie that ties the flow to this browser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OidcBegin {
    pub url: String,
    pub binding: String,
}

/// The provider's redirect back to the server.
#[derive(Debug, Clone, Copy)]
pub struct OidcFinish<'a> {
    pub code: &'a str,
    pub state: &'a str,
    /// The cookie value set when the flow began.
    pub binding: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternalSignIn {
    SignedIn {
        session: SessionRecord,
        /// The cookie value, which is not stored.
        cookie: String,
        return_to: Option<String>,
        created: bool,
    },
    Linked {
        user: UserId,
        return_to: Option<String>,
    },
}

impl AccessService {
    /// Offers sign-in with an OpenID Connect provider.
    pub fn with_oidc(mut self, provider: Arc<dyn OidcProvider>) -> Self {
        self.oidc = Some(provider);
        self
    }

    /// The name of the configured provider, if any.
    pub fn oidc_name(&self) -> Option<&str> {
        self.oidc.as_deref().map(|p| p.display_name())
    }

    /// Whether anyone can sign in with a password; administrators always can.
    pub fn password_sign_in_enabled(&self) -> bool {
        self.oidc.is_none() || self.config.external.password_sign_in
    }

    pub(super) async fn require_password_sign_in(&self, user: &UserId) -> Result<()> {
        if self.password_sign_in_enabled() {
            return Ok(());
        }
        match self.store.account(user).await? {
            Some(account) if account.is_admin => Ok(()),
            _ => Err(PynError::PasswordSignInDisabled),
        }
    }

    fn redirect_uri(&self) -> String {
        self.config
            .external
            .redirect_url
            .clone()
            .unwrap_or_else(|| {
                format!(
                    "{}/v1/oidc/callback",
                    self.public_url().trim_end_matches('/')
                )
            })
    }

    /// Starts an authorization-code flow with PKCE. The state, nonce and verifier are kept in the store, so any
    /// server instance can finish it.
    pub async fn begin_oidc(&self, start: OidcStart<'_>) -> Result<OidcBegin> {
        let provider = self.oidc.as_ref().ok_or(PynError::OidcNotConfigured)?;
        if let Some(path) = start.return_to {
            validate_return_to(path)?;
        }
        if let Some(client) = start.client {
            self.count_limited(
                &format!("oidc:client:{client}"),
                self.config.limits.sign_in_per_client,
                SIGN_IN_WINDOW,
            )
            .await?;
        }
        let link_user = match start.link {
            Some(actor) => {
                self.store
                    .account(&actor.user)
                    .await?
                    .ok_or_else(|| PynError::UserNotFound(actor.user.to_string()))?;
                self.require_active(&actor.user).await?;
                Some(actor.user.clone())
            }
            None => None,
        };
        let now = self.clock.now();
        self.store.delete_expired_oidc_flows(now).await?;
        let secrets = FlowSecrets::generate()?;
        let url = provider
            .authorization_url(&AuthorizationRequest {
                state: &secrets.state,
                nonce: &secrets.nonce,
                code_challenge: &secrets.code_challenge,
                redirect_uri: &self.redirect_uri(),
            })
            .await?;
        self.store
            .put_oidc_flow(OidcFlow {
                state_hash: token::hash_secret(&secrets.state),
                binding_hash: token::hash_secret(&secrets.binding),
                nonce: secrets.nonce,
                code_verifier: secrets.code_verifier,
                link_user,
                return_to: start.return_to.map(str::to_string),
                expires_at: now + FLOW_TTL,
            })
            .await?;
        Ok(OidcBegin {
            url,
            binding: secrets.binding,
        })
    }

    /// Completes the flow: validates the callback against the stored flow, then links the identity or signs in,
    /// creating the account when the operator allows it. Never links by email address.
    pub async fn finish_oidc(&self, finish: OidcFinish<'_>) -> Result<ExternalSignIn> {
        let provider = self.oidc.as_ref().ok_or(PynError::OidcNotConfigured)?;
        let failed = |why: &str| PynError::ExternalSignInFailed(why.to_string());
        let flow = self
            .store
            .take_oidc_flow(&token::hash_secret(finish.state))
            .await?
            .filter(|f| f.expires_at > self.clock.now())
            .ok_or_else(|| failed("the sign-in expired or was already used; start again"))?;
        let bound = finish
            .binding
            .is_some_and(|b| token::hashes_match(&flow.binding_hash, &token::hash_secret(b)));
        if !bound {
            return Err(failed("the sign-in did not start in this browser"));
        }
        let claims = provider
            .exchange(&CodeExchange {
                code: finish.code,
                code_verifier: &flow.code_verifier,
                redirect_uri: &self.redirect_uri(),
                nonce: &flow.nonce,
            })
            .await?;
        if claims.subject.is_empty() || claims.issuer.is_empty() {
            return Err(failed("the provider did not identify the person"));
        }
        match flow.link_user {
            Some(user) => self.link_identity(user, claims, flow.return_to).await,
            None => self.sign_in_external(claims, flow.return_to).await,
        }
    }

    fn trusted_email(claims: &ExternalClaims) -> Option<String> {
        claims
            .email
            .as_deref()
            .filter(|_| claims.email_verified)
            .and_then(|e| account::normalize_email(e).ok())
    }

    fn new_identity(&self, user: &UserId, claims: &ExternalClaims) -> Result<ExternalIdentity> {
        Ok(ExternalIdentity {
            id: token::random_hex(6)?,
            user: user.clone(),
            issuer: claims.issuer.clone(),
            subject: claims.subject.clone(),
            email: Self::trusted_email(claims),
            created_at: self.clock.now(),
            last_sign_in_at: None,
        })
    }

    async fn link_identity(
        &self,
        user: UserId,
        claims: ExternalClaims,
        return_to: Option<String>,
    ) -> Result<ExternalSignIn> {
        self.require_active(&user).await?;
        let existing = self
            .store
            .find_external_identity(&claims.issuer, &claims.subject)
            .await?;
        match existing {
            Some(found) if found.user == user => {}
            Some(_) => return Err(PynError::ExternalIdentityTaken),
            None => {
                let identity = self.new_identity(&user, &claims)?;
                let detail = format!("{user} linked {} subject {}", claims.issuer, claims.subject);
                if !self.store.link_external_identity(identity).await? {
                    return Err(PynError::ExternalIdentityTaken);
                }
                let action = AuditAction::ExternalIdentityLinked;
                self.record(AuditScope::Server, &user, action, detail)
                    .await?;
            }
        }
        Ok(ExternalSignIn::Linked { user, return_to })
    }

    async fn sign_in_external(
        &self,
        claims: ExternalClaims,
        return_to: Option<String>,
    ) -> Result<ExternalSignIn> {
        let found = self
            .store
            .find_external_identity(&claims.issuer, &claims.subject)
            .await?;
        let (user, created) = match found {
            Some(identity) => {
                self.require_active(&identity.user).await?;
                self.store
                    .touch_external_identity(
                        &identity.id,
                        Self::trusted_email(&claims).as_deref(),
                        self.clock.now(),
                    )
                    .await?;
                (identity.user, false)
            }
            None if self.config.external.create_accounts => {
                (self.create_external_account(&claims).await?, true)
            }
            None => return Err(PynError::ExternalNotLinked),
        };
        let (session, cookie) = self.open_session(user).await?;
        Ok(ExternalSignIn::SignedIn {
            session,
            cookie,
            return_to,
            created,
        })
    }

    /// Creates the account with a name from the provider's claim, never taking a reserved or existing name.
    async fn create_external_account(&self, claims: &ExternalClaims) -> Result<UserId> {
        let hint = claims
            .username_hint
            .as_deref()
            .or_else(|| claims.email.as_deref().and_then(|e| e.split('@').next()));
        let mut email = Self::trusted_email(claims);
        if let Some(address) = &email
            && self.store.verified_email_owner(address).await?.is_some()
        {
            email = None;
        }
        for name in username_candidates(hint)? {
            let Ok(user) = account::validate_username(&name) else {
                continue;
            };
            loop {
                let mut identity = self.new_identity(&user, claims)?;
                identity.last_sign_in_at = Some(identity.created_at);
                identity.email = email.clone();
                let new = NewExternalAccount {
                    user: user.clone(),
                    email: email.clone(),
                    identity,
                };
                match self.store.create_external_account(new).await? {
                    ExternalCreate::Created => {
                        let detail = format!(
                            "{user} created from {} subject {}",
                            claims.issuer, claims.subject
                        );
                        let action = AuditAction::ExternalAccountCreated;
                        self.record(AuditScope::Server, &user, action, detail)
                            .await?;
                        return Ok(user);
                    }
                    ExternalCreate::NameTaken => break,
                    ExternalCreate::EmailTaken => email = None,
                    ExternalCreate::IdentityTaken => {
                        // A concurrent first sign-in of the same person won.
                        return self
                            .store
                            .find_external_identity(&claims.issuer, &claims.subject)
                            .await?
                            .map(|i| i.user)
                            .ok_or(PynError::ExternalIdentityTaken);
                    }
                }
            }
        }
        Err(PynError::InvalidRequest(
            "no free user name could be derived from the provider's claims".into(),
        ))
    }

    /// The actor's linked provider identities.
    pub async fn external_identities(&self, actor: &Identity) -> Result<Vec<ExternalIdentity>> {
        self.store.list_external_identities(&actor.user).await
    }

    /// Removes one of the actor's links, unless it is their only way to sign in.
    pub async fn unlink_external_identity(&self, actor: &Identity, id: &str) -> Result<()> {
        let found = self
            .store
            .list_external_identities(&actor.user)
            .await?
            .into_iter()
            .find(|i| i.id == id);
        match self.store.unlink_external_identity(&actor.user, id).await? {
            IdentityUnlink::Removed => {}
            IdentityUnlink::NotFound => return Err(PynError::ExternalIdentityNotFound(id.into())),
            IdentityUnlink::LastCredential => return Err(PynError::LastSignInMethod),
        }
        let detail = found.map_or_else(
            || format!("{} unlinked {id}", actor.user),
            |i| format!("{} unlinked {} subject {}", actor.user, i.issuer, i.subject),
        );
        self.record(
            AuditScope::Server,
            &actor.user,
            AuditAction::ExternalIdentityUnlinked,
            detail,
        )
        .await
    }
}
