//! Start-up settings: every `PYN_*` the server reads, parsed and cross-checked in one pass that collects every
//! problem. The real start and `--check-config` both call [`Settings::load`], so they cannot disagree.

use std::fmt::Display;
use std::sync::Arc;
use std::time::Duration;

use pyn_core::gate::{DEFAULT_UPLOADS_PER_SERVER, DEFAULT_UPLOADS_PER_USER};
use pyn_core::memory::MemoryAccessStore;
use pyn_core::merge::default_concurrent_merges;
use pyn_core::{
    AccessConfig, AccessService, DEFAULT_BRANCH_STALE_DAYS, DEFAULT_MAX_FORKS_PER_OWNER,
    DEFAULT_MAX_FORKS_PER_REPOSITORY, EmailSender, ExternalPolicy, Limits, MemoryLimitEvents,
    NullEmailSender, RateLimits, Rules, SecretKey, SystemClock, WebhookSettings,
};
use pyn_oidc::{HttpOidcProvider, OidcConfig};

use crate::email::{MailerSetting, check_log_mailer_bind, email_verification};
use crate::observe::{LogConfig, LogFormat};
use crate::request_limits::RequestLimits;
use crate::storage::{ObjectStoreSetting, check_instances};
use crate::webhook_delivery::WorkerConfig;

/// One thing wrong with the configuration. `message` is the text the real start fails with.
#[derive(Debug, Clone)]
pub struct Problem {
    pub name: &'static str,
    pub message: String,
}

impl Problem {
    /// `PYN_NAME: what is wrong`; a message that already starts with a variable name is printed as it is.
    pub fn line(&self) -> String {
        if self.message.starts_with("PYN_") {
            self.message.clone()
        } else {
            format!("{}: {}", self.name, self.message)
        }
    }
}

/// Gathers problems instead of stopping at the first.
#[derive(Default)]
pub struct Collector {
    problems: Vec<Problem>,
}

impl Collector {
    fn take<T>(&mut self, name: &'static str, result: Result<T, impl Display>) -> Option<T> {
        match result {
            Ok(value) => Some(value),
            Err(e) => {
                self.problems.push(Problem {
                    name,
                    message: e.to_string(),
                });
                None
            }
        }
    }

    fn or_default<T>(
        &mut self,
        name: &'static str,
        result: Result<T, impl Display>,
        default: T,
    ) -> T {
        self.take(name, result).unwrap_or(default)
    }

    fn or<T: Default>(&mut self, name: &'static str, result: Result<T, impl Display>) -> T {
        self.take(name, result).unwrap_or_default()
    }

    fn fail(&mut self, name: &'static str, message: impl Into<String>) {
        self.problems.push(Problem {
            name,
            message: message.into(),
        });
    }

    /// The problems, or `Ok` when there are none.
    pub fn finish(self) -> Result<(), Vec<Problem>> {
        if self.problems.is_empty() {
            Ok(())
        } else {
            Err(self.problems)
        }
    }
}

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

pub fn flag(name: &str) -> bool {
    flag_or(name, false)
}

pub fn flag_or(name: &str, default: bool) -> bool {
    var(name).map_or(default, |v| v == "true")
}

pub fn number<T: std::str::FromStr>(name: &str, default: T) -> Result<T, String> {
    match var(name) {
        Some(v) => v
            .parse()
            .map_err(|_| format!("{name} must be a whole number")),
        None => Ok(default),
    }
}

/// A whole number of days, 0 or more.
fn day_count(name: &str, default: i64) -> Result<i64, String> {
    match var(name) {
        Some(v) => {
            i64::try_from(whole_number(name, &v)?).map_err(|_| format!("{name} is too large"))
        }
        None => Ok(default),
    }
}

/// A limit that stays unset, meaning unlimited, unless the variable is present.
fn optional_number(name: &str) -> Result<Option<u64>, String> {
    var(name).map(|v| whole_number(name, &v)).transpose()
}

pub fn whole_number(name: &str, value: &str) -> Result<u64, String> {
    value
        .parse()
        .map_err(|_| format!("{name} must be a non-negative whole number"))
}

/// A time limit in whole seconds; 0 turns it off.
fn seconds(name: &str, default: Option<Duration>) -> Result<Option<Duration>, String> {
    match var(name) {
        Some(v) => Ok(match whole_number(name, &v)? {
            0 => None,
            n => Some(Duration::from_secs(n)),
        }),
        None => Ok(default),
    }
}

