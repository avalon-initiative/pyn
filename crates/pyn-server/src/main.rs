use std::sync::Arc;

use pyn_core::memory::{MemoryMetadataStore, MemoryObjectStore};
use pyn_core::{RepoId, RepoService, Rules, ServiceConfig, SystemClock};
use pyn_server::{AppState, auth::DevHeaderAuth, router};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let addr = std::env::var("PYN_ADDR").unwrap_or_else(|_| "127.0.0.1:7878".into());
    let rules = match std::env::var("PYN_CONFIG") {
        Ok(path) => Rules::from_toml(&std::fs::read_to_string(path)?)?,
        Err(_) => Rules::empty(),
    };

    // In-memory stores: state is lost on restart. Postgres + local-FS object store come next.
    let objects = Arc::new(MemoryObjectStore::new());
    let service = Arc::new(RepoService::new(
        RepoId::new("default"),
        rules,
        Arc::new(MemoryMetadataStore::new()),
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
    tracing::warn!("pyn-server listening on {addr} with in-memory storage and DEV auth");
    axum::serve(listener, app).await?;
    Ok(())
}
