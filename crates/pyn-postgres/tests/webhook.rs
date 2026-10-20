use std::sync::Arc;

use pyn_core::contract::Webhooks;
use pyn_postgres::PgMetadataStore;

async fn postgres() -> Webhooks {
    let url = std::env::var("PYN_DATABASE_URL").expect("PYN_DATABASE_URL must be set");
    Arc::new(
        PgMetadataStore::connect_in_scratch_schema(&url)
            .await
            .unwrap(),
    )
}

pyn_core::webhook_contract_tests!(postgres(), #[ignore = "needs PYN_DATABASE_URL"]);
