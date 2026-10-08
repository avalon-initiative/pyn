use std::sync::Arc;

use pyn_core::contract::Audit;
use pyn_core::memory::MemoryAuditStore;

async fn memory() -> Audit {
    Arc::new(MemoryAuditStore::new())
}

pyn_core::audit_contract_tests!(memory());
