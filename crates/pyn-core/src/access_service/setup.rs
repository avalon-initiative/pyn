//! First-run setup: a fresh server serves only the setup routes until its first administrator exists.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use super::*;

const SETTINGS_TTL: Duration = Duration::seconds(60);
const SETUP_WINDOW: Duration = Duration::minutes(15);
const FAILURES_PER_CLIENT: u32 = 10;
const FAILURES_PER_SERVER: u32 = 1000;
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
    read_at: Mutex<Option<DateTime<Utc>>>,
    /// Set by an administrator on a server that never ran setup (nothing to store it in).
    unstored_access: Mutex<Option<AccessMode>>,
    unstored_two_factor: Mutex<Option<TwoFactorPolicy>>,
}

impl SetupState {
    pub(super) fn settings(&self) -> Option<ServerSettings> {
        self.settings.lock().unwrap().clone()
    }

    fn set_access(&self, mode: AccessMode) {
        if let Some(settings) = self.settings.lock().unwrap().as_mut() {
            settings.access_mode = mode;
        }
    }

    fn set_two_factor(&self, policy: Option<TwoFactorPolicy>) -> bool {
        match self.settings.lock().unwrap().as_mut() {
            Some(settings) => {
                settings.two_factor = policy;
                true
            }
            None => false,
        }
    }

    fn finish(&self, settings: ServerSettings, now: DateTime<Utc>) {
        *self.settings.lock().unwrap() = Some(settings);
        *self.read_at.lock().unwrap() = Some(now);
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
    pub access_mode: AccessMode,
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
    /// `None` takes the configured default.
    pub access_mode: Option<AccessMode>,
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
        if !self.setup.enabled {
            return Ok(true);
        }
        // Settings changed through another server copy are picked up after the TTL.
        let now = self.clock.now();
        let fresh = self
            .setup
            .read_at
            .lock()
            .unwrap()
            .is_some_and(|at| now - at < SETTINGS_TTL);
        if self.setup.initialised.load(Ordering::Acquire) && fresh {
            return Ok(true);
        }
        match self.store.server_settings().await? {
            Some(settings) => {
                self.setup.finish(settings, now);
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
            access_mode: self.access_mode(),
        })
    }

    /// Creates the first administrator and records the server's essentials. Runs once: later calls get
    /// `AlreadyInitialised`. Attempts count against a per-client limit and a much higher server-wide ceiling and a right token gives its count back.
    pub async fn complete_setup(&self, request: SetupRequest<'_>) -> Result<UserId> {
        if self.is_initialised().await? {
            return Err(PynError::AlreadyInitialised);
        }
        let client_key = request
            .client
            .map(|c| format!("setup:client:{}", bounded_key(c)));
        // Client first: a refused client must not spend the shared ceiling.
        if let Some(key) = &client_key {
            self.count_limited(key, FAILURES_PER_CLIENT, SETUP_WINDOW)
                .await?;
        }
        self.count_limited(SERVER_KEY, FAILURES_PER_SERVER, SETUP_WINDOW)
            .await?;
        let expected = self.setup.token_hash.lock().unwrap().clone();
        let presented = token::hash_secret(request.token);
        if !expected.is_some_and(|hash| token::hashes_match(&hash, &presented)) {
            return Err(PynError::InvalidSetupToken);
        }
        let now = self.clock.now();
        self.limits.refund(SERVER_KEY, now).await?;
        if let Some(key) = &client_key {
            self.limits.refund(key, now).await?;
        }

        let user = account::validate_username(request.username)?;
        account::validate_password(request.password)?;
        let email = request.email.map(account::normalize_email).transpose()?;
        let settings = ServerSettings {
            initialised_at: self.clock.now(),
            server_name: clean_server_name(request.server_name)?,
            public_url: clean_public_url(request.public_url)?,
            registration: Some(request.registration),
            access_mode: request.access_mode.unwrap_or(self.config.access_mode),
            two_factor: None,
        };
        let password_hash = self.passwords.hash(request.password).await?;
        // With a real mailer and verification on, the address is added unverified and confirmed by link.
        let confirm_by_link = self.config.require_email_verification
            && self.email.kind() != MailerKind::None
            && email.is_some();
        let admin = NewAccount {
            id: AccountId::generate(AccountKind::User)?,
            user: user.clone(),
            email: if confirm_by_link { None } else { email.clone() },
            password_hash,
            signup: SignupStage::Complete,
            created_at: settings.initialised_at,
        };
        let created = self.draft(DomainEvent::AccountCreated {
            user: user.clone(),
            user_id: admin.id.clone(),
            pending_approval: false,
        })?;
        if !self
            .store
            .complete_setup_with_events(admin, settings.clone(), &[created])
            .await?
        {
            return Err(PynError::AlreadyInitialised);
        }
        let access = settings.access_mode;
        self.setup.finish(settings, self.clock.now());
        let detail = format!(
            "server set up; {user} is the first administrator; registration {}; access {}",
            request.registration.as_str(),
            access.as_str()
        );
        self.record_server((&user).into(), AuditAction::ServerSetupCompleted, detail)
            .await?;
        if let Some(address) = email {
            self.mail_first_administrator(&user, &address, confirm_by_link)
                .await;
        }
        Ok(user)
    }
}

