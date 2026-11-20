//! Records the end of access sessions whose hour ran out, so the audit logs show it without anyone asking.

use std::sync::Arc;
use std::time::Duration;

use pyn_core::AccessService;

/// One pass: records each expiry once.
pub async fn run_once(access: &AccessService) {
    match access.finish_expired_access_sessions().await {
        Ok(0) => {}
        Ok(n) => tracing::info!(count = n, "recorded the end of expired access sessions"),
        Err(e) => tracing::error!(error = %e, "access session expiry pass failed"),
    }
}

/// Runs a pass now and then every `every`. State lives in the store, so a restart loses nothing.
pub fn spawn(access: Arc<AccessService>, every: Duration) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(every);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            run_once(&access).await;
        }
    });
}
