use std::sync::Arc;

use pyn_core::memory::{MemoryAccessStore, MemoryMetadataStore, MemoryObjectStore};
use pyn_core::{
    AccessService, AccessStore, AuthProvider, MetadataStore, ObjectStore, RepoId, RepoService,
    Rules, ServiceConfig, SystemClock, UserId,
};
use pyn_fs::FsObjectStore;
use pyn_postgres::PgMetadataStore;
use pyn_server::auth::{BearerAuth, DevHeaderAuth};
use pyn_server::{AppState, router};

fn flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| v == "true")
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let addr = std::env::var("PYN_ADDR").unwrap_or_else(|_| "127.0.0.1:7878".into());
    let rules = match std::env::var("PYN_CONFIG") {
        Ok(path) => Rules::from_toml(&std::fs::read_to_string(path)?)?,
        Err(_) => Rules::empty(),
    };

    let (meta, access_store, meta_kind): (Arc<dyn MetadataStore>, Arc<dyn AccessStore>, &str) =
        match std::env::var("PYN_DATABASE_URL") {
            Ok(url) => {
                let pg =
                    Arc::new(PgMetadataStore::connect(&url, flag("PYN_CREATE_DATABASE")).await?);
                (pg.clone(), pg, "postgres")
            }
            Err(_) => (
                Arc::new(MemoryMetadataStore::new()),
                Arc::new(MemoryAccessStore::new()),
                "in-memory",
            ),
        };
    let objects: Arc<dyn ObjectStore> = match std::env::var("PYN_DATA_DIR") {
        Ok(dir) => Arc::new(FsObjectStore::open(dir).await?),
        Err(_) => Arc::new(MemoryObjectStore::new()),
    };

    let repo = RepoId::new("default");
    let clock = Arc::new(SystemClock);
    let service = Arc::new(RepoService::new(
        repo.clone(),
        rules,
        meta,
        objects.clone(),
        clock.clone(),
        ServiceConfig::default(),
    ));
    let access = Arc::new(AccessService::new(access_store, clock));

    if let Ok(admin) = std::env::var("PYN_BOOTSTRAP_ADMIN") {
        let token = access
            .bootstrap_admin(&repo, &UserId::new(admin.clone()))
            .await?;
        tracing::warn!("administrator {admin} can sign in with this token, shown once: {token}");
    }

    let dev_auth: Option<Arc<dyn AuthProvider>> = if flag("PYN_DEV_AUTH") {
        tracing::warn!("PYN_DEV_AUTH is on: anyone who can reach this server can act as any user");
        Some(Arc::new(DevHeaderAuth))
    } else {
        None
    };
    let app = router(AppState {
        service,
        objects,
        access: access.clone(),
        auth: Arc::new(BearerAuth { access, repo }),
        dev_auth,
    });

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("pyn-server listening on {addr} with {meta_kind} storage");
    axum::serve(listener, app).await?;
    Ok(())
}
