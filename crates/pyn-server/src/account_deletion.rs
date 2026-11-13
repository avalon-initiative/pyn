//! The server's deletion task: purges accounts whose recovery window ended or that were disabled too long.

use std::sync::Arc;
use std::time::Duration;

use pyn_core::AccessService;

/// One pass: runs the sweep and logs what it did.
pub async fn run_once(access: &AccessService) {
    match access.run_deletion_sweep().await {
        Ok(sweep) => {
            for user in &sweep.purged {
                tracing::info!(%user, "account deleted");
            }
            for (user, why) in &sweep.blocked {
                tracing::warn!(%user, %why, "account deletion held back");
            }
            for user in &sweep.dropped {
                tracing::info!(%user, "dropped a deletion schedule for an account that is no longer disabled");
            }
            if sweep.names_released > 0 {
                tracing::info!(count = sweep.names_released, "released name reservations");
            }
        }
        Err(e) => tracing::error!(error = %e, "account deletion pass failed"),
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