impl AccessService {
    /// Welcome and, when verification is on, the confirmation link. The account exists by now, so a failure is
    /// logged and never undoes setup.
    async fn mail_first_administrator(&self, user: &UserId, address: &str, confirm_by_link: bool) {
        self.send_welcome(address, user, false, false).await;
        if !confirm_by_link {
            return;
        }
        let added = self.store.add_email(user, address, self.clock.now()).await;
        let sent = match added {
            Ok(_) => self.mail_address_check(user, address, true).await,
            Err(e) => Err(e),
        };
        if let Err(e) = sent {
            tracing::warn!(error = %e, "the first administrator's confirmation email could not be prepared");
        }
    }

    /// The stored mode, else the configured default; it follows a write at once and a change made through another
    /// server copy within the settings TTL.
    pub fn access_mode(&self) -> AccessMode {
        if let Some(settings) = self.setup.settings() {
            return settings.access_mode;
        }
        (*self.setup.unstored_access.lock().unwrap()).unwrap_or(self.config.access_mode)
    }

    /// Changes the access mode, recording it in the server audit and log. Server administrators only; setting
    /// the current mode changes nothing.
    pub async fn set_access_mode(&self, actor: &Identity, mode: AccessMode) -> Result<AccessMode> {
        self.require_server_admin(actor).await?;
        let previous = self.access_mode();
        if previous == mode {
            return Ok(mode);
        }
        if self.store.set_access_mode(mode).await? {
            self.setup.set_access(mode);
        } else {
            *self.setup.unstored_access.lock().unwrap() = Some(mode);
        }
        tracing::info!(
            actor = %actor.user,
            from = previous.as_str(),
            to = mode.as_str(),
            "server access mode changed"
        );
        self.record_server(
            actor.into(),
            AuditAction::ServerAccessModeChanged,
            format!(
                "access mode changed from {} to {}",
                previous.as_str(),
                mode.as_str()
            ),
        )
        .await?;
        Ok(mode)
    }
}

impl AccessService {
    /// The server-wide second-factor requirement, if an administrator set one.
    pub fn server_two_factor(&self) -> Option<TwoFactorPolicy> {
        match self.setup.settings() {
            Some(settings) => settings.two_factor,
            None => *self.setup.unstored_two_factor.lock().unwrap(),
        }
    }

    /// Requires a second factor of every account, or lifts the requirement (`None`). Accounts without one get
    /// `grace_days` (default 7) before they may only enrol. Server administrators only.
    pub async fn set_server_two_factor(
        &self,
        actor: &Identity,
        required: bool,
        grace_days: Option<u32>,
    ) -> Result<Option<TwoFactorPolicy>> {
        self.require_server_admin(actor).await?;
        let grace = crate::two_factor::validate_grace_days(grace_days)?;
        let previous = self.server_two_factor();
        let policy = required.then(|| TwoFactorPolicy {
            since: self.clock.now(),
            grace_days: grace,
        });
        if previous.is_some() == required && previous.is_none_or(|p| p.grace_days == grace) {
            return Ok(previous);
        }
        if self.store.set_server_two_factor(policy).await? {
            self.setup.set_two_factor(policy);
        } else {
            *self.setup.unstored_two_factor.lock().unwrap() = policy;
        }
        let detail = match policy {
            Some(p) => format!(
                "two-factor authentication required of every account, grace {} days",
                p.grace_days
            ),
            None => "two-factor authentication no longer required of every account".to_string(),
        };
        self.record_server(
            actor.into(),
            AuditAction::ServerTwoFactorRequirementChanged,
            detail,
        )
        .await?;
        Ok(policy)
    }
}
