use std::sync::Arc;

use pyn_core::contract::Access;
use pyn_core::memory::MemoryAccessStore;

async fn memory() -> Access {
    Arc::new(MemoryAccessStore::new())
}

pyn_core::access_contract_tests!(memory());