fn max_expensive(default: usize) -> Result<usize, String> {
    let value = match var("PYN_MAX_CONCURRENT_EXPENSIVE") {
        Some(v) => whole_number("PYN_MAX_CONCURRENT_EXPENSIVE", &v)? as usize,
        None => default,
    };
    if value < 1 {
        return Err("PYN_MAX_CONCURRENT_EXPENSIVE must be at least 1".into());
    }
    Ok(value)
}

/// `true` or `false`; anything else is an error rather than a silent default.
pub fn strict_flag(name: &str, default: bool) -> Result<bool, String> {
    match var(name).as_deref() {
        None => Ok(default),
        Some("true") => Ok(true),
        Some("false") => Ok(false),
        Some(_) => Err(format!("{name} must be true or false")),
    }
}

/// `on` or `off`; anything else is an error rather than a silent default.
fn on_off(name: &str, default: bool) -> Result<bool, String> {
    on_off_value(name, var(name).as_deref(), default)
}

pub fn on_off_value(name: &str, value: Option<&str>, default: bool) -> Result<bool, String> {
    match value {
        None => Ok(default),
        Some("on") => Ok(true),
        Some("off") => Ok(false),
        Some(_) => Err(format!("{name} must be on or off")),
    }
}

/// `PYN_WEBHOOKS_ALLOW_LOOPBACK` is refused unless plain-http development mode is on too.
pub fn loopback_mode(on: bool, allow_http: bool) -> Result<bool, String> {
    if on && !allow_http {
        return Err(
            "PYN_WEBHOOKS_ALLOW_LOOPBACK is for development and needs PYN_WEBHOOKS_ALLOW_HTTP=true"
                .into(),
        );
    }
    Ok(on)
}

fn allow_loopback() -> Result<bool, String> {
    loopback_mode(
        strict_flag("PYN_WEBHOOKS_ALLOW_LOOPBACK", false)?,
        strict_flag("PYN_WEBHOOKS_ALLOW_HTTP", false)?,
    )
}

/// `PYN_SHUTDOWN_DRAIN_DELAY_SECS`: seconds the server stays up and not ready before closing its listener, default 0.
pub fn drain_delay_value(value: Option<&str>) -> Result<Duration, String> {
    const NAME: &str = "PYN_SHUTDOWN_DRAIN_DELAY_SECS";
    const MAX: u64 = 3600;
    let Some(v) = value else {
        return Ok(Duration::ZERO);
    };
    let secs = whole_number(NAME, v)?;
    if secs > MAX {
        return Err(format!("{NAME} must be at most {MAX} seconds"));
    }
    Ok(Duration::from_secs(secs))
}

/// `PYN_TRUSTED_PROXIES`: how many proxies append to `X-Forwarded-For`, 1 to 16, default 1; needs forwarded headers trusted.
pub fn trusted_proxies_value(trust_forwarded: bool, value: Option<&str>) -> Result<usize, String> {
    const NAME: &str = "PYN_TRUSTED_PROXIES";
    let Some(v) = value else {
        return Ok(1);
    };
    if !trust_forwarded {
        return Err(format!(
            "{NAME} only applies with PYN_TRUST_FORWARDED_FOR=true"
        ));
    }
    let hops = whole_number(NAME, v)?;
    if !(1..=16).contains(&hops) {
        return Err(format!("{NAME} must be a whole number from 1 to 16"));
    }
    Ok(hops as usize)
}

/// A listen address needs a host and a port number; whether it can be bound is only known at bind time.
fn check_addr(addr: &str) -> Result<(), String> {
    let ok = addr
        .rsplit_once(':')
        .is_some_and(|(host, port)| !host.is_empty() && port.parse::<u16>().is_ok());
    if ok {
        Ok(())
    } else {
        Err("PYN_ADDR must be host:port with a port number, such as 127.0.0.1:7878".into())
    }
}

/// A PostgreSQL address; the value is not echoed because it carries the password.
fn check_database_url(url: &str) -> Result<(), String> {
    if url.starts_with("postgres://") || url.starts_with("postgresql://") {
        Ok(())
    } else {
        Err("PYN_DATABASE_URL must be a postgres:// or postgresql:// address".into())
    }
}

/// The three logging switches, parsed before the log subscriber exists.
#[derive(Clone, Copy)]
pub struct Logging {
    pub format: LogFormat,
    pub config: LogConfig,
    pub audit: bool,
}

