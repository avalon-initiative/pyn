use std::sync::Arc;

use pyn_core::contract::Webhooks;
use pyn_core::memory::MemoryWebhookStore;

async fn memory() -> Webhooks {
    Arc::new(MemoryWebhookStore::new())
}

pyn_core::webhook_contract_tests!(memory());
