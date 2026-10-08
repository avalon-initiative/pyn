use std::sync::Arc;

use pyn_core::contract::Access;
use pyn_postgres::PgMetadataStore;

async fn postgres() -> Access {
    let url = std::env::var("PYN_DATABASE_URL").expect("PYN_DATABASE_URL must be set");
    Arc::new(
        PgMetadataStore::connect_in_scratch_schema(&url)
            .await
            .unwrap(),
    )
}

pyn_core::access_contract_tests!(postgres(), #[ignore = "needs PYN_DATABASE_URL"]);