pub fn parse_logging(c: &mut Collector) -> Logging {
    let format = match var("PYN_LOG_FORMAT") {
        Some(v) => c
            .take("PYN_LOG_FORMAT", v.parse::<LogFormat>())
            .unwrap_or_default(),
        None => LogFormat::default(),
    };
    Logging {
        format,
        config: LogConfig {
            access: c.or_default("PYN_ACCESS_LOG", strict_flag("PYN_ACCESS_LOG", true), true),
            client: c.or(
                "PYN_ACCESS_LOG_CLIENT",
                strict_flag("PYN_ACCESS_LOG_CLIENT", false),
            ),
        },
        audit: c.or("PYN_AUDIT_LOG", strict_flag("PYN_AUDIT_LOG", false)),
    }
}

pub struct Database {
    pub url: String,
    pub create: bool,
}

pub struct WebhookWorker {
    pub allowed_hosts: Vec<String>,
    pub concurrency: usize,
    pub allow_loopback: bool,
}

/// Everything the server reads at start, validated.
pub struct Settings {
    pub addr: String,
    pub logging: Logging,
    pub dev_auth: bool,
    pub grace: Duration,
    pub delay: Duration,
    pub trust_forwarded: bool,
    pub trusted_proxies: usize,
    pub clock_speed: Option<f64>,
    pub rules: Rules,
    pub initial_policy: String,
    pub request_limits: RequestLimits,
    pub content: ObjectStoreSetting,
    pub database: Option<Database>,
    pub instances: u32,
    pub mailer: MailerSetting,
    pub email: Arc<dyn EmailSender>,
    pub access: AccessConfig,
    pub oidc: Option<Arc<HttpOidcProvider>>,
    pub password_hashes: usize,
    pub supplied_setup_token: Option<String>,
    pub webhooks: WebhookSettings,
    pub webhook_worker: Option<WebhookWorker>,
    pub max_locks: Option<u32>,
    pub unreferenced_upload_cap: Option<u64>,
    pub uploads_per_user: u32,
    pub uploads_per_server: u32,
    pub merges: usize,
    pub branch_stale_days: u32,
    /// Forks one repository may have and one owner may own.
    pub fork_caps: (u64, u64),
    pub update_check: bool,
    pub cert_interval: Option<Duration>,
}

impl Settings {
    /// Reads the environment and the files it names; opens no port, database or bucket and starts no task.
    pub fn load() -> Result<Self, Vec<Problem>> {
        let mut c = Collector::default();
        let settings = Self::parse(&mut c);
        c.finish().map(|()| settings)
    }

