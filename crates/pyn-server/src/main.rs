use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinHandle;

use pyn_core::memory::{
    MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore, MemoryObjectStore,
    MemoryRateLimitStore, MemoryWebhookStore,
};
use pyn_core::{
    AccessService, AccessStore, AuditStore, AuthProvider, ChunkedStore, Clock, MetadataStore,
    ObjectStore, RandomJitter, RateLimitStore, Repositories, ScaledClock, SystemClock,
    WebhookSettings, WebhookStore, generate_setup_token,
};
use pyn_fs::FsObjectStore;
use pyn_postgres::{PgMetadataStore, TimeSource};
use pyn_proto::StorageKind;
use pyn_server::auth::{BearerAuth, DevHeaderAuth};
use pyn_server::known_settings::unknown_in_environment;
use pyn_server::observe::LoggedAudit;
use pyn_server::passwords::PooledPasswords;
use pyn_server::ready::Readiness;
use pyn_server::settings::{Collector, Database, Problem, Settings, WebhookWorker, parse_logging};
use pyn_server::shutdown::{self, Drain, Outcome};
use pyn_server::storage::{ObjectStoreSetting, StorageReport};
use pyn_server::update_check::{GithubReleases, ReleaseSource, UpdateChecker};
use pyn_server::webhook_delivery::{DeliveryWorker, SafeSender, WorkerConfig};
use pyn_server::{AppState, router_with_log};

