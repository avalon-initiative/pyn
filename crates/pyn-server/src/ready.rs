//! `GET /readyz`: whether this copy should receive traffic. `/healthz` stays the liveness check.

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use pyn_proto as api;
use tokio::sync::Mutex;
use tokio::time::Instant;

use crate::AppState;
use crate::shutdown::Drain;

/// How long a metadata store answer is reused, so a burst of health checks costs one query.
const CACHE_FOR: Duration = Duration::from_secs(3);
/// How long the metadata store has to answer.
const QUERY_TIMEOUT: Duration = Duration::from_secs(2);

/// When the metadata store was last asked, and why it failed (`None` when it answered).
type Answer = (Instant, Option<String>);

/// The draining flag and the cached metadata store check. Clones share both.
#[derive(Clone, Default)]
pub struct Readiness {
    pub drain: Drain,
    cached: Arc<Mutex<Option<Answer>>>,
}

impl Readiness {
    pub fn new(drain: Drain) -> Self {
        Self {
            drain,
            cached: Arc::default(),
        }
    }

    /// `None` when the store answered, else why not. One caller at a time refreshes a stale answer.
    async fn metadata(
        &self,
        query: impl std::future::Future<Output = Result<(), String>>,
    ) -> Option<String> {
        let mut cached = self.cached.lock().await;
        if let Some((at, answer)) = cached.as_ref()
            && at.elapsed() < CACHE_FOR
        {
            return answer.clone();
        }
        let answer = match tokio::time::timeout(QUERY_TIMEOUT, query).await {
            Ok(Ok(())) => None,
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "readiness: the metadata store query failed");
                Some("the query failed".to_string())
            }
            Err(_) => {
                tracing::warn!("readiness: the metadata store query timed out");
                Some(format!(
                    "no answer within {} seconds",
                    QUERY_TIMEOUT.as_secs()
                ))
            }
        };
        *cached = Some((Instant::now(), answer.clone()));
        answer
    }
}

fn check(name: &str, failure: Option<String>) -> api::ReadinessCheck {
    api::ReadinessCheck {
        name: name.into(),
        status: if failure.is_some() {
            api::ReadinessCheckStatus::Failing
        } else {
            api::ReadinessCheckStatus::Ok
        },
        detail: failure,
    }
}

/// 200 when the metadata store answers and the object store's last probe passed; 503 otherwise, and 503 `draining` during shutdown.
#[utoipa::path(get, path = "/readyz", responses(
    (status = 200, body = api::ReadinessReport, description = "ready for traffic"),
    (status = 503, body = api::ReadinessReport, description = "a check failed, or the server is draining")))]
pub(crate) async fn readyz(State(s): State<AppState>) -> (StatusCode, Json<api::ReadinessReport>) {
    if s.ready.drain.is_draining() {
        let report = api::ReadinessReport {
            status: api::ReadinessStatus::Draining,
            checks: Vec::new(),
        };
        return (StatusCode::SERVICE_UNAVAILABLE, Json(report));
    }
    let metadata = s
        .ready
        .metadata(async {
            s.repos
                .lock_events_head()
                .await
                .map(|_| ())
                .map_err(|e| e.to_string())
        })
        .await;
    let degraded = s.storage.settings().status == api::StorageStatus::Degraded;
    let checks = vec![
        check("metadata_store", metadata),
        check(
            "object_store",
            degraded.then(|| "the last probe of the store failed".to_string()),
        ),
    ];
    let ok = checks
        .iter()
        .all(|c| c.status == api::ReadinessCheckStatus::Ok);
    let report = api::ReadinessReport {
        status: if ok {
            api::ReadinessStatus::Ok
        } else {
            api::ReadinessStatus::Unavailable
        },
        checks,
    };
    let code = if ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (code, Json(report))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[tokio::test(start_paused = true)]
    async fn an_answer_is_reused_until_it_goes_stale() {
        let ready = Readiness::default();
        let calls = AtomicUsize::new(0);
        let ask = |ok: bool| {
            let calls = &calls;
            let ready = ready.clone();
            async move {
                ready
                    .metadata(async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        if ok { Ok(()) } else { Err("down".into()) }
                    })
                    .await
            }
        };
        assert_eq!(ask(true).await, None);
        assert_eq!(ask(false).await, None, "still the cached answer");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        tokio::time::advance(CACHE_FOR + Duration::from_millis(1)).await;
        assert_eq!(ask(false).await.as_deref(), Some("the query failed"));
        assert_eq!(ask(true).await.as_deref(), Some("the query failed"));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn a_query_that_never_answers_is_a_timeout() {
        let ready = Readiness::default();
        let answer = ready.metadata(std::future::pending()).await;
        assert_eq!(answer.as_deref(), Some("no answer within 2 seconds"));
    }
}
