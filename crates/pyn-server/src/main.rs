use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinHandle;

use pyn_core::gate::{DEFAULT_UPLOADS_PER_SERVER, DEFAULT_UPLOADS_PER_USER};
use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore,
    MemoryRateLimitStore, MemoryWebhookStore,
};
use pyn_core::merge::default_concurrent_merges;
use pyn_core::{
    AccessConfig, AccessService, AccessStore, AuditStore, AuthProvider, ChunkedStore, Clock,
    ExternalPolicy, Limits, MetadataStore, ObjectStore, RandomJitter, RateLimitStore, RateLimits,
    Repositories, Rules, ScaledClock, SecretKey, SystemClock, WebhookSettings, WebhookStore,
    generate_setup_token,
};
use pyn_fs::FsObjectStore;
use pyn_oidc::{HttpOidcProvider, OidcConfig};
use pyn_postgres::{PgMetadataStore, TimeSource};
use pyn_proto::StorageKind;
use pyn_server::auth::{BearerAuth, DevHeaderAuth};
use pyn_server::email::{MailerSetting, check_log_mailer_bind};
use pyn_server::observe::{LogConfig, LogFormat, LoggedAudit};
use pyn_server::passwords::PooledPasswords;
use pyn_server::ready::Readiness;
use pyn_server::request_limits::RequestLimits;
use pyn_server::shutdown::{self, Drain, Outcome};
use pyn_server::storage::{ObjectStoreSetting, StorageReport};
use pyn_server::update_check::{GithubReleases, ReleaseSource, UpdateChecker};
use pyn_server::webhook_delivery::{DeliveryWorker, SafeSender, WorkerConfig};
use pyn_server::{AppState, router_with_log};

fn flag(name: &str) -> bool {
    flag_or(name, false)
}

fn flag_or(name: &str, default: bool) -> bool {
    std::env::var(name).map_or(default, |v| v == "true")
}

fn number<T: std::str::FromStr>(name: &str, default: T) -> anyhow::Result<T>
where
    T::Err: std::error::Error + Send + Sync + 'static,
{
    match std::env::var(name) {
        Ok(v) => Ok(v.parse()?),
        Err(_) => Ok(default),
    }
}

struct Stores {
    meta: Arc<dyn MetadataStore>,
    access: Arc<dyn AccessStore>,
    audit: Arc<dyn AuditStore>,
    limits: Arc<dyn RateLimitStore>,
    webhooks: Arc<dyn WebhookStore>,
    postgres: Option<Arc<PgMetadataStore>>,
    kind: &'static str,
}

/// PostgreSQL when `PYN_DATABASE_URL` is set, otherwise in-memory.
async fn stores(time: TimeSource) -> anyhow::Result<Stores> {
    Ok(match std::env::var("PYN_DATABASE_URL") {
        Ok(url) => {
            let pg = Arc::new(
                PgMetadataStore::connect(&url, flag("PYN_CREATE_DATABASE"))
                    .await?
                    .with_time_source(time),
            );
            Stores {
                meta: pg.clone(),
                access: pg.clone(),
                audit: pg.clone(),
                limits: pg.clone(),
                webhooks: pg.clone(),
                postgres: Some(pg),
                kind: "postgres",
            }
        }
        Err(_) => {
            let hooks = Arc::new(MemoryWebhookStore::new());
            Stores {
                meta: Arc::new(MemoryMetadataStore::new().with_outbox(hooks.clone())),
                access: Arc::new(MemoryAccessStore::new().with_outbox(hooks.clone())),
                audit: Arc::new(MemoryAuditStore::new()),
                limits: Arc::new(MemoryRateLimitStore::new()),
                webhooks: hooks,
                postgres: None,
                kind: "in-memory",
            }
        }
    })
}

/// `PYN_DEV_CLOCK_SPEED`: a clock running that many times faster than real time, for exercising lease expiry.
/// Needs `PYN_DEV_AUTH`; it also moves sessions and rate windows.
fn dev_clock() -> anyhow::Result<(Arc<dyn Clock>, TimeSource)> {
    let Ok(raw) = std::env::var("PYN_DEV_CLOCK_SPEED") else {
        return Ok((Arc::new(SystemClock), TimeSource::Database));
    };
    let speed: f64 = raw
        .parse()
        .map_err(|_| anyhow::anyhow!("PYN_DEV_CLOCK_SPEED must be a number, got {raw:?}"))?;
    anyhow::ensure!(
        speed.is_finite() && speed >= 1.0,
        "PYN_DEV_CLOCK_SPEED must be at least 1"
    );
    anyhow::ensure!(
        flag("PYN_DEV_AUTH"),
        "PYN_DEV_CLOCK_SPEED needs PYN_DEV_AUTH=true: a fast clock is for development only"
    );
    tracing::warn!(
        speed,
        "PYN_DEV_CLOCK_SPEED is on: leases, sessions and rate windows run on a clock {speed} times faster than real time; development only"
    );
    Ok((Arc::new(ScaledClock::new(speed)), TimeSource::Caller))
}

