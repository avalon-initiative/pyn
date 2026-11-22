//! Structured logging: the log format, request ids, the access log and the optional audit log lines.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::str::FromStr;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use async_trait::async_trait;
use axum::Extension;
use axum::extract::rejection::RawPathParamsRejection;
use axum::extract::{MatchedPath, RawPathParams, Request};
use axum::http::{HeaderValue, header};
use axum::middleware::Next;
use axum::response::Response;
use http_body::Body as _;
use pyn_core::{AuditEvent, AuditQuery, AuditScope, AuditStore, NewAuditEvent, Result, UserId};
use tracing::Instrument;
use tracing_subscriber::fmt::MakeWriter;

use crate::AppState;
use crate::client::Client;

pub const REQUEST_ID_HEADER: &str = "x-request-id";
const MAX_REQUEST_ID: usize = 128;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LogFormat {
    #[default]
    Text,
    Json,
}

impl FromStr for LogFormat {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, String> {
        match s {
            "text" => Ok(Self::Text),
            "json" => Ok(Self::Json),
            _ => Err("PYN_LOG_FORMAT must be text or json".into()),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct LogConfig {
    pub access: bool,
    pub client: bool,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            access: true,
            client: false,
        }
    }
}

/// A subscriber writing `format` lines to `writer`; json is one flat object per line.
pub fn subscriber<W>(
    format: LogFormat,
    filter: tracing_subscriber::EnvFilter,
    writer: W,
) -> Box<dyn tracing::Subscriber + Send + Sync>
where
    W: for<'w> MakeWriter<'w> + Send + Sync + 'static,
{
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(writer);
    match format {
        LogFormat::Text => Box::new(builder.finish()),
        LogFormat::Json => Box::new(builder.json().flatten_event(true).finish()),
    }
}

/// Visible ASCII only, bounded, so a caller cannot smuggle control characters into the logs.
fn well_formed(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_REQUEST_ID && id.bytes().all(|b| (0x21..=0x7e).contains(&b))
}

fn generate_id() -> String {
    let hasher = RandomState::new();
    let mut out = String::with_capacity(32);
    for salt in [1u64, 2] {
        let mut h = hasher.build_hasher();
        h.write_u64(salt);
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out
}

tokio::task_local! {
    static ACCOUNT: Arc<OnceLock<String>>;
}

/// Records the signed-in account for the access log of the request in flight.
pub(crate) fn note_account(user: &UserId) {
    let _ = ACCOUNT.try_with(|slot| {
        let _ = slot.set(user.to_string());
    });
}

/// What the matched route tells the access log: its template and the repository it names.
#[derive(Clone)]
struct RouteInfo {
    template: String,
    repo: Option<String>,
}

/// Runs inside the router so the matched route is known; passes it outward on the response.
pub(crate) async fn route_info(
    params: Result<RawPathParams, RawPathParamsRejection>,
    request: Request,
    next: Next,
) -> Response {
    let repo = params.ok().and_then(|p| {
        let get = |key| {
            p.iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.to_string())
        };
        Some(format!("{}/{}", get("owner")?, get("name")?))
    });
    let template = request
        .extensions()
        .get::<MatchedPath>()
        .map_or_else(|| "unmatched".into(), |m| m.as_str().to_string());
    let mut response = next.run(request).await;
    response
        .extensions_mut()
        .insert(RouteInfo { template, repo });
    response
}

/// Outermost layer: the request id, its span and the access log line (not for `/healthz` and `/readyz`, which health checks poll); `LogConfig` comes from an `Extension` layer.
pub(crate) async fn request_log(
    Extension(config): Extension<LogConfig>,
    client: Client,
    request: Request,
    next: Next,
) -> Response {
    let supplied = request
        .headers()
        .get(REQUEST_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .filter(|v| well_formed(v));
    let id = supplied.map_or_else(generate_id, str::to_string);
    let method = request.method().clone();
    let probe = matches!(request.uri().path(), "/healthz" | "/readyz");
    let user_agent = (config.access && config.client)
        .then(|| {
            request
                .headers()
                .get(header::USER_AGENT)
                .and_then(|v| v.to_str().ok())
                .map(|v| v.chars().take(256).collect::<String>())
        })
        .flatten();
    let span = tracing::info_span!("request", request_id = %id);
    let slot = Arc::new(OnceLock::new());
    let started = Instant::now();
    let mut response = ACCOUNT
        .scope(slot.clone(), next.run(request))
        .instrument(span.clone())
        .await;
    let latency_ms = started.elapsed().as_millis() as u64;
    if config.access && !probe {
        let route = response.extensions_mut().remove::<RouteInfo>();
        let bytes = response.body().size_hint().exact();
        let client_ip = config.client.then_some(client.0).flatten();
        span.in_scope(|| {
            tracing::info!(
                target: "access",
                request_id = %id,
                %method,
                route = route.as_ref().map_or("unmatched", |r| r.template.as_str()),
                status = response.status().as_u16(),
                latency_ms,
                response_bytes = bytes,
                account = slot.get().map(String::as_str),
                repo = route.as_ref().and_then(|r| r.repo.as_deref()),
                client_ip = client_ip.as_deref(),
                user_agent = user_agent.as_deref(),
                "request"
            );
        });
    }
    if let Ok(value) = HeaderValue::from_str(&id) {
        response.headers_mut().insert(REQUEST_ID_HEADER, value);
    }
    response
}

/// Writes each recorded audit event as a log line with target `audit`: scope, actor, action and path, never the detail.
pub struct LoggedAudit(pub Arc<dyn AuditStore>);

#[async_trait]
impl AuditStore for LoggedAudit {
    async fn record(&self, scope: &AuditScope, event: NewAuditEvent) -> Result<()> {
        let line = (
            scope.key(),
            event.actor.to_string(),
            event.action.to_string(),
            event.path.as_ref().map(ToString::to_string),
        );
        self.0.record(scope, event).await?;
        tracing::info!(
            target: "audit",
            scope = %line.0,
            actor = %line.1,
            action = %line.2,
            path = line.3.as_deref(),
            "audit"
        );
        Ok(())
    }

    async fn list(&self, scope: &AuditScope, query: &AuditQuery) -> Result<Vec<AuditEvent>> {
        self.0.list(scope, query).await
    }

    async fn actor_appears(&self, actor: &UserId) -> Result<bool> {
        self.0.actor_appears(actor).await
    }
}

/// Puts the request id and access log around `router`.
pub(crate) fn layer(router: axum::Router, state: AppState, config: LogConfig) -> axum::Router {
    router
        .layer(axum::middleware::from_fn_with_state(state, request_log))
        .layer(Extension(config))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_id_shape() {
        assert!(well_formed("abc-123_X.y"));
        assert!(!well_formed(""));
        assert!(!well_formed("has space"));
        assert!(!well_formed("tab\t"));
        assert!(!well_formed("caf\u{e9}"));
        assert!(!well_formed(&"a".repeat(MAX_REQUEST_ID + 1)));
        assert!(well_formed(&"a".repeat(MAX_REQUEST_ID)));
        let id = generate_id();
        assert!(well_formed(&id) && id.len() == 32);
        assert_ne!(id, generate_id());
    }

    #[test]
    fn format_parses() {
        assert_eq!("json".parse(), Ok(LogFormat::Json));
        assert_eq!(LogFormat::default(), LogFormat::Text);
        assert!(
            "JSON"
                .parse::<LogFormat>()
                .unwrap_err()
                .contains("PYN_LOG_FORMAT")
        );
    }
}
