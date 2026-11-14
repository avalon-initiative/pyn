use std::sync::Arc;

use pyn_core::contract::{Access, Audit, Store};
use pyn_postgres::PgMetadataStore;

async fn connect() -> Arc<PgMetadataStore> {
    let url = std::env::var("PYN_DATABASE_URL").expect("PYN_DATABASE_URL must be set");
    Arc::new(
        PgMetadataStore::connect_in_scratch_schema(&url)
            .await
            .unwrap(),
    )
}

async fn access() -> Access {
    connect().await
}

async fn audit() -> Audit {
    connect().await
}

async fn meta() -> Store {
    connect().await
}

pyn_core::account_deletion_contract_tests!(access(), #[ignore = "needs PYN_DATABASE_URL"]);
pyn_core::author_lookup_contract_tests!(meta(), #[ignore = "needs PYN_DATABASE_URL"]);
pyn_core::actor_lookup_contract_tests!(audit(), #[ignore = "needs PYN_DATABASE_URL"]);