/// A limit that stays unset, meaning unlimited, unless the variable is present.
/// A whole number of days, 0 or more.
fn day_count(name: &str, default: i64) -> anyhow::Result<i64> {
    match std::env::var(name) {
        Ok(v) => i64::try_from(whole_number(name, &v)?)
            .map_err(|_| anyhow::anyhow!("{name} is too large")),
        Err(_) => Ok(default),
    }
}

fn optional_number(name: &str) -> anyhow::Result<Option<u64>> {
    std::env::var(name)
        .ok()
        .map(|v| whole_number(name, &v))
        .transpose()
}

fn whole_number(name: &str, value: &str) -> anyhow::Result<u64> {
    value
        .parse()
        .map_err(|_| anyhow::anyhow!("{name} must be a non-negative whole number"))
}

/// A time limit in whole seconds; 0 turns it off.
fn seconds(
    name: &str,
    default: Option<std::time::Duration>,
) -> anyhow::Result<Option<std::time::Duration>> {
    match std::env::var(name) {
        Ok(v) => Ok(match whole_number(name, &v)? {
            0 => None,
            n => Some(std::time::Duration::from_secs(n)),
        }),
        Err(_) => Ok(default),
    }
}

fn request_limits() -> anyhow::Result<RequestLimits> {
    let defaults = RequestLimits::default();
    let max_expensive = match std::env::var("PYN_MAX_CONCURRENT_EXPENSIVE") {
        Ok(v) => whole_number("PYN_MAX_CONCURRENT_EXPENSIVE", &v)? as usize,
        Err(_) => defaults.max_expensive,
    };
    anyhow::ensure!(
        max_expensive >= 1,
        "PYN_MAX_CONCURRENT_EXPENSIVE must be at least 1"
    );
    Ok(RequestLimits {
        request_timeout: seconds("PYN_REQUEST_TIMEOUT_SECS", defaults.request_timeout)?,
        body_idle_timeout: seconds("PYN_BODY_IDLE_TIMEOUT_SECS", defaults.body_idle_timeout)?,
        max_expensive,
    })
}

/// `true` or `false`; anything else is an error rather than a silent default.
fn strict_flag(name: &str, default: bool) -> anyhow::Result<bool> {
    match std::env::var(name).as_deref() {
        Err(_) => Ok(default),
        Ok("true") => Ok(true),
        Ok("false") => Ok(false),
        Ok(_) => anyhow::bail!("{name} must be true or false"),
    }
}

/// `on` or `off`; anything else is an error rather than a silent default.
fn on_off(name: &str, default: bool) -> anyhow::Result<bool> {
    on_off_value(name, std::env::var(name).ok().as_deref(), default)
}

fn on_off_value(name: &str, value: Option<&str>, default: bool) -> anyhow::Result<bool> {
    match value {
        None => Ok(default),
        Some("on") => Ok(true),
        Some("off") => Ok(false),
        Some(_) => anyhow::bail!("{name} must be on or off"),
    }
}

/// The release lookup, only when `on`; off by default, and then no source is even built and nothing is contacted.
fn update_checker(
    on: bool,
    source: impl FnOnce() -> anyhow::Result<Arc<dyn ReleaseSource>>,
    clock: Arc<dyn Clock>,
    drain: &Drain,
    tasks: &mut Vec<JoinHandle<()>>,
) -> anyhow::Result<Option<Arc<UpdateChecker>>> {
    if !on {
        return Ok(None);
    }
    tracing::info!(
        "PYN_UPDATE_CHECK is on: this server asks api.github.com for the latest pyn releases about once a day"
    );
    let checker = Arc::new(UpdateChecker::new(
        source()?,
        clock,
        env!("CARGO_PKG_VERSION"),
    ));
    tasks.push(
        checker
            .clone()
            .spawn(std::time::Duration::from_secs(600), drain.clone()),
    );
    Ok(Some(checker))
}

