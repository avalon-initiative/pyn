use std::sync::Arc;

use pyn_core::memory::{MemoryMetadataStore, MemoryObjectStore};
use pyn_core::{
    MetadataStore, ObjectStore, RepoId, RepoService, Rules, ServiceConfig, SystemClock,
};
use pyn_fs::FsObjectStore;
use pyn_postgres::PgMetadataStore;
use pyn_server::{AppState, auth::DevHeaderAuth, router};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let addr = std::env::var("PYN_ADDR").unwrap_or_else(|_| "127.0.0.1:7878".into());
    let rules = match std::env::var("PYN_CONFIG") {
        Ok(path) => Rules::from_toml(&std::fs::read_to_string(path)?)?,
        Err(_) => Rules::empty(),
    };

    let (meta, meta_kind): (Arc<dyn MetadataStore>, &str) = match std::env::var("PYN_DATABASE_URL")
    {
        Ok(url) => {
            let create = std::env::var("PYN_CREATE_DATABASE").is_ok_and(|v| v == "true");
            (
                Arc::new(PgMetadataStore::connect(&url, create).await?),
                "postgres",
            )
        }
        Err(_) => (Arc::new(MemoryMetadataStore::new()), "in-memory"),
    };
    let objects: Arc<dyn ObjectStore> = match std::env::var("PYN_DATA_DIR") {
        Ok(dir) => Arc::new(FsObjectStore::open(dir).await?),
        Err(_) => Arc::new(MemoryObjectStore::new()),
    };
    let service = Arc::new(RepoService::new(
        RepoId::new("default"),
        rules,
        meta,
        objects.clone(),
        Arc::new(SystemClock),
        ServiceConfig::default(),
    ));
    let app = router(AppState {
        service,
        objects,
        auth: Arc::new(DevHeaderAuth),
    });

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::warn!("pyn-server listening on {addr} with {meta_kind} metadata and DEV auth");
    axum::serve(listener, app).await?;
    Ok(())
}
