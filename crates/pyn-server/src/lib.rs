//! HTTP API over `pyn_core::RepoService`. Handlers translate; they hold no policy.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use pyn_core::{
    AccessService, AuditAction, AuditQuery, AuthProvider, ContentHash, ObjectStore, Permission,
    Principal, PynError, RepoPath, RepoService, RevisionId,
};
use pyn_proto as api;
use serde::Deserialize;
use utoipa::OpenApi;

mod access_api;
pub mod auth;
mod session_api;

#[derive(Clone)]
pub struct AppState {
    pub service: Arc<RepoService>,
    pub objects: Arc<dyn ObjectStore>,
    pub access: Arc<AccessService>,
    /// Authenticates bearer tokens.
    pub auth: Arc<dyn AuthProvider>,
    /// Accepts the `X-Pyn-User` header; present only when development auth is switched on.
    pub dev_auth: Option<Arc<dyn AuthProvider>>,
}

#[derive(OpenApi)]
#[openapi(
    paths(
        health,
        access_api::registration,
        access_api::register,
        access_api::login,
        session_api::sign_in,
        session_api::current,
        session_api::sign_out,
        access_api::change_password,
        access_api::add_user,
        access_api::create_invite,
        access_api::list_invites,
        access_api::revoke_invite,
        access_api::add_key,
        access_api::list_keys,
        access_api::delete_key,
        access_api::me,
        access_api::create_token,
        access_api::list_tokens,
        access_api::revoke_token,
        access_api::list_roles,
        access_api::set_role,
        access_api::list_members,
        access_api::set_member,
        list_locks,
        list_files,
        get_content,
        checkout,
        release,
        checkin,
        restore,
        force_unlock,
        audit,
        put_object,
        history
    ),
    components(schemas(
        api::Lock,
        api::Revision,
        api::CheckoutRequest,
        api::ReleaseRequest,
        api::RestoreRequest,
        api::ForceUnlockRequest,
        api::AuditEntry,
        api::AuditPage,
        api::CheckinRequest,
        api::PutObjectResponse,
        api::ErrorBody,
        api::FileEntry,
        api::FilePage,
        api::Mode,
        api::RegistrationInfo,
        api::RegisterRequest,
        api::Registered,
        api::LoginRequest,
        api::SessionInfo,
        api::ChangePasswordRequest,
        api::AddUserRequest,
        api::CreateInviteRequest,
        api::InviteInfo,
        api::CreatedInvite,
        api::AddKeyRequest,
        api::SshKeyInfo,
        api::Me,
        api::CreateTokenRequest,
        api::TokenInfo,
        api::CreatedToken,
        api::RoleGrant,
        api::SetRoleRequest,
        api::Member,
        api::SetMemberRequest
    ))
)]
pub struct ApiDoc;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(health))
        .route("/openapi.json", get(|| async { Json(ApiDoc::openapi()) }))
        .route("/v1/registration", get(access_api::registration))
        .route("/v1/register", post(access_api::register))
        .route("/v1/login", post(access_api::login))
        .route(
            "/v1/session",
            get(session_api::current)
                .post(session_api::sign_in)
                .delete(session_api::sign_out),
        )
        .route("/v1/me/password", put(access_api::change_password))
        .route("/v1/users", post(access_api::add_user))
        .route(
            "/v1/invites",
            get(access_api::list_invites).post(access_api::create_invite),
        )
        .route(
            "/v1/invites/{id}",
            axum::routing::delete(access_api::revoke_invite),
        )
        .route(
            "/v1/keys",
            get(access_api::list_keys).post(access_api::add_key),
        )
        .route(
            "/v1/keys/{id}",
            axum::routing::delete(access_api::delete_key),
        )
        .route("/v1/me", get(access_api::me))
        .route(
            "/v1/tokens",
            get(access_api::list_tokens).post(access_api::create_token),
        )
        .route(
            "/v1/tokens/{id}",
            axum::routing::delete(access_api::revoke_token),
        )
        .route("/v1/roles", get(access_api::list_roles))
        .route("/v1/roles/{role}", put(access_api::set_role))
        .route("/v1/members", get(access_api::list_members))
        .route("/v1/members/{user}", put(access_api::set_member))
        .route("/v1/locks", get(list_locks))
        .route("/v1/files", get(list_files))
        .route("/v1/content", get(get_content))
        .route("/v1/checkout", post(checkout))
        .route("/v1/release", post(release))
        .route("/v1/checkin", post(checkin))
        .route("/v1/restore", post(restore))
        .route("/v1/force-unlock", post(force_unlock))
        .route("/v1/audit", get(audit))
        .route("/v1/objects", put(put_object))
        .route("/v1/history", get(history))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            session_api::csrf_guard,
        ))
        .with_state(state)
}