/// Watches the certificate at the public address unless it is switched off or there is nothing public to check.
fn start_certificate_check(
    public_url: &str,
    drain: &Drain,
    tasks: &mut Vec<JoinHandle<()>>,
) -> anyhow::Result<Arc<pyn_server::certificate::CertificateReport>> {
    use pyn_server::certificate::{CertificateReport, Target, interval_from, spawn_monitor};
    let report = Arc::new(CertificateReport::default());
    let every = interval_from(
        std::env::var("PYN_CERT_CHECK_INTERVAL_SECS")
            .ok()
            .as_deref(),
    )
    .map_err(anyhow::Error::msg)?;
    let Some(every) = every else {
        return Ok(report);
    };
    match Target::from_public_url(public_url) {
        Ok(target) => {
            tracing::info!(
                host = target.host,
                "the TLS certificate at the public address is checked about every {} seconds",
                every.as_secs()
            );
            tasks.push(spawn_monitor(
                target,
                report.clone(),
                every,
                Duration::from_secs(30),
                drain.clone(),
            ));
        }
        Err(skip) => tracing::debug!("the TLS certificate is not checked: {}", skip.reason()),
    }
    Ok(report)
}

/// `PYN_WEBHOOKS_ALLOW_LOOPBACK` is refused unless plain-http development mode is on too.
fn allow_loopback() -> anyhow::Result<bool> {
    loopback_mode(
        strict_flag("PYN_WEBHOOKS_ALLOW_LOOPBACK", false)?,
        strict_flag("PYN_WEBHOOKS_ALLOW_HTTP", false)?,
    )
}

fn loopback_mode(on: bool, allow_http: bool) -> anyhow::Result<bool> {
    anyhow::ensure!(
        !on || allow_http,
        "PYN_WEBHOOKS_ALLOW_LOOPBACK is for development and needs PYN_WEBHOOKS_ALLOW_HTTP=true"
    );
    Ok(on)
}

fn webhook_settings() -> anyhow::Result<WebhookSettings> {
    allow_loopback()?;
    let secret_key = match std::env::var("PYN_SECRET_KEY") {
        Ok(text) => {
            Some(SecretKey::from_hex(&text).map_err(|e| anyhow::anyhow!("PYN_SECRET_KEY: {e}"))?)
        }
        Err(std::env::VarError::NotPresent) => None,
        Err(_) => anyhow::bail!("PYN_SECRET_KEY is not valid text"),
    };
    match &secret_key {
        Some(key) => tracing::info!(
            key_id = key.key_id(),
            "webhook secrets and two-factor seeds are encrypted at rest"
        ),
        None => tracing::warn!(
            "PYN_SECRET_KEY is not set: webhook signing secrets and two-factor seeds are stored unencrypted"
        ),
    }
    Ok(WebhookSettings {
        secret_key,
        allow_http: strict_flag("PYN_WEBHOOKS_ALLOW_HTTP", false)?,
    })
}

/// Starts the loop that delivers queued webhook events, unless `PYN_WEBHOOK_WORKER` is `false`. Every copy of the
/// server may run one: deliveries are claimed with row locks and a lease.
fn start_webhook_delivery(
    store: Arc<dyn WebhookStore>,
    clock: Arc<dyn Clock>,
    settings: &WebhookSettings,
    public_url: String,
    drain: &Drain,
) -> anyhow::Result<Option<JoinHandle<()>>> {
    if !strict_flag("PYN_WEBHOOK_WORKER", true)? {
        tracing::info!("PYN_WEBHOOK_WORKER is false: this copy does not deliver webhooks");
        return Ok(None);
    }
    let allowed_hosts: Vec<String> = std::env::var("PYN_WEBHOOKS_ALLOWED_HOSTS")
        .map(|v| v.split(',').map(str::to_string).collect())
        .unwrap_or_default();
    let config = WorkerConfig {
        concurrency: number(
            "PYN_WEBHOOKS_CONCURRENCY",
            WorkerConfig::default().concurrency,
        )?,
        ..WorkerConfig::default()
    };
    anyhow::ensure!(
        config.concurrency >= 1,
        "PYN_WEBHOOKS_CONCURRENCY must be at least 1"
    );
    let mut sender = SafeSender::new(settings.allow_http, allowed_hosts);
    if allow_loopback()? {
        tracing::warn!(
            "PYN_WEBHOOKS_ALLOW_LOOPBACK is on: webhooks may be delivered to loopback addresses; development only"
        );
        sender = sender.allowing_loopback();
    }
    let handle = Arc::new(DeliveryWorker::new(
        store,
        clock,
        Arc::new(sender),
        Arc::new(RandomJitter),
        settings.secret_key.clone(),
        public_url,
        config,
    ))
    .spawn(drain.clone());
    Ok(Some(handle))
}

