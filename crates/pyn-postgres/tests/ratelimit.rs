use std::sync::Arc;

use pyn_core::contract::RateLimits;
use pyn_postgres::PgMetadataStore;

async fn postgres() -> RateLimits {
    let url = std::env::var("PYN_DATABASE_URL").expect("PYN_DATABASE_URL must be set");
    Arc::new(
        PgMetadataStore::connect_in_scratch_schema(&url)
            .await
            .unwrap(),
    )
}

pyn_core::rate_limit_contract_tests!(postgres(), #[ignore = "needs PYN_DATABASE_URL"]);
