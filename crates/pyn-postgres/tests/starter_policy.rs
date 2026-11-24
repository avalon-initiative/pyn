use std::sync::Arc;

use pyn_core::contract::{Access, Store};
use pyn_postgres::PgMetadataStore;

async fn pg() -> (Store, Access) {
    let url = std::env::var("PYN_DATABASE_URL").expect("PYN_DATABASE_URL must be set");
    let store = Arc::new(
        PgMetadataStore::connect_in_scratch_schema(&url)
            .await
            .unwrap(),
    );
    (store.clone(), store)
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs PYN_DATABASE_URL"]
async fn a_starter_creation_commits_the_policy_file_first() {
    let (meta, access) = pg().await;
    pyn_core::contract::a_starter_creation_commits_the_policy_file_first(meta, access).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs PYN_DATABASE_URL"]
async fn creation_without_a_starter_leaves_the_repository_empty() {
    let (meta, access) = pg().await;
    pyn_core::contract::creation_without_a_starter_leaves_the_repository_empty(meta, access).await;
}