/// OpenID Connect sign-in, only when the issuer, client id and client secret are all set.
fn oidc_provider() -> anyhow::Result<Option<Arc<HttpOidcProvider>>> {
    let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    let (issuer, id, secret) = (
        var("PYN_OIDC_ISSUER"),
        var("PYN_OIDC_CLIENT_ID"),
        var("PYN_OIDC_CLIENT_SECRET"),
    );
    let (Some(issuer), Some(id), Some(secret)) = (issuer.clone(), id.clone(), secret.clone())
    else {
        anyhow::ensure!(
            issuer.is_none() && id.is_none() && secret.is_none(),
            "PYN_OIDC_ISSUER, PYN_OIDC_CLIENT_ID and PYN_OIDC_CLIENT_SECRET must be set together"
        );
        return Ok(None);
    };
    let mut config = OidcConfig::new(&issuer, &id, &secret);
    if let Some(name) = var("PYN_OIDC_NAME") {
        config.display_name = name;
    }
    if let Some(claim) = var("PYN_OIDC_USERNAME_CLAIM") {
        config.username_claim = claim;
    }
    Ok(Some(Arc::new(HttpOidcProvider::new(config)?)))
}

/// Asks this server's own `/healthz` on the configured address; an unspecified bind address is probed on loopback.
async fn healthcheck(addr: &str) -> anyhow::Result<()> {
    let url = pyn_server::host_admin::healthz_url(addr)?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()?;
    client.get(url).send().await?.error_for_status()?;
    Ok(())
}

/// `PYN_SHUTDOWN_GRACE_SECS`: seconds in-flight requests get to finish after a stop signal, default 30; 0 is no grace.
fn shutdown_grace() -> anyhow::Result<Duration> {
    match std::env::var("PYN_SHUTDOWN_GRACE_SECS") {
        Ok(v) => Ok(Duration::from_secs(whole_number(
            "PYN_SHUTDOWN_GRACE_SECS",
            &v,
        )?)),
        Err(_) => Ok(Duration::from_secs(30)),
    }
}

/// `PYN_SHUTDOWN_DRAIN_DELAY_SECS`: seconds the server stays up and not ready before closing its listener, default 0.
fn drain_delay() -> anyhow::Result<Duration> {
    drain_delay_value(
        std::env::var("PYN_SHUTDOWN_DRAIN_DELAY_SECS")
            .ok()
            .as_deref(),
    )
}

fn drain_delay_value(value: Option<&str>) -> anyhow::Result<Duration> {
    const NAME: &str = "PYN_SHUTDOWN_DRAIN_DELAY_SECS";
    const MAX: u64 = 3600;
    let Some(v) = value else {
        return Ok(Duration::ZERO);
    };
    let secs = whole_number(NAME, v)?;
    anyhow::ensure!(secs <= MAX, "{NAME} must be at most {MAX} seconds");
    Ok(Duration::from_secs(secs))
}

/// `PYN_TRUSTED_PROXIES`: how many proxies append to `X-Forwarded-For`, 1 to 16, default 1; needs forwarded headers trusted.
fn trusted_proxies(trust_forwarded: bool) -> anyhow::Result<usize> {
    trusted_proxies_value(
        trust_forwarded,
        std::env::var("PYN_TRUSTED_PROXIES").ok().as_deref(),
    )
}

