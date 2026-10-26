use std::sync::Arc;

use pyn_core::gate::{DEFAULT_UPLOADS_PER_SERVER, DEFAULT_UPLOADS_PER_USER};
use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore,
    MemoryRateLimitStore, MemoryWebhookStore,
};
use pyn_core::merge::default_concurrent_merges;
use pyn_core::{
    AccessConfig, AccessService, AccessStore, AuditStore, AuthProvider, ChunkedStore,
    ExternalPolicy, Limits, MetadataStore, ObjectStore, RateLimitStore, RateLimits, Repositories,
    Rules, SecretKey, SystemClock, WebhookSettings, WebhookStore, generate_setup_token,
};
use pyn_fs::FsObjectStore;
use pyn_oidc::{HttpOidcProvider, OidcConfig};
use pyn_postgres::PgMetadataStore;
use pyn_server::auth::{BearerAuth, DevHeaderAuth};
use pyn_server::email::LogEmailSender;
use pyn_server::passwords::PooledPasswords;
use pyn_server::request_limits::RequestLimits;
use pyn_server::{AppState, router_with};

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
async fn stores() -> anyhow::Result<Stores> {
    Ok(match std::env::var("PYN_DATABASE_URL") {
        Ok(url) => {
            let pg = Arc::new(PgMetadataStore::connect(&url, flag("PYN_CREATE_DATABASE")).await?);
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

/// A limit that stays unset, meaning unlimited, unless the variable is present.
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

fn webhook_settings() -> anyhow::Result<WebhookSettings> {
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
            "webhook secrets are encrypted at rest"
        ),
        None => tracing::warn!(
            "PYN_SECRET_KEY is not set: webhook signing secrets are stored unencrypted"
        ),
    }
    Ok(WebhookSettings {
        secret_key,
        allow_http: strict_flag("PYN_WEBHOOKS_ALLOW_HTTP", false)?,
    })
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let addr = std::env::var("PYN_ADDR").unwrap_or_else(|_| "127.0.0.1:7878".into());
    let rules = match std::env::var("PYN_CONFIG") {
        Ok(path) => {
            let size = std::fs::metadata(&path)?.len();
            anyhow::ensure!(
                size <= pyn_core::rules::MAX_POLICY_BYTES,
                "PYN_CONFIG is larger than {} bytes",
                pyn_core::rules::MAX_POLICY_BYTES
            );
            Rules::from_toml(&std::fs::read_to_string(path)?)?
        }
        Err(_) => Rules::empty(),
    };

    let request_limits = request_limits()?;

    let Stores {
        meta,
        access: access_store,
        audit,
        limits,
        webhooks,
        postgres,
        kind: meta_kind,
    } = stores().await?;
    let instances: u32 = number("PYN_INSTANCES", 1)?;
    anyhow::ensure!(instances >= 1, "PYN_INSTANCES must be at least 1");
    anyhow::ensure!(
        instances == 1 || (meta_kind == "postgres" && std::env::var("PYN_DATA_DIR").is_ok()),
        "PYN_INSTANCES above 1 needs PYN_DATABASE_URL and PYN_DATA_DIR on storage all copies share"
    );
    let objects: Arc<dyn ObjectStore> = match std::env::var("PYN_DATA_DIR") {
        Ok(dir) => {
            let fs = FsObjectStore::open(dir).await?;
            fs.spawn_sweeper(std::time::Duration::from_secs(900));
            Arc::new(ChunkedStore::new(Arc::new(fs)))
        }
        Err(_) => Arc::new(ChunkedStore::new(Arc::new(MemoryObjectStore::new()))),
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

    let clock = Arc::new(SystemClock);
    let defaults = AccessConfig::default();
    let limit_defaults = RateLimits::default();
    let config = AccessConfig {
        registration: match std::env::var("PYN_REGISTRATION") {
            Ok(mode) => mode.parse()?,
            Err(_) => defaults.registration,
        },
        session_days: number("PYN_SESSION_DAYS", defaults.session_days)?,
        require_email_verification: flag_or("PYN_EMAIL_VERIFICATION", true),
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
        },
        public_url: std::env::var("PYN_PUBLIC_URL").unwrap_or_else(|_| format!("http://{addr}")),
        allowed_origins: match std::env::var("PYN_ALLOWED_ORIGINS") {
            Ok(list) => pyn_server::origin::parse_list(&list)
                .map_err(|e| anyhow::anyhow!("PYN_ALLOWED_ORIGINS: {e}"))?,
            Err(_) => Vec::new(),
        },
        verification_ttl: defaults.verification_ttl,
        org_creation: match std::env::var("PYN_ORG_CREATION") {
            Ok(mode) => mode.parse()?,
            Err(_) => defaults.org_creation,
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
    };
    let oidc = oidc_provider()?;
    anyhow::ensure!(
        oidc.is_some() || config.external.password_sign_in,
        "PYN_PASSWORD_SIGN_IN=false needs an OpenID Connect provider (PYN_OIDC_ISSUER)"
    );
    let parallelism = std::thread::available_parallelism().map_or(2, |n| n.get());
    let hashes = number("PYN_PASSWORD_HASHES", parallelism)?;
    anyhow::ensure!(hashes >= 1, "PYN_PASSWORD_HASHES must be at least 1");
    match std::env::var("PYN_EMAIL").as_deref() {
        Ok("log") | Err(_) => {}
        Ok(other) => anyhow::bail!("unknown PYN_EMAIL {other:?}; only \"log\" is available"),
    }
    let log_email_bodies = flag("PYN_EMAIL_LOG_BODIES");
    if log_email_bodies {
        tracing::warn!(
            "PYN_EMAIL_LOG_BODIES is on: verification links are written to the log; for local development only"
        );
    }
    let supplied_token = std::env::var("PYN_SETUP_TOKEN").ok();
    let setup_token = match &supplied_token {
        Some(token) => token.clone(),
        None => generate_setup_token()?,
    };
    let mut service = AccessService::new(
        access_store,
        clock.clone(),
        Arc::new(pyn_server::events::WebhookLimitEvents::new(
            webhooks.clone(),
            config.public_url.clone(),
        )),
    )
    .with_webhooks(webhooks, webhook_settings()?);
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
            .with_email(Arc::new(LogEmailSender::new(log_email_bodies)))
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
    }
    if let Some(warning) = pyn_server::insecure_cookie_warning(&addr, &access.public_url()) {
        tracing::warn!("{warning}");
    }
    for warning in access.startup_warnings() {
        tracing::warn!("{warning}");
    }
    let mut repos = Repositories::new(meta, objects.clone(), audit, access.clone(), clock, rules);
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

    let dev_auth: Option<Arc<dyn AuthProvider>> = if flag("PYN_DEV_AUTH") {
        tracing::warn!("PYN_DEV_AUTH is on: anyone who can reach this server can act as any user");
        Some(Arc::new(DevHeaderAuth))
    } else {
        None
    };
    let app = router_with(
        AppState {
            repos,
            objects,
            access: access.clone(),
            auth: Arc::new(BearerAuth { access }),
            dev_auth,
            trust_forwarded: flag("PYN_TRUST_FORWARDED_FOR"),
        },
        request_limits,
    );

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("pyn-server listening on {addr} with {meta_kind} storage");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::whole_number;

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
}
