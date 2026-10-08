use std::sync::Arc;

use pyn_core::contract::Store;
use pyn_core::memory::MemoryMetadataStore;

async fn memory() -> Store {
    Arc::new(MemoryMetadataStore::new())
}

pyn_core::contract_tests!(memory());