fn trusted_proxies_value(trust_forwarded: bool, value: Option<&str>) -> anyhow::Result<usize> {
    const NAME: &str = "PYN_TRUSTED_PROXIES";
    let Some(v) = value else {
        return Ok(1);
    };
    anyhow::ensure!(
        trust_forwarded,
        "{NAME} only applies with PYN_TRUST_FORWARDED_FOR=true"
    );
    let hops = whole_number(NAME, v)?;
    anyhow::ensure!(
        (1..=16).contains(&hops),
        "{NAME} must be a whole number from 1 to 16"
    );
    Ok(hops as usize)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--help" | "-h") => {
            println!("{}", pyn_server::host_admin::HELP);
            return Ok(());
        }
        Some("--version" | "-V") => {
            println!("pyn-server {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        _ => {}
    }
    let host_command = pyn_server::host_admin::parse(&args)?;
    if host_command.is_none()
        && let Some(first) = args.first().filter(|a| *a != "--healthcheck")
    {
        anyhow::bail!("unknown argument {first}\n{}", pyn_server::host_admin::HELP);
    }
    let addr = std::env::var("PYN_ADDR").unwrap_or_else(|_| "127.0.0.1:7878".into());
    if std::env::args().nth(1).as_deref() == Some("--healthcheck") {
        return healthcheck(&addr).await;
    }
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let log_format = match std::env::var("PYN_LOG_FORMAT") {
        Ok(v) => v.parse::<LogFormat>().map_err(anyhow::Error::msg)?,
        Err(_) => LogFormat::default(),
    };
    let log_config = LogConfig {
        access: strict_flag("PYN_ACCESS_LOG", true)?,
        client: strict_flag("PYN_ACCESS_LOG_CLIENT", false)?,
    };
    let audit_log = strict_flag("PYN_AUDIT_LOG", false)?;
    let subscriber = if host_command.is_some() {
        // Standard output carries a generated password and nothing else.
        pyn_server::observe::subscriber(log_format, filter, std::io::stderr)
    } else {
        pyn_server::observe::subscriber(log_format, filter, std::io::stdout)
    };
    tracing::subscriber::set_global_default(subscriber)?;
    if let Some(command) = host_command {
        return pyn_server::host_admin::run(command).await;
    }
    pyn_server::check_dev_auth_bind(
        &addr,
        flag("PYN_DEV_AUTH"),
        strict_flag("PYN_DEV_AUTH_ALLOW_NON_LOOPBACK", false)?,
    )
    .map_err(anyhow::Error::msg)?;
    let grace = shutdown_grace()?;
    let delay = drain_delay()?;
    let trust_forwarded = flag("PYN_TRUST_FORWARDED_FOR");
    let trusted_proxies = trusted_proxies(trust_forwarded)?;
    let drain = Drain::default();
    let signals = shutdown::watch_signals()?;
    let mut tasks: Vec<JoinHandle<()>> = Vec::new();
    let (clock, time) = dev_clock()?;
    let (rules, initial_policy) = match std::env::var("PYN_CONFIG") {
        Ok(path) => {
            let size = std::fs::metadata(&path)?.len();
            anyhow::ensure!(
                size <= pyn_core::rules::MAX_POLICY_BYTES,
                "PYN_CONFIG is larger than {} bytes",
                pyn_core::rules::MAX_POLICY_BYTES
            );
            let text = std::fs::read_to_string(path)?;
            (Rules::from_toml(&text)?, text)
        }
        Err(_) => (
            Rules::empty(),
            pyn_core::rules::DEFAULT_INITIAL_POLICY.to_string(),
        ),
    };

    let request_limits = request_limits()?;
    let content_setting = ObjectStoreSetting::from_env(
        |name| std::env::var(name).ok(),
        |path| std::fs::read_to_string(path),
    )
    .map_err(anyhow::Error::msg)?;

    let Stores {
        meta,
        access: access_store,
        audit,
        limits,
        webhooks,
        postgres,
        kind: meta_kind,
    } = stores(time).await?;
    let audit: Arc<dyn AuditStore> = if audit_log {
        Arc::new(LoggedAudit(audit))
    } else {
        audit
    };
    let instances: u32 = number("PYN_INSTANCES", 1)?;
    anyhow::ensure!(instances >= 1, "PYN_INSTANCES must be at least 1");
    pyn_server::storage::check_instances(
        instances,
        meta_kind == "postgres",
        content_setting.is_shared(),
    )
    .map_err(anyhow::Error::msg)?;
    let (objects, storage): (Arc<dyn ObjectStore>, Arc<StorageReport>) = match content_setting {
        ObjectStoreSetting::Filesystem(dir) => {
            let fs = FsObjectStore::open(dir).await?;
            tasks.push(fs.spawn_sweeper(Duration::from_secs(900)));
            (
                Arc::new(ChunkedStore::new(Arc::new(fs))),
                Arc::new(StorageReport::new(StorageKind::Filesystem, None)),
            )
        }
        ObjectStoreSetting::S3(settings) => {
            let bucket = settings.bucket.clone();
            let store = pyn_server::storage::open_s3(*settings).await?;
            let report = Arc::new(StorageReport::new(StorageKind::S3, Some(bucket.clone())));
            tasks.push(pyn_server::storage::spawn_monitor(
                store.clone(),
                report.clone(),
                pyn_server::storage::PROBE_INTERVAL,
                drain.clone(),
            ));
            tracing::info!(bucket, "file content is kept in an S3 bucket");
            (Arc::new(ChunkedStore::new(store)), report)
        }
        ObjectStoreSetting::Memory => (
            Arc::new(ChunkedStore::new(Arc::new(MemoryObjectStore::new()))),
            Arc::new(StorageReport::default()),
        ),
    };
    if let Some(pg) = postgres {
        let done = pg.migrate_content_ids(objects.as_ref()).await?;
        if done.migrated > 0 || !done.missing.is_empty() {
            tracing::info!(
                migrated = done.migrated,
                missing = done.missing.len(),
                "re-keyed stored content to git blob ids"
            );
        }
        for old in &done.missing {
            tracing::warn!(content = %old, "stored content not found; its revisions keep the SHA-256 content id");
        }
        let history = pg.migrate_history().await?;
        if history.commits > 0 {
            tracing::info!(
                repositories = history.repositories,
                commits = history.commits,
                "migrated existing history into git commits"
            );
        }
        let branches = pg.migrate_branches().await?;
        if branches.branches > 0 || branches.pointers > 0 {
            tracing::info!(
                branches = branches.branches,
                pointers = branches.pointers,
                "created default branches for existing repositories"
            );
        }
    }

    let mailer =
        MailerSetting::from_env(|name| std::env::var(name).ok()).map_err(anyhow::Error::msg)?;
    check_log_mailer_bind(
        &addr,
        mailer.is_log(),
        strict_flag("PYN_MAILER_LOG_ALLOW_NON_LOOPBACK", false)?,
    )
    .map_err(anyhow::Error::msg)?;
    if mailer.is_log() {
        tracing::warn!(
            "PYN_MAILER=log is on: every email, verification links included, is written to the log instead of sent; for testing only"
        );
    }
    let defaults = AccessConfig::default();
    let limit_defaults = RateLimits::default();
    let config = AccessConfig {
        registration: match std::env::var("PYN_REGISTRATION") {
            Ok(mode) => mode.parse()?,
            Err(_) => defaults.registration,
        },
        access_mode: match std::env::var("PYN_ACCESS") {
            Ok(mode) => mode.parse()?,
            Err(_) => defaults.access_mode,
        },
        session_days: number("PYN_SESSION_DAYS", defaults.session_days)?,
        require_email_verification: pyn_server::email::email_verification(
            strict_flag("PYN_EMAIL_VERIFICATION", mailer.is_configured())?,
            &mailer,
        )
        .map_err(anyhow::Error::msg)?,
        require_approval: flag("PYN_REQUIRE_APPROVAL"),
        limits: RateLimits {
            sign_in_per_account: number(
                "PYN_RATE_SIGN_IN_ACCOUNT",
                limit_defaults.sign_in_per_account,
            )?,
            sign_in_per_client: number(
                "PYN_RATE_SIGN_IN_CLIENT",
                limit_defaults.sign_in_per_client,
            )?,
            register_per_client: number(
                "PYN_RATE_REGISTER_CLIENT",
                limit_defaults.register_per_client,
            )?,
            mail_per_email: number("PYN_RATE_MAIL_PER_EMAIL", limit_defaults.mail_per_email)?,
            people_per_client: number("PYN_RATE_PEOPLE_CLIENT", limit_defaults.people_per_client)?,
            service_per_credential: number(
                "PYN_RATE_SERVICE_CREDENTIAL",
                limit_defaults.service_per_credential,
            )?,
        },
        public_url: std::env::var("PYN_PUBLIC_URL").unwrap_or_else(|_| format!("http://{addr}")),
        allowed_origins: match std::env::var("PYN_ALLOWED_ORIGINS") {
            Ok(list) => pyn_server::origin::parse_list(&list)
                .map_err(|e| anyhow::anyhow!("PYN_ALLOWED_ORIGINS: {e}"))?,
            Err(_) => Vec::new(),
        },
        webauthn_rp_id: std::env::var("PYN_WEBAUTHN_RP_ID")
            .ok()
            .filter(|id| !id.trim().is_empty()),
        verification_ttl: defaults.verification_ttl,
        org_creation: match std::env::var("PYN_ORG_CREATION") {
            Ok(mode) => mode.parse()?,
            Err(_) => defaults.org_creation,
        },
        org_member_add: match std::env::var("PYN_ORG_MEMBER_ADD") {
            Ok(mode) => mode.parse()?,
            Err(_) => defaults.org_member_add,
        },
        org_invitation_ttl: match optional_number("PYN_ORG_INVITATION_DAYS")? {
            Some(0) => anyhow::bail!("PYN_ORG_INVITATION_DAYS must be at least 1"),
            Some(days) => chrono::Duration::days(i64::try_from(days)?),
            None => defaults.org_invitation_ttl,
        },
        default_limits: Limits {
            repositories: optional_number("PYN_DEFAULT_MAX_REPOSITORIES")?,
            members: optional_number("PYN_DEFAULT_MAX_ORG_MEMBERS")?,
            storage_bytes: optional_number("PYN_DEFAULT_MAX_STORAGE_BYTES")?,
        }
        .validate()?,
        external: ExternalPolicy {
            create_accounts: flag("PYN_OIDC_CREATE_ACCOUNTS"),
            password_sign_in: flag_or("PYN_PASSWORD_SIGN_IN", true),
            redirect_url: std::env::var("PYN_OIDC_REDIRECT_URL").ok(),
        },
        deletion_window_days: day_count(
            "PYN_ACCOUNT_DELETION_WINDOW_DAYS",
            defaults.deletion_window_days,
        )?,
        disabled_retention_days: day_count(
            "PYN_DISABLED_ACCOUNT_RETENTION_DAYS",
            defaults.disabled_retention_days,
        )?,
    };
    let oidc = oidc_provider()?;
    anyhow::ensure!(
        oidc.is_some() || config.external.password_sign_in,
        "PYN_PASSWORD_SIGN_IN=false needs an OpenID Connect provider (PYN_OIDC_ISSUER)"
    );
    let parallelism = std::thread::available_parallelism().map_or(2, |n| n.get());
    let hashes = number("PYN_PASSWORD_HASHES", parallelism)?;
    anyhow::ensure!(hashes >= 1, "PYN_PASSWORD_HASHES must be at least 1");
    let supplied_token = std::env::var("PYN_SETUP_TOKEN").ok();
    let setup_token = match &supplied_token {
        Some(token) => token.clone(),
        None => generate_setup_token()?,
    };
    let hook_settings = webhook_settings()?;
    let mut service = AccessService::new(
        access_store,
        clock.clone(),
        Arc::new(pyn_server::events::WebhookLimitEvents::new(
            webhooks.clone(),
            config.public_url.clone(),
        )),
    )
    .with_webhooks(webhooks.clone(), hook_settings.clone());
    if let Some(provider) = oidc {
        tracing::info!("sign-in with the OpenID Connect provider is on");
        service = service.with_oidc(provider);
    }
    let access = Arc::new(
        service
            .with_config(config)
            .with_registry(meta.clone())
            .with_audit(audit.clone())
            .with_rate_limits(limits)
            .with_email(mailer.sender().map_err(anyhow::Error::msg)?)
            .with_passwords(Arc::new(PooledPasswords::new(hashes)))
            .with_setup_token(&setup_token)?,
    );
    if access.load_setup().await? {
        if supplied_token.is_some() {
            tracing::warn!("PYN_SETUP_TOKEN is set but this server is already set up; remove it");
        }
    } else if supplied_token.is_some() {
        tracing::warn!(
            "this server is not set up yet; finish setup with the PYN_SETUP_TOKEN you supplied"
        );
    } else {
        tracing::warn!(
            "this server is not set up yet; the one-time setup token is printed on stderr"
        );
        eprintln!(
            "----- pyn one-time setup token -----\n{setup_token}\n------------------------------------"
        );
        eprintln!(
            "{}",
            pyn_server::setup_hint(std::env::var("PYN_PUBLIC_URL").ok().as_deref())
        );
    }
    if let Some(warning) = pyn_server::insecure_cookie_warning(&addr, &access.public_url()) {
        tracing::warn!("{warning}");
    }
    if !access.passkeys_available() {
        tracing::info!(
            "passkeys are off: PYN_PUBLIC_URL ({}) needs a host name over https (or localhost){}",
            access.public_url(),
            if std::env::var_os("PYN_WEBAUTHN_RP_ID").is_some() {
                ", and PYN_WEBAUTHN_RP_ID must be a parent domain of that host"
            } else {
                ""
            }
        );
    }
    for warning in access.startup_warnings() {
        tracing::warn!("{warning}");
    }
    if let Some(warning) = access.single_admin_warning().await? {
        tracing::warn!("{warning}");
    }
    let mut repos = Repositories::new(
        meta,
        objects.clone(),
        audit,
        access.clone(),
        clock.clone(),
        rules,
    )
    .with_initial_policy(&initial_policy)?;
    if let Ok(limit) = std::env::var("PYN_MAX_LOCKS_ALLOWED_PER_USER") {
        repos = repos.with_default_max_locks(limit.parse()?)?;
    }
    if let Some(bytes) = optional_number("PYN_MAX_UNREFERENCED_UPLOAD_BYTES")? {
        repos = repos.with_unreferenced_upload_cap(bytes);
    }
    let uploads_per_user: u32 = number("PYN_MAX_UPLOADS_PER_USER", DEFAULT_UPLOADS_PER_USER)?;
    let uploads_per_server: u32 = number("PYN_MAX_UPLOADS", DEFAULT_UPLOADS_PER_SERVER)?;
    anyhow::ensure!(
        uploads_per_user >= 1 && uploads_per_server >= 1,
        "PYN_MAX_UPLOADS_PER_USER and PYN_MAX_UPLOADS must be at least 1"
    );
    repos = repos.with_upload_limits(uploads_per_user, uploads_per_server);
    let merges: usize = number("PYN_MAX_CONCURRENT_MERGES", default_concurrent_merges())?;
    anyhow::ensure!(merges >= 1, "PYN_MAX_CONCURRENT_MERGES must be at least 1");
    repos = repos.with_max_concurrent_merges(merges);
    tracing::info!(
        bytes = repos.unreferenced_upload_cap(),
        "cap on unreferenced uploads per repository"
    );
    let repos = Arc::new(repos);

    let updates = update_checker(
        on_off("PYN_UPDATE_CHECK", false)?,
        || Ok(Arc::new(GithubReleases::new()?)),
        clock.clone(),
        &drain,
        &mut tasks,
    )?;
    let certificate = start_certificate_check(&access.public_url(), &drain, &mut tasks)?;
    tasks.extend(start_webhook_delivery(
        webhooks,
        clock,
        &hook_settings,
        access.public_url(),
        &drain,
    )?);
    tasks.push(pyn_server::account_deletion::spawn(
        access.clone(),
        Duration::from_secs(3600),
        drain.clone(),
    ));
    tasks.push(pyn_server::access_session_sweep::spawn(
        access.clone(),
        Duration::from_secs(60),
        drain.clone(),
    ));
    tasks.push(pyn_server::lock_stream_api::spawn_sweep(
        repos.clone(),
        Duration::from_secs(5),
        drain.clone(),
    ));
    tasks.push(pyn_server::tus_sweep::spawn(
        repos.clone(),
        Duration::from_secs(600),
        drain.clone(),
    ));

    let dev_auth: Option<Arc<dyn AuthProvider>> = if flag("PYN_DEV_AUTH") {
        tracing::warn!(
            addr,
            "PYN_DEV_AUTH is on: anyone who can reach this server can act as any user"
        );
        Some(Arc::new(DevHeaderAuth))
    } else {
        None
    };
    let app = router_with_log(
        AppState {
            repos,
            objects,
            access: access.clone(),
            auth: Arc::new(BearerAuth { access }),
            dev_auth,
            trust_forwarded,
            trusted_proxies,
            updates,
            storage,
            certificate,
            ready: Readiness::new(drain.clone()),
        },
        request_limits,
        log_config,
    );

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("pyn-server listening on {addr} with {meta_kind} storage");
    let server = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown({
        let drain = drain.clone();
        async move { drain.closed().await }
    })
    .into_future();
    let outcome = shutdown::run(server, &drain, delay, grace, signals).await?;
    for task in &tasks {
        task.abort();
    }
    match outcome {
        Outcome::Clean => {
            tracing::info!("pyn-server stopped");
            Ok(())
        }
        Outcome::Forced { .. } => std::process::exit(1),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        drain_delay_value, loopback_mode, on_off_value, trusted_proxies_value, update_checker,
        whole_number,
    };

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
                err.to_string(),
                "PYN_MAX_UNREFERENCED_UPLOAD_BYTES must be a non-negative whole number",
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
            assert_eq!(
                err.to_string(),
                "PYN_UPDATE_CHECK must be on or off",
                "{bad:?}"
            );
        }
    }

    #[test]
    fn an_off_update_check_builds_no_source() {
        let clock: std::sync::Arc<dyn pyn_core::Clock> = std::sync::Arc::new(pyn_core::SystemClock);
        let checker = update_checker(
            false,
            || panic!("the source must not be built"),
            clock,
            &super::Drain::default(),
            &mut Vec::new(),
        );
        assert!(checker.unwrap().is_none());
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
            let err = drain_delay_value(Some(bad)).unwrap_err().to_string();
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
            let err = trusted_proxies_value(true, Some(bad))
                .unwrap_err()
                .to_string();
            assert!(err.contains("PYN_TRUSTED_PROXIES"), "{bad:?}: {err}");
        }
        let err = trusted_proxies_value(false, Some("2"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("PYN_TRUST_FORWARDED_FOR=true"), "{err}");
    }
}