    fn parse(c: &mut Collector) -> Self {
        let addr = var("PYN_ADDR").unwrap_or_else(|| "127.0.0.1:7878".into());
        if let Err(e) = check_addr(&addr) {
            c.fail("PYN_ADDR", e);
        }
        let logging = parse_logging(c);
        let dev_auth = flag("PYN_DEV_AUTH");
        let acknowledged = c.or(
            "PYN_DEV_AUTH_ALLOW_NON_LOOPBACK",
            strict_flag("PYN_DEV_AUTH_ALLOW_NON_LOOPBACK", false),
        );
        c.take(
            "PYN_DEV_AUTH",
            crate::check_dev_auth_bind(&addr, dev_auth, acknowledged),
        );
        let grace = c.or(
            "PYN_SHUTDOWN_GRACE_SECS",
            match var("PYN_SHUTDOWN_GRACE_SECS") {
                Some(v) => whole_number("PYN_SHUTDOWN_GRACE_SECS", &v).map(Duration::from_secs),
                None => Ok(Duration::from_secs(30)),
            },
        );
        let delay = c.or(
            "PYN_SHUTDOWN_DRAIN_DELAY_SECS",
            drain_delay_value(var("PYN_SHUTDOWN_DRAIN_DELAY_SECS").as_deref()),
        );
        let trust_forwarded = flag("PYN_TRUST_FORWARDED_FOR");
        let trusted_proxies = c
            .take(
                "PYN_TRUSTED_PROXIES",
                trusted_proxies_value(trust_forwarded, var("PYN_TRUSTED_PROXIES").as_deref()),
            )
            .unwrap_or(1);
        let clock_speed = Self::parse_clock_speed(c);
        let (rules, initial_policy) = Self::parse_policy(c);
        let defaults = RequestLimits::default();
        let request_limits = RequestLimits {
            request_timeout: c.or(
                "PYN_REQUEST_TIMEOUT_SECS",
                seconds("PYN_REQUEST_TIMEOUT_SECS", defaults.request_timeout),
            ),
            body_idle_timeout: c.or(
                "PYN_BODY_IDLE_TIMEOUT_SECS",
                seconds("PYN_BODY_IDLE_TIMEOUT_SECS", defaults.body_idle_timeout),
            ),
            max_expensive: c
                .take(
                    "PYN_MAX_CONCURRENT_EXPENSIVE",
                    max_expensive(defaults.max_expensive),
                )
                .unwrap_or(defaults.max_expensive),
        };
        let content = c.take(
            "PYN_S3_BUCKET",
            ObjectStoreSetting::from_env(var, |path| std::fs::read_to_string(path)),
        );
        let database = Self::parse_database(c);
        let instances = c
            .take("PYN_INSTANCES", number("PYN_INSTANCES", 1u32))
            .unwrap_or(1);
        if instances < 1 {
            c.fail("PYN_INSTANCES", "PYN_INSTANCES must be at least 1");
        } else if let Some(content) = &content {
            c.take(
                "PYN_INSTANCES",
                check_instances(instances, database.is_some(), content.is_shared()),
            );
        }
        let content = content.unwrap_or(ObjectStoreSetting::Memory);

        let mailer = c
            .take("PYN_MAILER", MailerSetting::from_env(var))
            .unwrap_or(MailerSetting::None);
        let acknowledged = c.or(
            "PYN_MAILER_LOG_ALLOW_NON_LOOPBACK",
            strict_flag("PYN_MAILER_LOG_ALLOW_NON_LOOPBACK", false),
        );
        c.take(
            "PYN_MAILER",
            check_log_mailer_bind(&addr, mailer.is_log(), acknowledged),
        );
        let email = c
            .take("PYN_MAILER", mailer.sender())
            .unwrap_or_else(|| Arc::new(NullEmailSender));
        let (access, oidc, supplied_setup_token) = Self::parse_access(c, &addr, &mailer);
        let password_hashes = Self::parse_hashes(c);
        let (webhooks, webhook_worker) = Self::parse_webhooks(c);
        let max_locks = Self::parse_max_locks(c);
        let uploads_per_user = c.or_default(
            "PYN_MAX_UPLOADS_PER_USER",
            number("PYN_MAX_UPLOADS_PER_USER", DEFAULT_UPLOADS_PER_USER),
            DEFAULT_UPLOADS_PER_USER,
        );
        let uploads_per_server = c.or_default(
            "PYN_MAX_UPLOADS",
            number("PYN_MAX_UPLOADS", DEFAULT_UPLOADS_PER_SERVER),
            DEFAULT_UPLOADS_PER_SERVER,
        );
        if uploads_per_user < 1 || uploads_per_server < 1 {
            c.fail(
                "PYN_MAX_UPLOADS",
                "PYN_MAX_UPLOADS_PER_USER and PYN_MAX_UPLOADS must be at least 1",
            );
        }
        let branch_stale_days = match optional_number("PYN_BRANCH_STALE_DAYS") {
            Ok(Some(days)) if days >= 1 && days <= u64::from(u32::MAX) => days as u32,
            Ok(None) => DEFAULT_BRANCH_STALE_DAYS,
            Ok(Some(_)) => {
                c.fail(
                    "PYN_BRANCH_STALE_DAYS",
                    "PYN_BRANCH_STALE_DAYS must be at least 1 and at most 4294967295",
                );
                DEFAULT_BRANCH_STALE_DAYS
            }
            Err(e) => {
                c.fail("PYN_BRANCH_STALE_DAYS", e);
                DEFAULT_BRANCH_STALE_DAYS
            }
        };
        let fork_caps = (
            c.or_default(
                "PYN_MAX_FORKS_PER_REPO",
                number("PYN_MAX_FORKS_PER_REPO", DEFAULT_MAX_FORKS_PER_REPOSITORY),
                DEFAULT_MAX_FORKS_PER_REPOSITORY,
            ),
            c.or_default(
                "PYN_MAX_FORKS_PER_OWNER",
                number("PYN_MAX_FORKS_PER_OWNER", DEFAULT_MAX_FORKS_PER_OWNER),
                DEFAULT_MAX_FORKS_PER_OWNER,
            ),
        );
        let default_merges = default_concurrent_merges();
        let merges = c.or_default(
            "PYN_MAX_CONCURRENT_MERGES",
            number("PYN_MAX_CONCURRENT_MERGES", default_merges),
            default_merges,
        );
        if merges < 1 {
            c.fail(
                "PYN_MAX_CONCURRENT_MERGES",
                "PYN_MAX_CONCURRENT_MERGES must be at least 1",
            );
        }
        let unreferenced_upload_cap = c.or(
            "PYN_MAX_UNREFERENCED_UPLOAD_BYTES",
            optional_number("PYN_MAX_UNREFERENCED_UPLOAD_BYTES"),
        );
        let update_check = c.or("PYN_UPDATE_CHECK", on_off("PYN_UPDATE_CHECK", false));
        let cert_interval = c
            .take(
                "PYN_CERT_CHECK_INTERVAL_SECS",
                crate::certificate::interval_from(var("PYN_CERT_CHECK_INTERVAL_SECS").as_deref()),
            )
            .unwrap_or(None);
        Self {
            addr,
            logging,
            dev_auth,
            grace,
            delay,
            trust_forwarded,
            trusted_proxies,
            clock_speed,
            rules,
            initial_policy,
            request_limits,
            content,
            database,
            instances,
            mailer,
            email,
            access,
            oidc,
            password_hashes,
            supplied_setup_token,
            webhooks,
            webhook_worker,
            max_locks,
            unreferenced_upload_cap,
            uploads_per_user,
            uploads_per_server,
            merges,
            branch_stale_days,
            fork_caps,
            update_check,
            cert_interval,
        }
    }