pub(crate) struct ApiError(PynError);

impl From<PynError> for ApiError {
    fn from(e: PynError) -> Self {
        Self(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match &self.0 {
            PynError::LockHeld { .. }
            | PynError::StaleBase { .. }
            | PynError::LockRequired(_)
            | PynError::NotLocked(_)
            | PynError::UserExists(_)
            | PynError::KeyInUse
            | PynError::ConfirmationRequired { .. } => StatusCode::CONFLICT,
            PynError::NotLockHolder(_)
            | PynError::Forbidden(_)
            | PynError::RegistrationClosed
            | PynError::CsrfFailed => StatusCode::FORBIDDEN,
            PynError::TooManyAttempts { .. } => StatusCode::TOO_MANY_REQUESTS,
            PynError::Unauthenticated(_) => StatusCode::UNAUTHORIZED,
            PynError::TokenNotFound(_) | PynError::KeyNotFound(_) => StatusCode::NOT_FOUND,
            PynError::RevisionNotFound { .. } => StatusCode::NOT_FOUND,
            PynError::InvalidPath(_)
            | PynError::InvalidRules(_)
            | PynError::InvalidRequest(_)
            | PynError::InvalidInvite(_)
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

pub(crate) type ApiResult<T> = Result<T, ApiError>;

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let value = headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?;
    let (scheme, token) = value.split_once(' ')?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then_some(token.trim())
}

/// Authenticates the request and, when `need` is given, checks the caller holds that permission.
pub(crate) async fn authorize(
    state: &AppState,
    headers: &HeaderMap,
    need: Option<Permission>,
) -> ApiResult<Principal> {
    let dev_user = headers
        .get(api::DEV_USER_HEADER)
        .and_then(|v| v.to_str().ok());
    let principal = match (bearer_token(headers), &state.dev_auth, dev_user) {
        (Some(token), _, _) => state.auth.authenticate(token).await?,
        (None, Some(dev), Some(user)) => dev.authenticate(user).await?,
        _ => match session_api::session_cookie(headers) {
            Some(cookie) => {
                state
                    .access
                    .authenticate_session(state.service.repo(), cookie)
                    .await?
                    .0
            }
            None => return Err(PynError::Unauthenticated("missing credentials".into()).into()),
        },
    };
    if let Some(permission) = need {
        principal.require(permission)?;
    }
    Ok(principal)
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
        restored_from: r.restored_from.map(|r| r.0),
    }
}

#[utoipa::path(get, path = "/healthz", responses((status = 200, body = String)))]
async fn health() -> &'static str {
    "ok"
}

#[utoipa::path(get, path = "/v1/locks", responses((status = 200, body = Vec<api::Lock>)))]
async fn list_locks(
    State(s): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<Vec<api::Lock>>> {
    authorize(&s, &headers, Some(Permission::Read)).await?;
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
    let user = authorize(&s, &headers, Some(Permission::Lock)).await?.user;
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
    let user = authorize(&s, &headers, Some(Permission::Lock)).await?.user;
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
    let user = authorize(&s, &headers, Some(Permission::Checkin))
        .await?
        .user;
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

#[utoipa::path(post, path = "/v1/restore", request_body = api::RestoreRequest, responses(
    (status = 200, body = api::Revision),
    (status = 403, body = api::ErrorBody, description = "needs the restore permission"),
    (status = 409, body = api::ErrorBody, description = "lock_required, lock_held, stale_base or confirmation_required"),
))]
async fn restore(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<api::RestoreRequest>,
) -> ApiResult<Json<api::Revision>> {
    let user = authorize(&s, &headers, Some(Permission::Restore))
        .await?
        .user;
    let rev = s
        .service
        .restore(
            &RepoPath::new(req.path)?,
            &user,
            RevisionId(req.revision),
            RevisionId(req.base_revision),
            &req.confirm,
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
    authorize(&s, &headers, Some(Permission::Checkin)).await?;
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
    headers: HeaderMap,
    Query(q): Query<HistoryQuery>,
) -> ApiResult<Json<Vec<api::Revision>>> {
    authorize(&s, &headers, Some(Permission::Read)).await?;
    let revs = s.service.history(&RepoPath::new(q.path)?).await?;
    Ok(Json(revs.into_iter().map(revision_dto).collect()))
}

#[derive(Deserialize)]
struct FilesQuery {
    after: Option<String>,
    limit: Option<usize>,
}

fn mode_dto(mode: pyn_core::Mode) -> api::Mode {
    match mode {
        pyn_core::Mode::Shared => api::Mode::Shared,
        pyn_core::Mode::Exclusive => api::Mode::Exclusive,
    }
}

#[utoipa::path(get, path = "/v1/files",
    params(("after" = Option<String>, Query, description = "return paths after this one"),
           ("limit" = Option<usize>, Query, description = "page size, default 200, max 1000")),
    responses((status = 200, body = api::FilePage)))]
