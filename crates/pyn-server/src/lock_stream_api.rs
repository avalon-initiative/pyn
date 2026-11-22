//! A repository's lock changes as a cursor long-poll, and the sweep that makes lapsed leases raise `lock.expired`.

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::{Query, State};
use pyn_core::{OutboxEvent, PynError, Repositories};
use pyn_proto as api;
use serde::Deserialize;
use tokio::task::JoinHandle;

use crate::caller::RepoReader;
use crate::shutdown::Drain;
use crate::{ApiResult, AppState, RepoAddress};

const MAX_WAIT_SECS: u64 = 25;
const DEFAULT_LIMIT: u32 = 100;
const MAX_LIMIT: u32 = 500;
const POLL_EVERY: Duration = Duration::from_millis(500);

#[derive(Deserialize)]
pub(crate) struct StreamQuery {
    after: Option<String>,
    wait: Option<u64>,
    limit: Option<u32>,
}

fn cursor_of(raw: &str) -> Result<i64, PynError> {
    raw.parse::<i64>()
        .ok()
        .filter(|n| *n >= 0)
        .ok_or_else(|| PynError::InvalidRequest(format!("{raw:?} is not a lock stream cursor")))
}

fn item(event: OutboxEvent) -> ApiResult<api::LockStreamEvent> {
    let parsed = serde_json::from_str(&event.payload)
        .map_err(|e| PynError::Storage(format!("lock event {}: {e}", event.id)))?;
    Ok(api::LockStreamEvent {
        cursor: event.id.to_string(),
        event: parsed,
    })
}

/// Lock changes of the repository after a cursor, oldest first. Waits up to `wait` seconds for one when there is none, or less when the server starts shutting down.
#[utoipa::path(get, path = "/v1/repos/{owner}/{name}/locks/events", operation_id = "lock_events",
    params(RepoAddress,
        ("after" = Option<String>, Query, description = "cursor of the last event seen; without it the stream starts now and returns no events"),
        ("wait" = Option<u64>, Query, description = "seconds to hold the request open when nothing is new, default 0, at most 25"),
        ("limit" = Option<u32>, Query, description = "most events returned, default 100, at most 500")),
    responses((status = 200, body = api::LockStreamPage),
              (status = 400, body = api::ErrorBody, description = "invalid_request: the cursor is not one this server issued")))]
pub(crate) async fn lock_events(
    State(s): State<AppState>,
    RepoReader { repo, .. }: RepoReader,
    Query(q): Query<StreamQuery>,
) -> ApiResult<Json<api::LockStreamPage>> {
    let Some(raw) = q.after.as_deref() else {
        let head = s.repos.lock_events_head().await?;
        return Ok(Json(api::LockStreamPage {
            events: Vec::new(),
            cursor: head.to_string(),
        }));
    };
    let after = cursor_of(raw)?;
    let limit = q.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let deadline =
        tokio::time::Instant::now() + Duration::from_secs(q.wait.unwrap_or(0).min(MAX_WAIT_SECS));
    loop {
        let found = s
            .repos
            .lock_events_after(&repo.record, after, limit)
            .await?;
        if !found.is_empty()
            || tokio::time::Instant::now() >= deadline
            || s.ready.drain.is_draining()
        {
            let events = found.into_iter().map(item).collect::<ApiResult<Vec<_>>>()?;
            let cursor = events
                .last()
                .map_or_else(|| raw.to_string(), |e| e.cursor.clone());
            return Ok(Json(api::LockStreamPage { events, cursor }));
        }
        // The wait ends early once the server drains, so the client reconnects instead of holding the shutdown.
        s.ready.drain.sleep(POLL_EVERY).await;
    }
}

/// Removes lapsed leases every `every`, raising `lock.expired` for each so a stream shows the expiry when it happens.
pub fn spawn_sweep(repos: Arc<Repositories>, every: Duration, drain: Drain) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(every);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        while drain.tick(&mut tick).await {
            if let Err(e) = repos.sweep_expired_locks().await {
                tracing::error!(error = %e, "lock expiry sweep failed");
            }
        }
    })
}
