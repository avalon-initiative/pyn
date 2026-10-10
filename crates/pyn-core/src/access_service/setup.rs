//! First-run setup: a fresh server serves only the setup routes until its first administrator exists.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use super::*;

const SETUP_WINDOW: Duration = Duration::minutes(15);
const FAILURES_PER_CLIENT: u32 = 10;
const FAILURES_PER_SERVER: u32 = 50;
const SERVER_KEY: &str = "setup:server";
const MIN_TOKEN_LENGTH: usize = 16;
const MAX_SERVER_NAME: usize = 100;
const MAX_PUBLIC_URL: usize = 2048;

/// The in-process half of setup: the token's hash until setup completes, and what it recorded.
#[derive(Default)]
pub(super) struct SetupState {
    enabled: bool,
    initialised: AtomicBool,
    token_hash: Mutex<Option<String>>,
    settings: Mutex<Option<ServerSettings>>,
}

impl SetupState {
    pub(super) fn settings(&self) -> Option<ServerSettings> {
        self.settings.lock().unwrap().clone()
    }

    fn finish(&self, settings: ServerSettings) {
        *self.settings.lock().unwrap() = Some(settings);
        *self.token_hash.lock().unwrap() = None;
        self.initialised.store(true, Ordering::Release);
    }
}

/// A random one-time setup token.
pub fn generate_setup_token() -> Result<String> {
    token::random_hex(24)
}

/// What anyone may learn about a server before and after setup; no secrets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupStatus {
    pub initialised: bool,
    pub server_name: Option<String>,
    /// What setup recorded, or the configured default while the server is uninitialised.
    pub public_url: String,
    pub registration: RegistrationMode,
}

/// The first administrator and the server's essentials, with the token that authorises them.
#[derive(Debug, Clone, Copy)]
pub struct SetupRequest<'a> {
    pub token: &'a str,
    pub username: &'a str,
    pub password: &'a str,
    pub email: Option<&'a str>,
    pub server_name: Option<&'a str>,
    pub public_url: Option<&'a str>,
    pub registration: RegistrationMode,
    /// The caller's network address, for rate limits.
    pub client: Option<&'a str>,
}

fn clean_server_name(name: Option<&str>) -> Result<Option<String>> {
    let Some(name) = name.map(str::trim).filter(|n| !n.is_empty()) else {
        return Ok(None);
    };
    if name.chars().count() > MAX_SERVER_NAME || name.chars().any(char::is_control) {
        return Err(PynError::InvalidRequest(format!(
            "the server name is at most {MAX_SERVER_NAME} characters with no control characters"
        )));
    }
    Ok(Some(name.to_string()))
}

fn clean_public_url(url: Option<&str>) -> Result<Option<String>> {
    let Some(url) = url.map(str::trim).filter(|u| !u.is_empty()) else {
        return Ok(None);
    };
    let host = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .and_then(|rest| rest.split(['/', '?', '#']).next())
        .filter(|h| !h.is_empty());
    if host.is_none()
        || url.len() > MAX_PUBLIC_URL
        || url.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(PynError::InvalidRequest(
            "the public address must be an http:// or https:// URL".into(),
        ));
    }
    Ok(Some(url.trim_end_matches('/').to_string()))
}

impl AccessService {
    /// Starts the server uninitialised until setup runs with `token`. Without this call the server counts as
    /// initialised, which is what tests and tools that create their own accounts want.
    pub fn with_setup_token(mut self, token: &str) -> Result<Self> {
        if token.len() < MIN_TOKEN_LENGTH || token.chars().any(char::is_whitespace) {
            return Err(PynError::InvalidRequest(format!(
                "a setup token is at least {MIN_TOKEN_LENGTH} characters with no whitespace"
            )));
        }
        self.setup.enabled = true;
        *self.setup.token_hash.lock().unwrap() = Some(token::hash_secret(token));
        Ok(self)
    }

    /// Reads what setup recorded and applies it. Returns whether the server is initialised.
    pub async fn load_setup(&self) -> Result<bool> {
        if !self.setup.enabled || self.setup.initialised.load(Ordering::Acquire) {
            return Ok(true);
        }
        match self.store.server_settings().await? {
            Some(settings) => {
                self.setup.finish(settings);
                Ok(true)
            }
            None => Ok(false),
        }
    }

    pub async fn is_initialised(&self) -> Result<bool> {
        self.load_setup().await
    }

    /// Whether a setup token is still waiting to be used in this process.
    pub fn setup_token_pending(&self) -> bool {
        self.setup.token_hash.lock().unwrap().is_some()
    }

    pub async fn setup_status(&self) -> Result<SetupStatus> {
        let initialised = self.is_initialised().await?;
        let settings = self.setup.settings();
        Ok(SetupStatus {
            initialised,
            server_name: settings.and_then(|s| s.server_name),
            public_url: self.public_url(),
            registration: self.registration_mode(),
        })
    }

    /// Creates the first administrator and records the server's essentials. Runs once: later calls get
    /// `AlreadyInitialised`. Wrong tokens count against a per-client and a server-wide limit.
    pub async fn complete_setup(&self, request: SetupRequest<'_>) -> Result<UserId> {
        if self.is_initialised().await? {
            return Err(PynError::AlreadyInitialised);
        }
        let client_key = request.client.map(|c| format!("setup:client:{c}"));
        self.gate(SERVER_KEY, FAILURES_PER_SERVER).await?;
        if let Some(key) = &client_key {
            self.gate(key, FAILURES_PER_CLIENT).await?;
        }
        let expected = self.setup.token_hash.lock().unwrap().clone();
        let presented = token::hash_secret(request.token);
        if !expected.is_some_and(|hash| token::hashes_match(&hash, &presented)) {
            let now = self.clock.now();
            self.limits.hit(SERVER_KEY, SETUP_WINDOW, now).await?;
            if let Some(key) = &client_key {
                self.limits.hit(key, SETUP_WINDOW, now).await?;
            }
            return Err(PynError::InvalidSetupToken);
        }

        let user = account::validate_username(request.username)?;
        account::validate_password(request.password)?;
        let email = request.email.map(account::normalize_email).transpose()?;
        let settings = ServerSettings {
            initialised_at: self.clock.now(),
            server_name: clean_server_name(request.server_name)?,
            public_url: clean_public_url(request.public_url)?,
            registration: Some(request.registration),
        };
        let password_hash = self.passwords.hash(request.password).await?;
        let admin = NewAccount {
            user: user.clone(),
            email,
            password_hash,
            signup: SignupStage::Complete,
            created_at: settings.initialised_at,
        };
        if !self.store.complete_setup(admin, settings.clone()).await? {
            return Err(PynError::AlreadyInitialised);
        }
        self.setup.finish(settings);
        let detail = format!(
            "server set up; {user} is the first administrator; registration {}",
            request.registration.as_str()
        );
        self.record_server(&user, AuditAction::ServerSetupCompleted, detail)
            .await?;
        Ok(user)
    }
}
