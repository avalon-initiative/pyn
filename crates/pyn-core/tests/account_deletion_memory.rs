use std::sync::Arc;

use pyn_core::contract::{Access, Audit, Store};
use pyn_core::memory::{MemoryAccessStore, MemoryAuditStore, MemoryMetadataStore};

async fn access() -> Access {
    Arc::new(MemoryAccessStore::new())
}

async fn audit() -> Audit {
    Arc::new(MemoryAuditStore::new())
}

async fn meta() -> Store {
    Arc::new(MemoryMetadataStore::new())
}

pyn_core::account_deletion_contract_tests!(access());
pyn_core::author_lookup_contract_tests!(meta());
pyn_core::actor_lookup_contract_tests!(audit());
