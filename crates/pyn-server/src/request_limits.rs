//! Server-wide bounds on a request: a time limit for ordinary routes, an idle limit on streamed bodies and a cap on
//! how many costly reads run at once.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::BoxError;
use axum::Router;
use axum::body::{Body, Bytes, HttpBody};
use axum::extract::{Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header::RETRY_AFTER};
use axum::middleware::{Next, from_fn_with_state};
use axum::response::{IntoResponse, Response};
use http_body::{Frame, SizeHint};
use pyn_proto as api;
use tokio::sync::Semaphore;
use tokio::time::{Instant, Sleep};

pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
pub const DEFAULT_BODY_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct RequestLimits {
    /// Time an ordinary route has to answer; `None` for no limit.
    pub request_timeout: Option<Duration>,
    /// Time a streamed upload body may sit without sending a byte; `None` for no limit.
    pub body_idle_timeout: Option<Duration>,
    /// Requests to the costly read routes that run at once.
    pub max_expensive: usize,
}

impl Default for RequestLimits {
    fn default() -> Self {
        let cores = std::thread::available_parallelism().map_or(2, |n| n.get());
        Self {
            request_timeout: Some(DEFAULT_REQUEST_TIMEOUT),
            body_idle_timeout: Some(DEFAULT_BODY_IDLE_TIMEOUT),
            max_expensive: cores * 4,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Plain,
    Timed,
    Expensive,
    Streaming,
}

/// Where a route falls: streamed bodies get an idle limit, reads get a time limit, and some reads are also capped.
fn classify(method: &Method, path: &str) -> Class {
    let Some(rest) = path.strip_prefix("/v1/repos/") else {
        return Class::Plain;
    };
    let mut parts = rest.split('/').skip(2);
    let (first, second) = (parts.next(), parts.next());
    let get = method == Method::GET;
    let post = method == Method::POST;
    if (first, second) == (Some("info"), Some("lfs")) {
        return match (parts.next(), parts.next()) {
            (Some("objects"), Some(_)) if get || method == Method::PUT => Class::Streaming,
            _ if get => Class::Timed,
            _ => Class::Plain,
        };
    }
    match (first, second) {
        (Some("objects"), None) if method == Method::PUT => Class::Streaming,
        (Some("objects"), Some("chunk")) if method == Method::PUT => Class::Streaming,
        (Some("objects"), Some("assemble")) if post => Class::Streaming,
        (Some("objects"), Some(_)) if get => Class::Streaming,
        (Some("push"), None) if post => Class::Streaming,
        (Some("fetch"), None) if post => Class::Expensive,
        (Some("merge"), Some("preview")) if post => Class::Expensive,
        (Some("history" | "verify" | "snapshot" | "summary"), None) if get => Class::Expensive,
        (Some("policy"), Some("preview")) if post => Class::Timed,
        (Some("objects"), Some("missing")) if post => Class::Timed,
        (_, _) if get => Class::Timed,
        _ => Class::Plain,
    }
}

struct Shared {
    limits: RequestLimits,
    expensive: Semaphore,
}

fn refusal(code: &str, message: &str) -> Response {
    let body = api::ErrorBody {
        code: code.into(),
        message: message.into(),
    };
    let mut res = (StatusCode::SERVICE_UNAVAILABLE, axum::Json(body)).into_response();
    res.headers_mut().insert(RETRY_AFTER, HeaderValue::from(1));
    res
}

async fn limit(State(shared): State<Arc<Shared>>, request: Request, next: Next) -> Response {
    let class = classify(request.method(), request.uri().path());
    let limits = &shared.limits;
    match class {
        Class::Plain => next.run(request).await,
        Class::Streaming => {
            next.run(idle_limited(request, limits.body_idle_timeout))
                .await
        }
        Class::Timed | Class::Expensive => {
            let _permit = if class == Class::Expensive {
                let wait = shared.expensive.acquire();
                let permit = match limits.request_timeout {
                    Some(t) => tokio::time::timeout(t, wait).await.ok(),
                    None => Some(wait.await),
                };
                match permit {
                    Some(permit) => Some(permit.expect("the semaphore is never closed")),
                    None => return refusal("server_busy", "the server is busy; retry shortly"),
                }
            } else {
                None
            };
            match limits.request_timeout {
                Some(t) => match tokio::time::timeout(t, next.run(request)).await {
                    Ok(response) => response,
                    Err(_) => refusal("request_timeout", "the request took too long"),
                },
                None => next.run(request).await,
            }
        }
    }
}

/// A body that fails when no frame arrives for `idle`; sizes are the wrapped body's.
struct IdleBody {
    inner: Body,
    idle: Duration,
    timer: Pin<Box<Sleep>>,
}

impl HttpBody for IdleBody {
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, BoxError>>> {
        let this = self.get_mut();
        match Pin::new(&mut this.inner).poll_frame(cx) {
            Poll::Ready(frame) => {
                this.timer.as_mut().reset(Instant::now() + this.idle);
                Poll::Ready(frame.map(|r| r.map_err(Into::into)))
            }
            Poll::Pending => match this.timer.as_mut().poll(cx) {
                Poll::Ready(()) => Poll::Ready(Some(Err(Box::new(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "the upload stalled",
                ))))),
                Poll::Pending => Poll::Pending,
            },
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

fn idle_limited(request: Request, idle: Option<Duration>) -> Request {
    let Some(idle) = idle else { return request };
    let (parts, inner) = request.into_parts();
    let timer = Box::pin(tokio::time::sleep(idle));
    let body = IdleBody { inner, idle, timer };
    Request::from_parts(parts, Body::new(body))
}

/// Wraps `router` in the limits.
pub fn apply(router: Router, limits: RequestLimits) -> Router {
    let shared = Arc::new(Shared {
        expensive: Semaphore::new(limits.max_expensive.max(1)),
        limits,
    });
    router.layer(from_fn_with_state(shared, limit))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::routing::{get, post, put};
    use tower::ServiceExt;

    use super::*;

    const REPO: &str = "/v1/repos/o/n";

    #[test]
    fn routes_fall_in_their_classes() {
        let at = |m: Method, p: &str| classify(&m, &format!("{REPO}/{p}"));
        assert!(at(Method::PUT, "objects") == Class::Streaming);
        assert!(at(Method::PUT, "objects/chunk") == Class::Streaming);
        assert!(at(Method::POST, "objects/assemble") == Class::Streaming);
        assert!(at(Method::GET, "objects/abc") == Class::Streaming);
        assert!(at(Method::POST, "push") == Class::Streaming);
        assert!(at(Method::PUT, "info/lfs/objects/abc") == Class::Streaming);
        assert!(at(Method::GET, "info/lfs/objects/abc") == Class::Streaming);
        assert!(at(Method::POST, "info/lfs/objects/batch") == Class::Plain);
        assert!(at(Method::GET, "info/lfs/locks") == Class::Timed);
        assert!(at(Method::POST, "fetch") == Class::Expensive);
        assert!(at(Method::POST, "merge/preview") == Class::Expensive);
        assert!(at(Method::GET, "history") == Class::Expensive);
        assert!(at(Method::GET, "verify") == Class::Expensive);
        assert!(at(Method::GET, "snapshot") == Class::Expensive);
        assert!(at(Method::GET, "summary") == Class::Expensive);
        assert!(at(Method::GET, "files") == Class::Timed);
        assert!(at(Method::POST, "objects/missing") == Class::Timed);
        assert!(at(Method::POST, "merge") == Class::Plain);
        assert!(at(Method::POST, "checkin") == Class::Plain);
        assert!(classify(&Method::GET, "/healthz") == Class::Plain);
    }

    fn limits(timeout_ms: u64, max: usize) -> RequestLimits {
        RequestLimits {
            request_timeout: Some(Duration::from_millis(timeout_ms)),
            body_idle_timeout: Some(Duration::from_millis(100)),
            max_expensive: max,
        }
    }

    async fn status(app: &Router, method: Method, path: &str) -> StatusCode {
        let request = Request::builder()
            .method(method)
            .uri(path)
            .body(Body::empty())
            .unwrap();
        app.clone().oneshot(request).await.unwrap().status()
    }

    async fn slow() -> &'static str {
        tokio::time::sleep(Duration::from_millis(300)).await;
        "done"
    }

    #[tokio::test]
    async fn a_slow_ordinary_route_is_answered_with_a_timeout() {
        let app = apply(
            Router::new()
                .route("/v1/repos/o/n/files", get(slow))
                .route("/v1/repos/o/n/checkin", post(slow))
                .route("/v1/repos/o/n/objects", put(slow)),
            limits(50, 8),
        );
        let files = status(&app, Method::GET, "/v1/repos/o/n/files").await;
        assert_eq!(files, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            status(&app, Method::POST, "/v1/repos/o/n/checkin").await,
            StatusCode::OK,
            "writes are not cut off"
        );
        assert_eq!(
            status(&app, Method::PUT, "/v1/repos/o/n/objects").await,
            StatusCode::OK,
            "streamed routes have no time limit"
        );
    }

    #[tokio::test]
    async fn no_timeout_leaves_a_slow_route_alone() {
        let mut none = limits(50, 8);
        none.request_timeout = None;
        let app = apply(Router::new().route("/v1/repos/o/n/files", get(slow)), none);
        assert_eq!(
            status(&app, Method::GET, "/v1/repos/o/n/files").await,
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn costly_reads_run_at_most_the_configured_number_at_once() {
        static RUNNING: AtomicUsize = AtomicUsize::new(0);
        static PEAK: AtomicUsize = AtomicUsize::new(0);
        async fn counted() -> &'static str {
            let now = RUNNING.fetch_add(1, Ordering::SeqCst) + 1;
            PEAK.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(40)).await;
            RUNNING.fetch_sub(1, Ordering::SeqCst);
            "ok"
        }
        let app = apply(
            Router::new().route("/v1/repos/o/n/summary", get(counted)),
            limits(5000, 2),
        );
        let calls = (0..8).map(|_| status(&app, Method::GET, "/v1/repos/o/n/summary"));
        for s in futures_util::future::join_all(calls).await {
            assert_eq!(s, StatusCode::OK);
        }
        assert_eq!(PEAK.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_request_that_cannot_get_a_slot_in_time_is_refused() {
        let app = apply(
            Router::new().route("/v1/repos/o/n/summary", get(slow)),
            limits(100, 1),
        );
        let calls = (0..2).map(|_| status(&app, Method::GET, "/v1/repos/o/n/summary"));
        let mut got = futures_util::future::join_all(calls).await;
        got.sort_by_key(|s| s.as_u16());
        assert_eq!(got, [StatusCode::SERVICE_UNAVAILABLE; 2]);
    }

    #[tokio::test]
    async fn a_stalled_upload_body_is_cut_off() {
        async fn drain(body: Body) -> StatusCode {
            match axum::body::to_bytes(body, usize::MAX).await {
                Ok(_) => StatusCode::OK,
                Err(_) => StatusCode::BAD_REQUEST,
            }
        }
        let app = apply(
            Router::new().route("/v1/repos/o/n/objects", put(drain)),
            limits(5000, 2),
        );
        let stalled = futures_util::stream::once(async {
            tokio::time::sleep(Duration::from_millis(600)).await;
            Ok::<_, std::io::Error>(axum::body::Bytes::from_static(b"late"))
        });
        let request = Request::put("/v1/repos/o/n/objects")
            .body(Body::from_stream(stalled))
            .unwrap();
        let res = app.clone().oneshot(request).await.unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);

        let steady = futures_util::stream::iter([Ok::<_, std::io::Error>(
            axum::body::Bytes::from_static(b"quick"),
        )]);
        let request = Request::put("/v1/repos/o/n/objects")
            .body(Body::from_stream(steady))
            .unwrap();
        let res = app.oneshot(request).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }
}
