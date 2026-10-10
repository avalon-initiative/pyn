//! External sign-in with an OpenID Connect provider. The protocol client sits behind `OidcProvider`; this module
//! holds what the core needs to run the flow and link accounts.

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};

use crate::access::token;
use crate::error::{PynError, Result};
use crate::types::UserId;

/// What the provider proved about a person after the ID token was validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalClaims {
    pub issuer: String,
    /// Stable per issuer; the only thing an account is linked by.
    pub subject: String,
    pub email: Option<String>,
    /// Whether the provider vouches for `email`.
    pub email_verified: bool,
    /// The configured claim to derive a user name from.
    pub username_hint: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct AuthorizationRequest<'a> {
    pub state: &'a str,
    pub nonce: &'a str,
    pub code_challenge: &'a str,
    pub redirect_uri: &'a str,
}

#[derive(Debug, Clone, Copy)]
pub struct CodeExchange<'a> {
    pub code: &'a str,
    pub code_verifier: &'a str,
    pub redirect_uri: &'a str,
    /// The ID token must carry exactly this nonce.
    pub nonce: &'a str,
}

/// An OpenID Connect client for one configured provider.
#[async_trait]
pub trait OidcProvider: Send + Sync {
    /// What the sign-in button calls the provider.
    fn display_name(&self) -> &str;

    /// The provider URL to send the browser to.
    async fn authorization_url(&self, request: &AuthorizationRequest<'_>) -> Result<String>;

    /// Trades the code for tokens and returns the claims of the validated ID token.
    async fn exchange(&self, exchange: &CodeExchange<'_>) -> Result<ExternalClaims>;
}

/// An account's link to a provider subject.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalIdentity {
    pub id: String,
    pub user: UserId,
    pub issuer: String,
    pub subject: String,
    /// The address the provider gave at the last sign-in, for display only.
    pub email: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_sign_in_at: Option<DateTime<Utc>>,
}

/// A sign-in in progress. Only hashes of the state and the browser binding are kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OidcFlow {
    pub state_hash: String,
    pub binding_hash: String,
    pub nonce: String,
    pub code_verifier: String,
    /// Set when a signed-in person is linking the provider to their account rather than signing in.
    pub link_user: Option<UserId>,
    /// A path in the web app to return to.
    pub return_to: Option<String>,
    pub expires_at: DateTime<Utc>,
}

/// An account created at first external sign-in, with its link to the provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewExternalAccount {
    pub user: UserId,
    /// A provider-verified address; stored as verified.
    pub email: Option<String>,
    pub identity: ExternalIdentity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalCreate {
    Created,
    NameTaken,
    IdentityTaken,
    /// Another account has already verified the address.
    EmailTaken,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityUnlink {
    Removed,
    NotFound,
    /// The account has no password and this is its only link, so removing it would lock the person out.
    LastCredential,
}

/// The values of one authorization request: random, and the PKCE challenge derived from the verifier.
pub(crate) struct FlowSecrets {
    pub state: String,
    pub binding: String,
    pub nonce: String,
    pub code_verifier: String,
    pub code_challenge: String,
}

impl FlowSecrets {
    pub(crate) fn generate() -> Result<Self> {
        let code_verifier = token::random_hex(32)?;
        Ok(Self {
            state: token::random_hex(32)?,
            binding: token::random_hex(32)?,
            nonce: token::random_hex(16)?,
            code_challenge: pkce_challenge(&code_verifier),
            code_verifier,
        })
    }
}

/// The S256 code challenge for a verifier.
pub fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// A path inside the web app: absolute, not protocol-relative, no control characters.
pub(crate) fn validate_return_to(path: &str) -> Result<()> {
    let ok = path.starts_with('/')
        && !path.starts_with("//")
        && path.len() <= 512
        && !path.contains('\\')
        && !path.chars().any(char::is_control);
    if ok {
        Ok(())
    } else {
        Err(PynError::InvalidRequest(
            "return_to must be a path in the web app, such as /repos".into(),
        ))
    }
}

/// Candidate user names for a person: the cleaned hint, then numbered and random variants. Reserved and
/// malformed ones are left for the caller to skip.
pub(crate) fn username_candidates(hint: Option<&str>) -> Result<Vec<String>> {
    const ROOM: usize = 7;
    let mut base: String = hint
        .unwrap_or_default()
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    base = base
        .trim_start_matches(|c: char| !c.is_ascii_alphanumeric())
        .chars()
        .take(39 - ROOM)
        .collect();
    if base.len() < 2 {
        base = "member".into();
    }
    let mut out = vec![base.clone()];
    out.extend((2..=5).map(|n| format!("{base}-{n}")));
    for _ in 0..4 {
        out.push(format!("{base}-{}", token::random_hex(3)?));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn challenge_matches_the_rfc_example() {
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn return_to_must_stay_in_the_app() {
        assert!(validate_return_to("/repos/acme").is_ok());
        for bad in [
            "https://evil.example",
            "//evil.example",
            "repos",
            "/a\\b",
            "/a\nb",
        ] {
            assert!(validate_return_to(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn candidates_are_clean_and_fit_the_name_rules() {
        let c = username_candidates(Some("Åsa.Nilsson@Example")).unwrap();
        assert_eq!(c[0], "sa-nilsson-example");
        assert!(c.iter().all(|n| n.len() <= 39));
        assert_eq!(username_candidates(None).unwrap()[0], "member");
        assert_eq!(username_candidates(Some("x")).unwrap()[0], "member");
    }
}
