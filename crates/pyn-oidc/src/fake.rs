//! An identity provider that runs inside the test process on a loopback port: discovery, keys, and a token endpoint
//! that checks PKCE and client authentication. `authorize` plays the browser's visit to the provider.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use aws_lc_rs::encoding::AsDer;
use aws_lc_rs::rsa::{KeyPair as RsaKeyPair, KeySize, PublicKeyComponents};
use aws_lc_rs::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde_json::{Value, json};

pub const CLIENT_ID: &str = "pyn-test-client";
pub const CLIENT_SECRET: &str = "pyn-test-secret";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyKind {
    Rsa,
    Ec,
}

/// What the provider does wrong on purpose when it issues the next ID token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Fault {
    #[default]
    None,
    WrongNonce,
    NoNonce,
    WrongAudience,
    WrongIssuer,
    Expired,
    SignedByStranger,
    AlgNone,
    NoSubject,
    NoIdToken,
    /// Two audiences, one of them this client, and no `azp`.
    MultipleAudiences,
}

/// Who signs in at the provider and what it says about them.
#[derive(Debug, Clone)]
pub struct Person {
    pub subject: String,
    pub email: Option<String>,
    pub email_verified: Value,
    pub username: Option<String>,
}

impl Person {
    pub fn new(subject: &str) -> Self {
        Self {
            subject: subject.into(),
            email: None,
            email_verified: Value::Null,
            username: None,
        }
    }

    pub fn username(mut self, name: &str) -> Self {
        self.username = Some(name.into());
        self
    }

    pub fn email(mut self, email: &str, verified: bool) -> Self {
        self.email = Some(email.into());
        self.email_verified = Value::Bool(verified);
        self
    }
}

fn pem(pkcs8: &[u8]) -> String {
    let body = STANDARD.encode(pkcs8);
    let lines: Vec<&str> = body
        .as_bytes()
        .chunks(64)
        .map(|c| std::str::from_utf8(c).unwrap())
        .collect();
    format!(
        "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
        lines.join("\n")
    )
}

struct Signer {
    kind: KeyKind,
    kid: String,
    encoding: EncodingKey,
    jwk: Value,
}

impl Signer {
    fn new(kind: KeyKind, kid: &str) -> Self {
        match kind {
            KeyKind::Rsa => {
                let pair = RsaKeyPair::generate(KeySize::Rsa2048).unwrap();
                let der = AsDer::<aws_lc_rs::encoding::Pkcs8V1Der>::as_der(&pair).unwrap();
                let parts = PublicKeyComponents::<Vec<u8>>::from(pair.public_key());
                let jwk = json!({
                    "kty": "RSA", "use": "sig", "alg": "RS256", "kid": kid,
                    "n": URL_SAFE_NO_PAD.encode(parts.n), "e": URL_SAFE_NO_PAD.encode(parts.e),
                });
                Self {
                    kind,
                    kid: kid.into(),
                    encoding: EncodingKey::from_rsa_pem(pem(der.as_ref()).as_bytes()).unwrap(),
                    jwk,
                }
            }
            KeyKind::Ec => {
                let pkcs8 = EcdsaKeyPair::generate_pkcs8(
                    &ECDSA_P256_SHA256_FIXED_SIGNING,
                    &aws_lc_rs::rand::SystemRandom::new(),
                )
                .unwrap();
                let pair =
                    EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref())
                        .unwrap();
                let point = pair.public_key().as_ref().to_vec();
                let jwk = json!({
                    "kty": "EC", "use": "sig", "alg": "ES256", "kid": kid, "crv": "P-256",
                    "x": URL_SAFE_NO_PAD.encode(&point[1..33]),
                    "y": URL_SAFE_NO_PAD.encode(&point[33..65]),
                });
                Self {
                    kind,
                    kid: kid.into(),
                    encoding: EncodingKey::from_ec_der(pkcs8.as_ref()),
                    jwk,
                }
            }
        }
    }

    fn algorithm(&self) -> Algorithm {
        match self.kind {
            KeyKind::Rsa => Algorithm::RS256,
            KeyKind::Ec => Algorithm::ES256,
        }
    }

    fn sign(&self, claims: &Value) -> String {
        let mut header = Header::new(self.algorithm());
        header.kid = Some(self.kid.clone());
        jsonwebtoken::encode(&header, claims, &self.encoding).unwrap()
    }
}