const NOT_CHECKED: &str = "not checked: that PYN_DATABASE_URL is reachable, that the S3 bucket exists and its keys work, that PYN_DATA_DIR is writable, and that PYN_ADDR is free to bind";

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
async fn stores(database: Option<&Database>, time: TimeSource) -> anyhow::Result<Stores> {
    Ok(match database {
        Some(db) => {
            let pg = Arc::new(
                PgMetadataStore::connect(&db.url, db.create)
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
        None => {
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
    every: Option<Duration>,
    drain: &Drain,
    tasks: &mut Vec<JoinHandle<()>>,
) -> Arc<pyn_server::certificate::CertificateReport> {
    use pyn_server::certificate::{CertificateReport, Target, spawn_monitor};
    let report = Arc::new(CertificateReport::default());
    let Some(every) = every else {
        return report;
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
    report
}

/// Starts the loop that delivers queued webhook events unless `PYN_WEBHOOK_WORKER` is `false`. Every copy of the
/// server may run one: deliveries are claimed with row locks and a lease.
fn start_webhook_delivery(
    store: Arc<dyn WebhookStore>,
    clock: Arc<dyn Clock>,
    settings: &WebhookSettings,
    worker: Option<WebhookWorker>,
    public_url: String,
    drain: &Drain,
) -> Option<JoinHandle<()>> {
    let Some(worker) = worker else {
        tracing::info!("PYN_WEBHOOK_WORKER is false: this copy does not deliver webhooks");
        return None;
    };
    let config = WorkerConfig {
        concurrency: worker.concurrency,
        ..WorkerConfig::default()
    };
    let mut sender = SafeSender::new(settings.allow_http, worker.allowed_hosts);
    if worker.allow_loopback {
        tracing::warn!(
            "PYN_WEBHOOKS_ALLOW_LOOPBACK is on: webhooks may be delivered to loopback addresses; development only"
        );
        sender = sender.allowing_loopback();
    }
    Some(
        Arc::new(DeliveryWorker::new(
            store,
            clock,
            Arc::new(sender),
            Arc::new(RandomJitter),
            settings.secret_key.clone(),
            public_url,
            config,
        ))
        .spawn(drain.clone()),
    )
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

/// The first problem as the start-up failure, with a pointer when there are more.
fn first_problem(problems: Vec<Problem>) -> anyhow::Error {
    let more = problems.len() - 1;
    let first = &problems[0].message;
    if more == 0 {
        anyhow::anyhow!("{first}")
    } else {
        anyhow::anyhow!(
            "{first}\n({more} more problem(s); run pyn-server --check-config to list every one)"
        )
    }
}

/// Runs every start-up parse without starting anything and prints what it found.
fn check_config() -> anyhow::Result<()> {
    let result = Settings::load();
    for line in unknown_in_environment() {
        println!("warning: {line}");
    }
    let failed = match &result {
        Ok(_) => {
            println!("configuration ok");
            false
        }
        Err(problems) => {
            for problem in problems {
                println!("{}", problem.line());
            }
            println!("{} problem(s) found", problems.len());
            true
        }
    };
    println!("{NOT_CHECKED}");
    if failed {
        std::process::exit(1);
    }
    Ok(())
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
        Some("--check-config") => {
            anyhow::ensure!(
                args.len() == 1,
                "--check-config takes no arguments\n{}",
                pyn_server::host_admin::HELP
            );
            return check_config();
        }
        _ => {}
    }
    let host_command = pyn_server::host_admin::parse(&args)?;
    if host_command.is_none()
        && let Some(first) = args.first().filter(|a| *a != "--healthcheck")
    {
        anyhow::bail!("unknown argument {first}\n{}", pyn_server::host_admin::HELP);
    }
    if std::env::args().nth(1).as_deref() == Some("--healthcheck") {
        let addr = std::env::var("PYN_ADDR").unwrap_or_else(|_| "127.0.0.1:7878".into());
        return healthcheck(&addr).await;
    }
    let (settings, logging) = if host_command.is_some() {
        let mut collector = Collector::default();
        let logging = parse_logging(&mut collector);
        collector.finish().map_err(first_problem)?;
        (None, logging)
    } else {
        let settings = Settings::load().map_err(first_problem)?;
        let logging = settings.logging;
        (Some(settings), logging)
    };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let subscriber = if host_command.is_some() {
        // Standard output carries a generated password and nothing else.
        pyn_server::observe::subscriber(logging.format, filter, std::io::stderr)
    } else {
        pyn_server::observe::subscriber(logging.format, filter, std::io::stdout)
    };
    tracing::subscriber::set_global_default(subscriber)?;
    if let Some(command) = host_command {
        return pyn_server::host_admin::run(command).await;
    }
    let Some(settings) = settings else {
        unreachable!("a server start always has settings");
    };
    for line in unknown_in_environment() {
        tracing::warn!("{line}");
    }
    let Settings {
        addr,
        logging: _,
        dev_auth,
        grace,
        delay,
        trust_forwarded,
        trusted_proxies,
        clock_speed,
        rules,
        initial_policy,
        request_limits,
        content: content_setting,
        database,
        instances: _,
        mailer,
        email,
        access: config,
        oidc,
        password_hashes: hashes,
        supplied_setup_token: supplied_token,
        webhooks: hook_settings,
        webhook_worker,
        max_locks,
        unreferenced_upload_cap,
        uploads_per_user,
        uploads_per_server,
        merges,
        branch_stale_days,
        update_check,
        cert_interval,
    } = settings;
    let log_config = logging.config;
    let drain = Drain::default();
    let signals = shutdown::watch_signals()?;
    let mut tasks: Vec<JoinHandle<()>> = Vec::new();
    let (clock, time): (Arc<dyn Clock>, TimeSource) = match clock_speed {
        Some(speed) => {
            tracing::warn!(
                speed,
                "PYN_DEV_CLOCK_SPEED is on: leases, sessions and rate windows run on a clock {speed} times faster than real time; development only"
            );
            (Arc::new(ScaledClock::new(speed)), TimeSource::Caller)
        }
        None => (Arc::new(SystemClock), TimeSource::Database),
    };

    let Stores {
        meta,
        access: access_store,
        audit,
        limits,
        webhooks,
        postgres,
        kind: meta_kind,
    } = stores(database.as_ref(), time).await?;
    let audit: Arc<dyn AuditStore> = if logging.audit {
        Arc::new(LoggedAudit(audit))
    } else {
        audit
    };

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

    if mailer.is_log() {
        tracing::warn!(
            "PYN_MAILER=log is on: every email, verification links included, is written to the log instead of sent; for testing only"
        );
    }
    match &hook_settings.secret_key {
        Some(key) => tracing::info!(
            key_id = key.key_id(),
            "webhook secrets and two-factor seeds are encrypted at rest"
        ),
        None => tracing::warn!(
            "PYN_SECRET_KEY is not set: webhook signing secrets and two-factor seeds are stored unencrypted"
        ),
    }
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
            .with_email(email)
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
    if let Some(limit) = max_locks {
        repos = repos.with_default_max_locks(limit)?;
    }
    if let Some(bytes) = unreferenced_upload_cap {
        repos = repos.with_unreferenced_upload_cap(bytes);
    }
    repos = repos.with_upload_limits(uploads_per_user, uploads_per_server);
    repos = repos.with_max_concurrent_merges(merges);
    repos = repos.with_branch_stale_days(branch_stale_days);
    tracing::info!(
        bytes = repos.unreferenced_upload_cap(),
        "cap on unreferenced uploads per repository"
    );
    let repos = Arc::new(repos);

    let updates = update_checker(
        update_check,
        || Ok(Arc::new(GithubReleases::new()?)),
        clock.clone(),
        &drain,
        &mut tasks,
    )?;
    let certificate =
        start_certificate_check(&access.public_url(), cert_interval, &drain, &mut tasks);
    tasks.extend(start_webhook_delivery(
        webhooks,
        clock,
        &hook_settings,
        webhook_worker,
        access.public_url(),
        &drain,
    ));
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

    let dev_auth: Option<Arc<dyn AuthProvider>> = if dev_auth {
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
    use super::update_checker;

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
}
