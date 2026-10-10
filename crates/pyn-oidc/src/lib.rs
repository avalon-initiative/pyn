//! An OpenID Connect client for `pyn_core::OidcProvider`: discovery, the authorization-code flow with PKCE, and
//! ID-token validation against the provider's published keys.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use jsonwebtoken::jwk::{AlgorithmParameters, Jwk, JwkSet};
use jsonwebtoken::{AlgorithmFamily, DecodingKey, Validation, decode, decode_header};
use pyn_core::{
    AuthorizationRequest, CodeExchange, ExternalClaims, OidcProvider, PynError, Result,
};
use reqwest::{StatusCode, Url};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::Mutex;

#[cfg(feature = "test-support")]
pub mod fake;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const DISCOVERY_TTL: Duration = Duration::from_secs(3600);
const JWKS_TTL: Duration = Duration::from_secs(3600);
/// How soon after a fetch an unknown key id may trigger another one.
const JWKS_REFETCH_AFTER: Duration = Duration::from_secs(30);
const MAX_SUBJECT: usize = 255;

/// What the operator configures.
#[derive(Debug, Clone)]
pub struct OidcConfig {
    pub issuer: String,
    pub client_id: String,
    pub client_secret: String,
    /// The provider's name on the sign-in button.
    pub display_name: String,
    /// The ID-token claim a new account's user name comes from.
    pub username_claim: String,
}