struct Grant {
    nonce: String,
    challenge: String,
    redirect_uri: String,
    person: Person,
}

struct Shared {
    issuer: String,
    signer: Signer,
    stranger: Signer,
    grants: Mutex<HashMap<String, Grant>>,
    fault: Mutex<Fault>,
    token_status: Mutex<Option<u16>>,
    token_calls: Mutex<u32>,
    next_code: Mutex<u32>,
}

pub struct FakeIdp {
    shared: Arc<Shared>,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for FakeIdp {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl FakeIdp {
    pub async fn start() -> Self {
        Self::start_with(KeyKind::Rsa).await
    }

    pub async fn start_with(kind: KeyKind) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let issuer = format!("http://{}", listener.local_addr().unwrap());
        let shared = Arc::new(Shared {
            issuer,
            signer: Signer::new(kind, "key-1"),
            stranger: Signer::new(kind, "key-1"),
            grants: Mutex::default(),
            fault: Mutex::default(),
            token_status: Mutex::default(),
            token_calls: Mutex::default(),
            next_code: Mutex::default(),
        });
        let app = Router::new()
            .route("/.well-known/openid-configuration", get(discovery))
            .route("/jwks", get(jwks))
            .route("/token", post(token))
            .with_state(shared.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self { shared, server }
    }

    pub fn issuer(&self) -> &str {
        &self.shared.issuer
    }

    /// Makes the next ID token wrong in one way.
    pub fn misbehave(&self, fault: Fault) {
        *self.shared.fault.lock().unwrap() = fault;
    }

    /// Makes the token endpoint answer with this status and an `invalid_grant` error.
    pub fn reject_tokens_with(&self, status: u16) {
        *self.shared.token_status.lock().unwrap() = Some(status);
    }

    pub fn token_requests(&self) -> u32 {
        *self.shared.token_calls.lock().unwrap()
    }

    /// Plays the browser at the provider: checks the authorization request and returns the URL it would be sent
    /// back to, carrying a fresh code and the request's state.
    pub fn authorize(&self, authorization_url: &str, person: &Person) -> Result<String, String> {
        let url = reqwest::Url::parse(authorization_url).map_err(|e| e.to_string())?;
        let q: HashMap<String, String> = url.query_pairs().into_owned().collect();
        let get = |k: &str| q.get(k).cloned().ok_or(format!("missing {k}"));
        if !authorization_url.starts_with(&format!("{}/authorize", self.shared.issuer)) {
            return Err("not this provider's authorization endpoint".into());
        }
        let expect = |k: &str, v: &str| (get(k)? == v).then_some(()).ok_or(format!("bad {k}"));
        expect("response_type", "code")?;
        expect("client_id", CLIENT_ID)?;
        expect("code_challenge_method", "S256")?;
        if !get("scope")?.split(' ').any(|s| s == "openid") {
            return Err("scope lacks openid".into());
        }
        let mut next = self.shared.next_code.lock().unwrap();
        *next += 1;
        let code = format!("code-{next}");
        self.shared.grants.lock().unwrap().insert(
            code.clone(),
            Grant {
                nonce: get("nonce")?,
                challenge: get("code_challenge")?,
                redirect_uri: get("redirect_uri")?,
                person: person.clone(),
            },
        );
        let mut back = reqwest::Url::parse(&get("redirect_uri")?).map_err(|e| e.to_string())?;
        back.query_pairs_mut()
            .append_pair("code", &code)
            .append_pair("state", &get("state")?);
        Ok(back.into())
    }
}

async fn discovery(State(s): State<Arc<Shared>>) -> Json<Value> {
    Json(json!({
        "issuer": s.issuer,
        "authorization_endpoint": format!("{}/authorize", s.issuer),
        "token_endpoint": format!("{}/token", s.issuer),
        "jwks_uri": format!("{}/jwks", s.issuer),
        "response_types_supported": ["code"],
        "id_token_signing_alg_values_supported": ["RS256", "ES256"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["client_secret_basic", "client_secret_post"],
    }))
}

async fn jwks(State(s): State<Arc<Shared>>) -> Json<Value> {
    Json(json!({ "keys": [s.signer.jwk] }))
}

fn client_authenticated(headers: &HeaderMap) -> bool {
    let want = STANDARD.encode(format!("{CLIENT_ID}:{CLIENT_SECRET}"));
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == format!("Basic {want}"))
}

fn denied(status: u16, error: &str) -> axum::response::Response {
    (
        StatusCode::from_u16(status).unwrap(),
        Json(json!({ "error": error })),
    )
        .into_response()
}

async fn token(
    State(s): State<Arc<Shared>>,
    headers: HeaderMap,
    Form(form): Form<HashMap<String, String>>,
) -> axum::response::Response {
    *s.token_calls.lock().unwrap() += 1;
    if let Some(status) = *s.token_status.lock().unwrap() {
        return denied(status, "invalid_grant");
    }
    if !client_authenticated(&headers) {
        return denied(401, "invalid_client");
    }
    let field = |k: &str| form.get(k).map(String::as_str).unwrap_or_default();
    let Some(grant) = s.grants.lock().unwrap().remove(field("code")) else {
        return denied(400, "invalid_grant");
    };
    let verifier_ok = {
        use sha2::Digest;
        let digest = sha2::Sha256::digest(field("code_verifier").as_bytes());
        URL_SAFE_NO_PAD.encode(digest) == grant.challenge
    };
    if field("grant_type") != "authorization_code"
        || field("redirect_uri") != grant.redirect_uri
        || !verifier_ok
    {
        return denied(400, "invalid_grant");
    }
    let fault = *s.fault.lock().unwrap();
    if fault == Fault::NoIdToken {
        return Json(json!({ "access_token": "x", "token_type": "Bearer" })).into_response();
    }
    let id_token = issue(&s, &grant, fault);
    Json(json!({ "access_token": "x", "token_type": "Bearer", "id_token": id_token }))
        .into_response()
}

fn issue(s: &Shared, grant: &Grant, fault: Fault) -> String {
    let now = jsonwebtoken::get_current_timestamp();
    let person = &grant.person;
    let mut claims = json!({
        "iss": s.issuer,
        "sub": person.subject,
        "aud": CLIENT_ID,
        "iat": now,
        "exp": now + 300,
        "nonce": grant.nonce,
    });
    let set = |claims: &mut Value, k: &str, v: Value| claims[k] = v;
    if let Some(email) = &person.email {
        set(&mut claims, "email", json!(email));
    }
    if !person.email_verified.is_null() {
        set(&mut claims, "email_verified", person.email_verified.clone());
    }
    if let Some(name) = &person.username {
        set(&mut claims, "preferred_username", json!(name));
    }
    match fault {
        Fault::WrongNonce => set(&mut claims, "nonce", json!("not-the-nonce")),
        Fault::NoNonce => {
            claims.as_object_mut().unwrap().remove("nonce");
        }
        Fault::WrongAudience => set(&mut claims, "aud", json!("another-client")),
        Fault::WrongIssuer => set(&mut claims, "iss", json!("https://evil.example")),
        Fault::Expired => set(&mut claims, "exp", json!(now - 3600)),
        Fault::NoSubject => {
            claims.as_object_mut().unwrap().remove("sub");
        }
        Fault::MultipleAudiences => set(&mut claims, "aud", json!([CLIENT_ID, "another-client"])),
        _ => {}
    }
    match fault {
        Fault::SignedByStranger => s.stranger.sign(&claims),
        Fault::AlgNone => {
            let part = |v: &Value| URL_SAFE_NO_PAD.encode(serde_json::to_vec(v).unwrap());
            format!(
                "{}.{}.",
                part(&json!({"alg": "none", "kid": s.signer.kid})),
                part(&claims)
            )
        }
        _ => s.signer.sign(&claims),
    }
}