async fn list_files(
    State(s): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<FilesQuery>,
) -> ApiResult<Json<api::FilePage>> {
    authorize(&s, &headers, Some(Permission::Read)).await?;
    let limit = q.limit.unwrap_or(200).clamp(1, 1000);
    let after = q.after.map(RepoPath::new).transpose()?;
    let mut files = s.service.files(after.as_ref(), limit + 1).await?;
    let next_after = (files.len() > limit).then(|| {
        files.truncate(limit);
        files[limit - 1].path.to_string()
    });
    let entries = files
        .into_iter()
        .map(|f| api::FileEntry {
            path: f.path.to_string(),
            mode: mode_dto(f.mode),
            revision: f.revision.map(|r| r.0),
            lock: f.lock.map(lock_dto),
        })
        .collect();
    Ok(Json(api::FilePage {
        entries,
        next_after,
    }))
}

#[derive(Deserialize)]
struct ContentQuery {
    path: String,
    revision: Option<u64>,
}

#[utoipa::path(get, path = "/v1/content",
    params(("path" = String, Query, description = "repo-relative path"),
           ("revision" = Option<u64>, Query, description = "revision number; the head if omitted")),
    responses((status = 200, content_type = "application/octet-stream", body = Vec<u8>),
              (status = 404, body = api::ErrorBody, description = "revision_not_found")))]
async fn get_content(
    State(s): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ContentQuery>,
) -> ApiResult<Response> {
    authorize(&s, &headers, Some(Permission::Read)).await?;
    let (rev, bytes) = s
        .service
        .read(&RepoPath::new(q.path)?, q.revision.map(RevisionId))
        .await?;
    let headers = [
        (
            axum::http::header::CONTENT_TYPE,
            "application/octet-stream".to_string(),
        ),
        (
            axum::http::HeaderName::from_static(api::REVISION_HEADER),
            rev.id.to_string(),
        ),
    ];
    Ok((headers, bytes).into_response())
}

#[utoipa::path(post, path = "/v1/force-unlock", request_body = api::ForceUnlockRequest, responses(
    (status = 200, body = api::Lock, description = "the lock that was removed"),
    (status = 403, body = api::ErrorBody, description = "needs the force_unlock permission"),
    (status = 409, body = api::ErrorBody, description = "not_locked"),
))]
async fn force_unlock(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<api::ForceUnlockRequest>,
) -> ApiResult<Json<api::Lock>> {
    let actor = authorize(&s, &headers, Some(Permission::ForceUnlock))
        .await?
        .user;
    let lock = s
        .service
        .force_unlock(&RepoPath::new(req.path)?, &actor, &req.reason)
        .await?;
    Ok(Json(lock_dto(lock)))
}

#[derive(Deserialize)]
struct AuditQueryParams {
    path: Option<String>,
    actor: Option<String>,
    action: Option<String>,
    before: Option<i64>,
    limit: Option<usize>,
}

#[utoipa::path(get, path = "/v1/audit",
    params(("path" = Option<String>, Query, description = "only events for this path"),
           ("actor" = Option<String>, Query, description = "only events by this user"),
           ("action" = Option<String>, Query, description = "checkout, release, checkin, restore or force_unlock"),
           ("before" = Option<i64>, Query, description = "events older than this id"),
           ("limit" = Option<usize>, Query, description = "page size, default 50, max 500")),
    responses((status = 200, body = api::AuditPage), (status = 403, body = api::ErrorBody, description = "needs view_audit")))]
async fn audit(
    State(s): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<AuditQueryParams>,
) -> ApiResult<Json<api::AuditPage>> {
    authorize(&s, &headers, Some(Permission::ViewAudit)).await?;
    let limit = q.limit.unwrap_or(50).clamp(1, 500);
    let query = AuditQuery {
        path: q.path.map(RepoPath::new).transpose()?,
        actor: q.actor.map(pyn_core::UserId::new),
        action: q.action.map(|a| a.parse::<AuditAction>()).transpose()?,
        before: q.before,
        limit: limit + 1,
    };
    let mut events = s.service.audit(&query).await?;
    let next_before = (events.len() > limit).then(|| {
        events.truncate(limit);
        events[limit - 1].id
    });
    let entries = events
        .into_iter()
        .map(|e| api::AuditEntry {
            id: e.id,
            at: e.at,
            actor: e.actor.to_string(),
            action: e.action.to_string(),
            path: e.path.map(|p| p.to_string()),
            detail: e.detail,
        })
        .collect();
    Ok(Json(api::AuditPage {
        entries,
        next_before,
    }))
}
