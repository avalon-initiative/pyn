//! HTTP API over `pyn_core::RepoService`. Handlers translate; they hold no policy.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use pyn_core::{
    AuthProvider, ContentHash, ObjectStore, PynError, RepoPath, RepoService, RevisionId, UserId,
};
use pyn_proto as api;
use serde::Deserialize;
use utoipa::OpenApi;

pub mod auth;

#[derive(Clone)]
pub struct AppState {
    pub service: Arc<RepoService>,
    pub objects: Arc<dyn ObjectStore>,
    pub auth: Arc<dyn AuthProvider>,
}

#[derive(OpenApi)]
#[openapi(
    paths(health, list_locks, checkout, release, checkin, put_object, history),
    components(schemas(
        api::Lock,
        api::Revision,
        api::CheckoutRequest,
        api::ReleaseRequest,
        api::CheckinRequest,
        api::PutObjectResponse,
        api::ErrorBody
    ))
)]
pub struct ApiDoc;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(health))
        .route("/openapi.json", get(|| async { Json(ApiDoc::openapi()) }))
        .route("/v1/locks", get(list_locks))
        .route("/v1/checkout", post(checkout))
        .route("/v1/release", post(release))
        .route("/v1/checkin", post(checkin))
        .route("/v1/objects", put(put_object))
        .route("/v1/history", get(history))
        .with_state(state)
}

struct ApiError(PynError);

impl From<PynError> for ApiError {
    fn from(e: PynError) -> Self {
        Self(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match &self.0 {
            PynError::LockHeld { .. } | PynError::StaleBase { .. } | PynError::LockRequired(_) => {
                StatusCode::CONFLICT
            }
            PynError::NotLockHolder(_) => StatusCode::FORBIDDEN,
            PynError::InvalidPath(_)
            | PynError::InvalidRules(_)
            | PynError::NotExclusive(_)
            | PynError::ObjectMissing(_) => StatusCode::BAD_REQUEST,
            PynError::Storage(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let body = api::ErrorBody {
            code: self.0.code().to_string(),
            message: self.0.to_string(),
        };
        (status, Json(body)).into_response()
    }
}

type ApiResult<T> = Result<T, ApiError>;

async fn caller(state: &AppState, headers: &HeaderMap) -> ApiResult<UserId> {
    let credential = headers
        .get(api::DEV_USER_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    Ok(state.auth.authenticate(credential).await?)
}

fn lock_dto(l: pyn_core::Lock) -> api::Lock {
    api::Lock {
        path: l.path.to_string(),
        owner: l.owner.to_string(),
        acquired_at: l.acquired_at,
        expires_at: l.expires_at,
    }
}

fn revision_dto(r: pyn_core::Revision) -> api::Revision {
    api::Revision {
        id: r.id.0,
        path: r.path.to_string(),
        content: r.content.to_string(),
        author: r.author.to_string(),
        message: r.message,
        created_at: r.created_at,
    }
}

#[utoipa::path(get, path = "/healthz", responses((status = 200, body = String)))]
async fn health() -> &'static str {
    "ok"
}

#[utoipa::path(get, path = "/v1/locks", responses((status = 200, body = Vec<api::Lock>)))]
async fn list_locks(State(s): State<AppState>) -> ApiResult<Json<Vec<api::Lock>>> {
    Ok(Json(
        s.service.locks().await?.into_iter().map(lock_dto).collect(),
    ))
}

#[utoipa::path(post, path = "/v1/checkout", request_body = api::CheckoutRequest, responses(
    (status = 200, body = api::Lock),
    (status = 409, body = api::ErrorBody, description = "lock_held or stale_base"),
    (status = 400, body = api::ErrorBody, description = "not_exclusive or invalid_path"),
))]
async fn checkout(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<api::CheckoutRequest>,
) -> ApiResult<Json<api::Lock>> {
    let user = caller(&s, &headers).await?;
    let lock = s
        .service
        .checkout(
            &RepoPath::new(req.path)?,
            &user,
            req.base_revision.map(RevisionId),
        )
        .await?;
    Ok(Json(lock_dto(lock)))
}

#[utoipa::path(post, path = "/v1/release", request_body = api::ReleaseRequest, responses(
    (status = 204),
    (status = 403, body = api::ErrorBody, description = "not_lock_holder"),
))]
async fn release(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<api::ReleaseRequest>,
) -> ApiResult<StatusCode> {
    let user = caller(&s, &headers).await?;
    s.service.release(&RepoPath::new(req.path)?, &user).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(post, path = "/v1/checkin", request_body = api::CheckinRequest, responses(
    (status = 200, body = api::Revision),
    (status = 409, body = api::ErrorBody, description = "lock_required, lock_held or stale_base"),
    (status = 400, body = api::ErrorBody, description = "object_missing or invalid_path"),
))]
async fn checkin(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<api::CheckinRequest>,
) -> ApiResult<Json<api::Revision>> {
    let user = caller(&s, &headers).await?;
    let rev = s
        .service
        .checkin(
            &RepoPath::new(req.path)?,
            &user,
            ContentHash::new(req.content),
            req.base_revision.map(RevisionId),
            req.message,
        )
        .await?;
    Ok(Json(revision_dto(rev)))
}

#[utoipa::path(put, path = "/v1/objects", request_body(content = Vec<u8>, content_type = "application/octet-stream"),
    responses((status = 200, body = api::PutObjectResponse)))]
async fn put_object(
    State(s): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> ApiResult<Json<api::PutObjectResponse>> {
    caller(&s, &headers).await?;
    let hash = s.objects.put(body.to_vec()).await?;
    Ok(Json(api::PutObjectResponse {
        content: hash.to_string(),
    }))
}

#[derive(Deserialize)]
struct HistoryQuery {
    path: String,
}

#[utoipa::path(get, path = "/v1/history", params(("path" = String, Query, description = "repo-relative path")),
    responses((status = 200, body = Vec<api::Revision>)))]
async fn history(
    State(s): State<AppState>,
    Query(q): Query<HistoryQuery>,
) -> ApiResult<Json<Vec<api::Revision>>> {
    let revs = s.service.history(&RepoPath::new(q.path)?).await?;
    Ok(Json(revs.into_iter().map(revision_dto).collect()))
}