    /// `PYN_DEV_CLOCK_SPEED`: a clock running that many times faster than real time; needs `PYN_DEV_AUTH`.
    fn parse_clock_speed(c: &mut Collector) -> Option<f64> {
        let raw = var("PYN_DEV_CLOCK_SPEED")?;
        let Ok(speed) = raw.parse::<f64>() else {
            c.fail(
                "PYN_DEV_CLOCK_SPEED",
                format!("PYN_DEV_CLOCK_SPEED must be a number, got {raw:?}"),
            );
            return None;
        };
        if !(speed.is_finite() && speed >= 1.0) {
            c.fail(
                "PYN_DEV_CLOCK_SPEED",
                "PYN_DEV_CLOCK_SPEED must be at least 1",
            );
        } else if !flag("PYN_DEV_AUTH") {
            c.fail(
                "PYN_DEV_CLOCK_SPEED",
                "PYN_DEV_CLOCK_SPEED needs PYN_DEV_AUTH=true: a fast clock is for development only",
            );
        }
        Some(speed)
    }

    fn parse_policy(c: &mut Collector) -> (Rules, String) {
        let fallback = || {
            (
                Rules::empty(),
                pyn_core::rules::DEFAULT_INITIAL_POLICY.to_string(),
            )
        };
        let Some(path) = var("PYN_CONFIG") else {
            return fallback();
        };
        let read = || -> anyhow::Result<(Rules, String)> {
            let size = std::fs::metadata(&path)?.len();
            anyhow::ensure!(
                size <= pyn_core::rules::MAX_POLICY_BYTES,
                "PYN_CONFIG is larger than {} bytes",
                pyn_core::rules::MAX_POLICY_BYTES
            );
            let text = std::fs::read_to_string(&path)?;
            Ok((Rules::from_toml(&text)?, text))
        };
        c.take("PYN_CONFIG", read()).unwrap_or_else(fallback)
    }

    fn parse_database(c: &mut Collector) -> Option<Database> {
        let url = var("PYN_DATABASE_URL")?;
        c.take("PYN_DATABASE_URL", check_database_url(&url));
        let create = flag("PYN_CREATE_DATABASE");
        Some(Database { url, create })
    }

    fn parse_hashes(c: &mut Collector) -> usize {
        let parallelism = std::thread::available_parallelism().map_or(2, |n| n.get());
        let hashes = c.or_default(
            "PYN_PASSWORD_HASHES",
            number("PYN_PASSWORD_HASHES", parallelism),
            parallelism,
        );
        if hashes < 1 {
            c.fail(
                "PYN_PASSWORD_HASHES",
                "PYN_PASSWORD_HASHES must be at least 1",
            );
        }
        hashes
    }

    fn parse_max_locks(c: &mut Collector) -> Option<u32> {
        let raw = var("PYN_MAX_LOCKS_ALLOWED_PER_USER")?;
        let parsed = raw
            .parse::<u32>()
            .map_err(|_| "PYN_MAX_LOCKS_ALLOWED_PER_USER must be a whole number".to_string())
            .and_then(|n| {
                pyn_core::validate_max_locks(n)
                    .map_err(|e| format!("PYN_MAX_LOCKS_ALLOWED_PER_USER: {e}"))
            });
        c.take("PYN_MAX_LOCKS_ALLOWED_PER_USER", parsed)
    }

