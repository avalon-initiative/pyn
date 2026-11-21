use std::sync::Arc;

use pyn_core::ObjectStore;
use pyn_core::memory::MemoryObjectStore;

async fn memory() -> (Arc<dyn ObjectStore>, ()) {
    (Arc::new(MemoryObjectStore::new()), ())
}

pyn_core::object_store_contract_tests!(memory());
