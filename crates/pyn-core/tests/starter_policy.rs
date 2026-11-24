use std::sync::Arc;

use pyn_core::contract::{Access, Store};
use pyn_core::memory::{MemoryAccessStore, MemoryMetadataStore};

fn memory() -> (Store, Access) {
    (
        Arc::new(MemoryMetadataStore::new()),
        Arc::new(MemoryAccessStore::new()),
    )
}

#[tokio::test]
async fn a_starter_creation_commits_the_policy_file_first() {
    let (meta, access) = memory();
    pyn_core::contract::a_starter_creation_commits_the_policy_file_first(meta, access).await;
}

#[tokio::test]
async fn creation_without_a_starter_leaves_the_repository_empty() {
    let (meta, access) = memory();
    pyn_core::contract::creation_without_a_starter_leaves_the_repository_empty(meta, access).await;
}