    fn parse_webhooks(c: &mut Collector) -> (WebhookSettings, Option<WebhookWorker>) {
        let loopback = c.take("PYN_WEBHOOKS_ALLOW_LOOPBACK", allow_loopback());
        let secret_key = match std::env::var("PYN_SECRET_KEY") {
            Ok(text) => c.take(
                "PYN_SECRET_KEY",
                SecretKey::from_hex(&text).map_err(|e| format!("PYN_SECRET_KEY: {e}")),
            ),
            Err(std::env::VarError::NotPresent) => None,
            Err(_) => {
                c.fail("PYN_SECRET_KEY", "PYN_SECRET_KEY is not valid text");
                None
            }
        };
        let allow_http = c.or(
            "PYN_WEBHOOKS_ALLOW_HTTP",
            strict_flag("PYN_WEBHOOKS_ALLOW_HTTP", false),
        );
        let settings = WebhookSettings {
            secret_key,
            allow_http,
        };
        let worker = c.take(
            "PYN_WEBHOOK_WORKER",
            strict_flag("PYN_WEBHOOK_WORKER", true),
        );
        if worker != Some(true) {
            return (settings, None);
        }
        let allowed_hosts = var("PYN_WEBHOOKS_ALLOWED_HOSTS")
            .map(|v| v.split(',').map(str::to_string).collect())
            .unwrap_or_default();
        let default_concurrency = WorkerConfig::default().concurrency;
        let concurrency = c.or_default(
            "PYN_WEBHOOKS_CONCURRENCY",
            number("PYN_WEBHOOKS_CONCURRENCY", default_concurrency),
            default_concurrency,
        );
        if concurrency < 1 {
            c.fail(
                "PYN_WEBHOOKS_CONCURRENCY",
                "PYN_WEBHOOKS_CONCURRENCY must be at least 1",
            );
        }
        let worker = WebhookWorker {
            allowed_hosts,
            concurrency,
            allow_loopback: loopback.unwrap_or(false),
        };
        (settings, Some(worker))
    }

    fn parse_oidc(c: &mut Collector) -> Option<Arc<HttpOidcProvider>> {
        let value = |name: &str| var(name).filter(|v| !v.is_empty());
        let (issuer, id, secret) = (
            value("PYN_OIDC_ISSUER"),
            value("PYN_OIDC_CLIENT_ID"),
            value("PYN_OIDC_CLIENT_SECRET"),
        );
        let (Some(issuer), Some(id), Some(secret)) = (issuer.clone(), id.clone(), secret.clone())
        else {
            if !(issuer.is_none() && id.is_none() && secret.is_none()) {
                c.fail(
                    "PYN_OIDC_ISSUER",
                    "PYN_OIDC_ISSUER, PYN_OIDC_CLIENT_ID and PYN_OIDC_CLIENT_SECRET must be set together",
                );
            }
            return None;
        };
        let mut config = OidcConfig::new(&issuer, &id, &secret);
        if let Some(name) = value("PYN_OIDC_NAME") {
            config.display_name = name;
        }
        if let Some(claim) = value("PYN_OIDC_USERNAME_CLAIM") {
            config.username_claim = claim;
        }
        c.take(
            "PYN_OIDC_ISSUER",
            HttpOidcProvider::new(config).map(Arc::new),
        )
    }

