use std::sync::Arc;

use pyn_core::contract::{Access, Store, Webhooks};
use pyn_core::memory::{MemoryAccessStore, MemoryMetadataStore, MemoryWebhookStore};

fn memory() -> (Store, Access, Webhooks) {
    let access = MemoryAccessStore::new();
    let hooks = Arc::new(MemoryWebhookStore::new().with_watchers(&access));
    (
        Arc::new(MemoryMetadataStore::new().with_outbox(hooks.clone())),
        Arc::new(access.with_outbox(hooks.clone())),
        hooks,
    )
}

#[tokio::test]
async fn changes_queue_their_events_in_order() {
    let (meta, access, hooks) = memory();
    pyn_core::contract::changes_queue_their_events_in_order(meta, access, hooks).await;
}

#[tokio::test]
async fn lock_changes_queue_their_events() {
    let (meta, access, hooks) = memory();
    pyn_core::contract::lock_changes_queue_their_events(meta, access, hooks).await;
}

#[tokio::test]
async fn lock_events_read_back_by_repository_and_cursor() {
    let (meta, access, hooks) = memory();
    pyn_core::contract::lock_events_read_back_by_repository_and_cursor(meta, access, hooks).await;
}

#[tokio::test]
async fn refused_changes_queue_no_event() {
    let (meta, access, hooks) = memory();
    pyn_core::contract::refused_changes_queue_no_event(meta, access, hooks).await;
}

#[tokio::test]
async fn watchers_are_queued_the_events_their_level_asks_for() {
    let (meta, access, hooks) = memory();
    pyn_core::contract::watchers_are_queued_the_events_their_level_asks_for(meta, access, hooks)
        .await;
}

#[tokio::test]
async fn revoked_access_stops_notifications() {
    let (meta, access, hooks) = memory();
    pyn_core::contract::revoked_access_stops_notifications(meta, access, hooks).await;
}