impl OidcConfig {
    pub fn new(issuer: &str, client_id: &str, client_secret: &str) -> Self {
        Self {
            issuer: issuer.into(),
            client_id: client_id.into(),
            client_secret: client_secret.into(),
            display_name: "Single sign-on".into(),
            username_claim: "preferred_username".into(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
struct Discovery {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    jwks_uri: String,
    #[serde(default)]
    token_endpoint_auth_methods_supported: Option<Vec<String>>,
    #[serde(default)]
    code_challenge_methods_supported: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct TokenResponse {
    id_token: Option<String>,
}

struct Cached<T> {
    value: T,
    fetched: Instant,
}

pub struct HttpOidcProvider {
    config: OidcConfig,
    http: reqwest::Client,
    discovery: Mutex<Option<Cached<Discovery>>>,
    keys: Mutex<Option<Cached<JwkSet>>>,
}

fn unavailable(what: &str, e: impl std::fmt::Display) -> PynError {
    PynError::ExternalProviderUnavailable(format!("{what}: {e}"))
}

fn failed(why: impl Into<String>) -> PynError {
    PynError::ExternalSignInFailed(why.into())
}

/// HTTPS, except plain HTTP to this machine for local development and tests.
fn require_secure(url: &str, what: &str) -> Result<Url> {
    let parsed = Url::parse(url).map_err(|_| failed(format!("{what} is not a URL")))?;
    let loopback = matches!(parsed.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    match parsed.scheme() {
        "https" => Ok(parsed),
        "http" if loopback => Ok(parsed),
        _ => Err(failed(format!("{what} must use https"))),
    }
}

impl HttpOidcProvider {
    /// Fails if the issuer is not an https URL (or plain http to this machine) or a setting is empty.
    pub fn new(config: OidcConfig) -> Result<Self> {
        let bad = |why: &str| PynError::InvalidRequest(why.to_string());
        if config.client_id.is_empty() || config.client_secret.is_empty() {
            return Err(bad("an OpenID Connect client id and secret are required"));
        }
        require_secure(&config.issuer, "the issuer")
            .map_err(|_| bad("the issuer must be an https URL"))?;
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| PynError::Storage(format!("http client: {e}")))?;
        Ok(Self {
            config,
            http,
            discovery: Mutex::new(None),
            keys: Mutex::new(None),
        })
    }

    async fn discovery(&self) -> Result<Discovery> {
        let mut cached = self.discovery.lock().await;
        if let Some(c) = cached
            .as_ref()
            .filter(|c| c.fetched.elapsed() < DISCOVERY_TTL)
        {
            return Ok(c.value.clone());
        }
        let url = format!(
            "{}/.well-known/openid-configuration",
            self.config.issuer.trim_end_matches('/')
        );
        let doc: Discovery = self.get_json(&url, "provider discovery").await?;
        if doc.issuer != self.config.issuer {
            return Err(failed(
                "the provider's issuer does not match the configured one",
            ));
        }
        for (url, what) in [
            (&doc.authorization_endpoint, "the authorization endpoint"),
            (&doc.token_endpoint, "the token endpoint"),
            (&doc.jwks_uri, "the key set URL"),
        ] {
            require_secure(url, what)?;
        }
        if let Some(methods) = &doc.code_challenge_methods_supported
            && !methods.iter().any(|m| m == "S256")
        {
            return Err(failed("the provider does not support PKCE with S256"));
        }
        *cached = Some(Cached {
            value: doc.clone(),
            fetched: Instant::now(),
        });
        Ok(doc)
    }

    async fn get_json<T: for<'de> Deserialize<'de>>(&self, url: &str, what: &str) -> Result<T> {
        let response = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|e| unavailable(what, e))?;
        if !response.status().is_success() {
            return Err(unavailable(what, response.status()));
        }
        response.json().await.map_err(|e| unavailable(what, e))
    }

    /// The provider's key for `kid`, refetching the set when it is stale or does not know the key yet.
    async fn key_for(&self, jwks_uri: &str, kid: Option<&str>) -> Result<Jwk> {
        let mut cached = self.keys.lock().await;
        let find = |set: &JwkSet| match kid {
            Some(kid) => set.find(kid).cloned(),
            None if set.keys.len() == 1 => set.keys.first().cloned(),
            None => None,
        };
        if let Some(c) = cached.as_ref().filter(|c| c.fetched.elapsed() < JWKS_TTL)
            && let Some(key) = find(&c.value)
        {
            return Ok(key);
        }
        let recent = cached
            .as_ref()
            .is_some_and(|c| c.fetched.elapsed() < JWKS_REFETCH_AFTER);
        if recent {
            return Err(failed("the ID token is signed with an unknown key"));
        }
        let set: JwkSet = self.get_json(jwks_uri, "provider keys").await?;
        let key = find(&set);
        *cached = Some(Cached {
            value: set,
            fetched: Instant::now(),
        });
        key.ok_or_else(|| failed("the ID token is signed with an unknown key"))
    }

    async fn validate_id_token(
        &self,
        discovery: &Discovery,
        id_token: &str,
        nonce: &str,
    ) -> Result<ExternalClaims> {
        let header = decode_header(id_token).map_err(|_| failed("the ID token is malformed"))?;
        let jwk = self
            .key_for(&discovery.jwks_uri, header.kid.as_deref())
            .await?;
        let family = match jwk.algorithm {
            AlgorithmParameters::RSA(_) => AlgorithmFamily::Rsa,
            AlgorithmParameters::EllipticCurve(_) => AlgorithmFamily::Ec,
            AlgorithmParameters::OctetKeyPair(_) => AlgorithmFamily::Ed,
            _ => return Err(failed("the provider's key cannot verify a signature")),
        };
        if let Some(declared) = jwk.common.key_algorithm
            && serde_json::to_value(declared).ok() != serde_json::to_value(header.alg).ok()
        {
            return Err(failed("the ID token's algorithm does not match its key"));
        }
        let key =
            DecodingKey::from_jwk(&jwk).map_err(|_| failed("the provider's key is unusable"))?;
        let mut validation = Validation::new_for_family(family);
        validation.set_audience(&[&self.config.client_id]);
        validation.set_issuer(&[&self.config.issuer]);
        validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
        validation.validate_nbf = true;
        let data = decode::<Value>(id_token, &key, &validation)
            .map_err(|e| failed(format!("the ID token is not valid: {:?}", e.kind())))?;
        let claims = data.claims;

        let text = |name: &str| claims.get(name).and_then(Value::as_str);
        if claims.get("nonce").and_then(Value::as_str) != Some(nonce) {
            return Err(failed("the ID token does not answer this sign-in"));
        }
        let several_audiences = claims.get("aud").is_some_and(Value::is_array)
            && claims["aud"].as_array().is_some_and(|a| a.len() > 1);
        if several_audiences && text("azp") != Some(self.config.client_id.as_str()) {
            return Err(failed("the ID token was issued to another client"));
        }
        let subject = text("sub")
            .filter(|s| !s.is_empty() && s.len() <= MAX_SUBJECT)
            .ok_or_else(|| failed("the ID token has no usable subject"))?;
        let email_verified = match claims.get("email_verified") {
            Some(Value::Bool(b)) => *b,
            Some(Value::String(s)) => s == "true",
            _ => false,
        };
        Ok(ExternalClaims {
            issuer: self.config.issuer.clone(),
            subject: subject.to_string(),
            email: text("email").map(str::to_string),
            email_verified,
            username_hint: text(&self.config.username_claim).map(str::to_string),
        })
    }
}

#[async_trait]
impl OidcProvider for HttpOidcProvider {
    fn display_name(&self) -> &str {
        &self.config.display_name
    }

    async fn authorization_url(&self, r: &AuthorizationRequest<'_>) -> Result<String> {
        let discovery = self.discovery().await?;
        let mut url = require_secure(
            &discovery.authorization_endpoint,
            "the authorization endpoint",
        )?;
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &self.config.client_id)
            .append_pair("redirect_uri", r.redirect_uri)
            .append_pair("scope", "openid profile email")
            .append_pair("state", r.state)
            .append_pair("nonce", r.nonce)
            .append_pair("code_challenge", r.code_challenge)
            .append_pair("code_challenge_method", "S256");
        Ok(url.into())
    }

    async fn exchange(&self, e: &CodeExchange<'_>) -> Result<ExternalClaims> {
        let discovery = self.discovery().await?;
        let use_basic = discovery
            .token_endpoint_auth_methods_supported
            .as_ref()
            .is_none_or(|m| m.iter().any(|m| m == "client_secret_basic"));
        let mut form = vec![
            ("grant_type", "authorization_code"),
            ("code", e.code),
            ("redirect_uri", e.redirect_uri),
            ("code_verifier", e.code_verifier),
        ];
        let mut request = self.http.post(&discovery.token_endpoint);
        if use_basic {
            request = request.basic_auth(&self.config.client_id, Some(&self.config.client_secret));
        } else {
            form.push(("client_id", &self.config.client_id));
            form.push(("client_secret", &self.config.client_secret));
        }
        let response = request
            .form(&form)
            .send()
            .await
            .map_err(|err| unavailable("token endpoint", err))?;
        let status = response.status();
        if status.is_server_error() {
            return Err(unavailable("token endpoint", status));
        }
        if status != StatusCode::OK {
            let reason = response
                .json::<Value>()
                .await
                .ok()
                .and_then(|v| v.get("error").and_then(Value::as_str).map(str::to_string))
                .unwrap_or_else(|| status.to_string());
            let reason: String = reason
                .chars()
                .filter(|c| c.is_ascii_graphic())
                .take(60)
                .collect();
            return Err(failed(format!("the provider refused the code ({reason})")));
        }
        let tokens: TokenResponse = response
            .json()
            .await
            .map_err(|err| unavailable("token endpoint", err))?;
        let id_token = tokens
            .id_token
            .ok_or_else(|| failed("the provider returned no ID token"))?;
        self.validate_id_token(&discovery, &id_token, e.nonce).await
    }
}
