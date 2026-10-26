use std::sync::Arc;

use pyn_core::contract::{Access, Store, Webhooks};
use pyn_core::memory::{MemoryAccessStore, MemoryMetadataStore, MemoryWebhookStore};

fn memory() -> (Store, Access, Webhooks) {
    let hooks = Arc::new(MemoryWebhookStore::new());
    (
        Arc::new(MemoryMetadataStore::new().with_outbox(hooks.clone())),
        Arc::new(MemoryAccessStore::new().with_outbox(hooks.clone())),
        hooks,
    )
}

#[tokio::test]
async fn changes_queue_their_events_in_order() {
    let (meta, access, hooks) = memory();
    pyn_core::contract::changes_queue_their_events_in_order(meta, access, hooks).await;
}

#[tokio::test]
async fn refused_changes_queue_no_event() {
    let (meta, access, hooks) = memory();
    pyn_core::contract::refused_changes_queue_no_event(meta, access, hooks).await;
}