    fn parse_access(
        c: &mut Collector,
        addr: &str,
        mailer: &MailerSetting,
    ) -> (AccessConfig, Option<Arc<HttpOidcProvider>>, Option<String>) {
        let defaults = AccessConfig::default();
        let limit_defaults = RateLimits::default();
        let verification = c.or(
            "PYN_EMAIL_VERIFICATION",
            strict_flag("PYN_EMAIL_VERIFICATION", mailer.is_configured())
                .and_then(|requested| email_verification(requested, mailer)),
        );
        // A value that does not parse as the named type is reported against that variable.
        macro_rules! parsed {
            ($name:literal, $default:expr) => {
                match var($name) {
                    Some(v) => c
                        .take($name, v.parse().map_err(|e| format!("{}: {e}", $name)))
                        .unwrap_or($default),
                    None => $default,
                }
            };
        }
        let registration = parsed!("PYN_REGISTRATION", defaults.registration);
        let access_mode = parsed!("PYN_ACCESS", defaults.access_mode);
        let org_creation = parsed!("PYN_ORG_CREATION", defaults.org_creation);
        let org_member_add = parsed!("PYN_ORG_MEMBER_ADD", defaults.org_member_add);
        let session_days = c.or_default(
            "PYN_SESSION_DAYS",
            number("PYN_SESSION_DAYS", defaults.session_days),
            defaults.session_days,
        );
        let session_remember_days = c.or_default(
            "PYN_SESSION_REMEMBER_DAYS",
            number("PYN_SESSION_REMEMBER_DAYS", defaults.session_remember_days),
            defaults.session_remember_days,
        );
        let mut rate =
            |name: &'static str, default: u32| c.or_default(name, number(name, default), default);
        let limits = RateLimits {
            sign_in_per_account: rate(
                "PYN_RATE_SIGN_IN_ACCOUNT",
                limit_defaults.sign_in_per_account,
            ),
            sign_in_per_client: rate("PYN_RATE_SIGN_IN_CLIENT", limit_defaults.sign_in_per_client),
            register_per_client: rate(
                "PYN_RATE_REGISTER_CLIENT",
                limit_defaults.register_per_client,
            ),
            mail_per_email: rate("PYN_RATE_MAIL_PER_EMAIL", limit_defaults.mail_per_email),
            people_per_client: rate("PYN_RATE_PEOPLE_CLIENT", limit_defaults.people_per_client),
            service_per_credential: rate(
                "PYN_RATE_SERVICE_CREDENTIAL",
                limit_defaults.service_per_credential,
            ),
        };
        let allowed_origins = match var("PYN_ALLOWED_ORIGINS") {
            Some(list) => c
                .take(
                    "PYN_ALLOWED_ORIGINS",
                    crate::origin::parse_list(&list)
                        .map_err(|e| format!("PYN_ALLOWED_ORIGINS: {e}")),
                )
                .unwrap_or_default(),
            None => Vec::new(),
        };
        let invitation = match optional_number("PYN_ORG_INVITATION_DAYS") {
            Ok(Some(0)) => {
                c.fail(
                    "PYN_ORG_INVITATION_DAYS",
                    "PYN_ORG_INVITATION_DAYS must be at least 1",
                );
                defaults.org_invitation_ttl
            }
            Ok(Some(days)) => match i64::try_from(days) {
                Ok(days) => chrono::Duration::days(days),
                Err(_) => {
                    c.fail(
                        "PYN_ORG_INVITATION_DAYS",
                        "PYN_ORG_INVITATION_DAYS is too large",
                    );
                    defaults.org_invitation_ttl
                }
            },
            Ok(None) => defaults.org_invitation_ttl,
            Err(e) => {
                c.fail("PYN_ORG_INVITATION_DAYS", e);
                defaults.org_invitation_ttl
            }
        };
        let default_limits = Limits {
            repositories: c.or(
                "PYN_DEFAULT_MAX_REPOSITORIES",
                optional_number("PYN_DEFAULT_MAX_REPOSITORIES"),
            ),
            members: c.or(
                "PYN_DEFAULT_MAX_ORG_MEMBERS",
                optional_number("PYN_DEFAULT_MAX_ORG_MEMBERS"),
            ),
            storage_bytes: c.or(
                "PYN_DEFAULT_MAX_STORAGE_BYTES",
                optional_number("PYN_DEFAULT_MAX_STORAGE_BYTES"),
            ),
        };
        let default_limits = c
            .take("PYN_DEFAULT_MAX_REPOSITORIES", default_limits.validate())
            .unwrap_or_default();
        let config = AccessConfig {
            registration,
            access_mode,
            session_days,
            session_remember_days,
            require_email_verification: verification,
            require_approval: flag("PYN_REQUIRE_APPROVAL"),
            limits,
            public_url: var("PYN_PUBLIC_URL").unwrap_or_else(|| format!("http://{addr}")),
            allowed_origins,
            webauthn_rp_id: var("PYN_WEBAUTHN_RP_ID").filter(|id| !id.trim().is_empty()),
            verification_ttl: defaults.verification_ttl,
            org_creation,
            org_member_add,
            org_invitation_ttl: invitation,
            default_limits,
            external: ExternalPolicy {
                create_accounts: flag("PYN_OIDC_CREATE_ACCOUNTS"),
                password_sign_in: flag_or("PYN_PASSWORD_SIGN_IN", true),
                redirect_url: var("PYN_OIDC_REDIRECT_URL"),
            },
            deletion_window_days: c.or_default(
                "PYN_ACCOUNT_DELETION_WINDOW_DAYS",
                day_count(
                    "PYN_ACCOUNT_DELETION_WINDOW_DAYS",
                    defaults.deletion_window_days,
                ),
                defaults.deletion_window_days,
            ),
            disabled_retention_days: c.or_default(
                "PYN_DISABLED_ACCOUNT_RETENTION_DAYS",
                day_count(
                    "PYN_DISABLED_ACCOUNT_RETENTION_DAYS",
                    defaults.disabled_retention_days,
                ),
                defaults.disabled_retention_days,
            ),
        };
        let oidc = Self::parse_oidc(c);
        if oidc.is_none() && !config.external.password_sign_in {
            c.fail(
                "PYN_PASSWORD_SIGN_IN",
                "PYN_PASSWORD_SIGN_IN=false needs an OpenID Connect provider (PYN_OIDC_ISSUER)",
            );
        }
        let supplied = var("PYN_SETUP_TOKEN");
        if let Some(token) = &supplied {
            // The service validates the token; a throwaway in-memory one runs the same check.
            let probe = AccessService::new(
                Arc::new(MemoryAccessStore::new()),
                Arc::new(SystemClock),
                Arc::new(MemoryLimitEvents::default()),
            );
            c.take("PYN_SETUP_TOKEN", probe.with_setup_token(token).map(|_| ()));
        }
        (config, oidc, supplied)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_whole_number_parses_including_zero() {
        assert_eq!(whole_number("X", "0").unwrap(), 0);
        assert_eq!(whole_number("X", "1073741824").unwrap(), 1 << 30);
    }

    #[test]
    fn anything_else_is_refused_naming_the_variable() {
        for bad in ["", "-1", "1.5", "32G", "ten", " 5"] {
            let err = whole_number("PYN_MAX_UNREFERENCED_UPLOAD_BYTES", bad).unwrap_err();
            assert_eq!(
                err, "PYN_MAX_UNREFERENCED_UPLOAD_BYTES must be a non-negative whole number",
                "{bad:?}"
            );
        }
    }

    #[test]
    fn the_update_check_takes_only_on_or_off() {
        assert!(!on_off_value("X", None, false).unwrap());
        assert!(on_off_value("X", Some("on"), false).unwrap());
        assert!(!on_off_value("X", Some("off"), true).unwrap());
        for bad in ["true", "ON", "", "1", "yes"] {
            let err = on_off_value("PYN_UPDATE_CHECK", Some(bad), false).unwrap_err();
            assert_eq!(err, "PYN_UPDATE_CHECK must be on or off", "{bad:?}");
        }
    }

    #[test]
    fn loopback_delivery_needs_the_http_development_mode() {
        assert!(loopback_mode(true, false).is_err());
        assert!(loopback_mode(true, true).unwrap());
        assert!(!loopback_mode(false, false).unwrap());
    }

    #[test]
    fn the_drain_delay_defaults_to_zero_and_is_bounded() {
        assert_eq!(drain_delay_value(None).unwrap().as_secs(), 0);
        assert_eq!(drain_delay_value(Some("20")).unwrap().as_secs(), 20);
        assert_eq!(drain_delay_value(Some("3600")).unwrap().as_secs(), 3600);
        for bad in ["soon", "-1", "1.5", "", "3601", "99999999999"] {
            let err = drain_delay_value(Some(bad)).unwrap_err();
            assert!(
                err.contains("PYN_SHUTDOWN_DRAIN_DELAY_SECS"),
                "{bad:?}: {err}"
            );
        }
    }

    #[test]
    fn trusted_proxies_are_one_to_sixteen_and_need_forwarded_headers_trusted() {
        assert_eq!(trusted_proxies_value(false, None).unwrap(), 1);
        assert_eq!(trusted_proxies_value(true, None).unwrap(), 1);
        assert_eq!(trusted_proxies_value(true, Some("2")).unwrap(), 2);
        assert_eq!(trusted_proxies_value(true, Some("16")).unwrap(), 16);
        for bad in ["0", "17", "two", "-1", "", "1.5"] {
            let err = trusted_proxies_value(true, Some(bad)).unwrap_err();
            assert!(err.contains("PYN_TRUSTED_PROXIES"), "{bad:?}: {err}");
        }
        let err = trusted_proxies_value(false, Some("2")).unwrap_err();
        assert!(err.contains("PYN_TRUST_FORWARDED_FOR=true"), "{err}");
    }

    #[test]
    fn a_listen_address_needs_a_host_and_a_port() {
        for good in [
            "127.0.0.1:7878",
            "0.0.0.0:80",
            "[::1]:7878",
            "localhost:8080",
        ] {
            assert!(check_addr(good).is_ok(), "{good}");
        }
        for bad in ["7878", "localhost", ":7878", "host:port", "host:70000"] {
            assert!(check_addr(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_database_address_is_never_echoed() {
        assert!(check_database_url("postgres://u:pw@h/db").is_ok());
        let err = check_database_url("mysql://u:hunter2@h/db").unwrap_err();
        assert!(!err.contains("hunter2"));
    }

    #[test]
    fn a_problem_line_names_the_variable_once() {
        let named = Problem {
            name: "PYN_ADDR",
            message: "PYN_ADDR must be on or off".into(),
        };
        assert_eq!(named.line(), "PYN_ADDR must be on or off");
        let bare = Problem {
            name: "PYN_ADDR",
            message: "invalid".into(),
        };
        assert_eq!(bare.line(), "PYN_ADDR: invalid");
    }
}
