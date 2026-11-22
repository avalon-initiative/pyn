//! The server's sweep of lapsed resumable LFS uploads and the pieces they stored.

use std::sync::Arc;
use std::time::Duration;

use pyn_core::Repositories;
use tokio::task::JoinHandle;

use crate::shutdown::Drain;

/// Runs a sweep now and then every `every` until draining starts. Sessions live in the store, so a restart loses nothing.
pub fn spawn(repos: Arc<Repositories>, every: Duration, drain: Drain) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(every);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        while drain.tick(&mut tick).await {
            match repos.sweep_tus_sessions().await {
                Ok(0) => {}
                Ok(n) => tracing::info!(count = n, "dropped lapsed resumable uploads"),
                Err(e) => tracing::error!(error = %e, "resumable upload sweep failed"),
            }
        }
    })
}
