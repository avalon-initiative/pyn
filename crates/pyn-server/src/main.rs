use std::sync::Arc;

use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore,
    MemoryRateLimitStore,
};
use pyn_core::{
    AccessConfig, AccessService, AccessStore, AuditStore, AuthProvider, MetadataStore, ObjectStore,
    RateLimitStore, RateLimits, Repositories, Rules, SystemClock, generate_setup_token,
};
use pyn_fs::FsObjectStore;
use pyn_postgres::PgMetadataStore;
use pyn_server::auth::{BearerAuth, DevHeaderAuth};
use pyn_server::email::LogEmailSender;
use pyn_server::passwords::PooledPasswords;
use pyn_server::{AppState, router};

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
                limits: pg,
                kind: "postgres",
            }
        }
        Err(_) => Stores {
            meta: Arc::new(MemoryMetadataStore::new()),
            access: Arc::new(MemoryAccessStore::new()),
            audit: Arc::new(MemoryAuditStore::new()),
            limits: Arc::new(MemoryRateLimitStore::new()),
            kind: "in-memory",
        },
    })
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let addr = std::env::var("PYN_ADDR").unwrap_or_else(|_| "127.0.0.1:7878".into());
    let rules = match std::env::var("PYN_CONFIG") {
        Ok(path) => Rules::from_toml(&std::fs::read_to_string(path)?)?,
        Err(_) => Rules::empty(),
    };

    let Stores {
        meta,
        access: access_store,
        audit,
        limits,
        kind: meta_kind,
    } = stores().await?;
    let objects: Arc<dyn ObjectStore> = match std::env::var("PYN_DATA_DIR") {
        Ok(dir) => Arc::new(FsObjectStore::open(dir).await?),
        Err(_) => Arc::new(MemoryObjectStore::new()),
    };

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
        verification_ttl: defaults.verification_ttl,
        org_creation: match std::env::var("PYN_ORG_CREATION") {
            Ok(mode) => mode.parse()?,
            Err(_) => defaults.org_creation,
        },
    };
    let parallelism = std::thread::available_parallelism().map_or(2, |n| n.get());
    let hashes = number("PYN_PASSWORD_HASHES", parallelism)?;
    anyhow::ensure!(hashes >= 1, "PYN_PASSWORD_HASHES must be at least 1");
    match std::env::var("PYN_EMAIL").as_deref() {
        Ok("log") | Err(_) => {}
        Ok(other) => anyhow::bail!("unknown PYN_EMAIL {other:?}; only \"log\" is available"),
    }
    let supplied_token = std::env::var("PYN_SETUP_TOKEN").ok();
    let setup_token = match &supplied_token {
        Some(token) => token.clone(),
        None => generate_setup_token()?,
    };
    let access = Arc::new(
        AccessService::new(access_store, clock.clone())
            .with_config(config)
            .with_registry(meta.clone())
            .with_audit(audit.clone())
            .with_rate_limits(limits)
            .with_email(Arc::new(LogEmailSender))
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
            "this server is not set up yet; finish setup with this one-time token: {setup_token}"
        );
    }
    for warning in access.startup_warnings() {
        tracing::warn!("{warning}");
    }
    let mut repos = Repositories::new(meta, objects.clone(), audit, access.clone(), clock, rules);
    if let Ok(limit) = std::env::var("PYN_MAX_LOCKS_ALLOWED_PER_USER") {
        repos = repos.with_default_max_locks(limit.parse()?)?;
    }
    let repos = Arc::new(repos);

    let dev_auth: Option<Arc<dyn AuthProvider>> = if flag("PYN_DEV_AUTH") {
        tracing::warn!("PYN_DEV_AUTH is on: anyone who can reach this server can act as any user");
        Some(Arc::new(DevHeaderAuth))
    } else {
        None
    };
    let app = router(AppState {
        repos,
        objects,
        access: access.clone(),
        auth: Arc::new(BearerAuth { access }),
        dev_auth,
        trust_forwarded: flag("PYN_TRUST_FORWARDED_FOR"),
    });

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("pyn-server listening on {addr} with {meta_kind} storage");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await?;
    Ok(())
}
