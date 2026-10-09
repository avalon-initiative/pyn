//! HTTP API over `pyn_core::RepoService`. Handlers translate; they hold no policy.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use pyn_core::repositories::OpenRepo;
use pyn_core::{
    AccessService, AuditAction, AuditQuery, AuthProvider, ContentHash, Identity, ObjectStore,
    Permission, Principal, PynError, RepoPath, Repositories, RevisionId,
};
use pyn_proto as api;
use serde::Deserialize;
use utoipa::{IntoParams, OpenApi};

mod access_api;
pub mod auth;
mod repo_api;
mod session_api;

#[derive(Clone)]
pub struct AppState {
    pub repos: Arc<Repositories>,
    pub objects: Arc<dyn ObjectStore>,
    pub access: Arc<AccessService>,
    /// Authenticates bearer tokens.
    pub auth: Arc<dyn AuthProvider>,
    /// Accepts the `X-Pyn-User` header; present only when development auth is switched on.
    pub dev_auth: Option<Arc<dyn AuthProvider>>,
}

/// The `{owner}` and `{name}` of every repository route.
#[derive(IntoParams)]
#[allow(dead_code)]
#[into_params(parameter_in = Path)]
struct RepoAddress {
    /// The namespace that owns the repository.
    owner: String,
    /// The repository's name within its owner.
    name: String,
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
        access_api::add_key,
        access_api::list_keys,
        access_api::delete_key,
        access_api::me,
        access_api::create_token,
        access_api::list_tokens,
        access_api::revoke_token,
        repo_api::list_repos,
        repo_api::create_repo,
        repo_api::get_repo,
        repo_api::update_repo,
        repo_api::delete_repo,
        repo_api::repo_me,
        repo_api::add_user,
        repo_api::create_invite,
        repo_api::list_invites,
        repo_api::revoke_invite,
        repo_api::list_roles,
        repo_api::set_role,
        repo_api::list_members,
        repo_api::set_member,
        list_locks,
        list_files,
        tree,
        summary,
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
        api::TreeEntryKind,
        api::TreeMode,
        api::TreeEntry,
        api::TreeListing,
        api::ActivityEntry,
        api::RepoSummary,
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
        api::Account,
        api::Me,
        api::Visibility,
        api::RepoInfo,
        api::CreateRepoRequest,
        api::UpdateRepoRequest,
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
        .route(
            "/v1/repos",
            get(repo_api::list_repos).post(repo_api::create_repo),
        )
        .route(
            "/v1/repos/{owner}/{name}",
            get(repo_api::get_repo)
                .patch(repo_api::update_repo)
                .delete(repo_api::delete_repo),
        )
        .route("/v1/repos/{owner}/{name}/me", get(repo_api::repo_me))
        .route("/v1/repos/{owner}/{name}/users", post(repo_api::add_user))
        .route(
            "/v1/repos/{owner}/{name}/invites",
            get(repo_api::list_invites).post(repo_api::create_invite),
        )
        .route(
            "/v1/repos/{owner}/{name}/invites/{id}",
            axum::routing::delete(repo_api::revoke_invite),
        )
        .route("/v1/repos/{owner}/{name}/roles", get(repo_api::list_roles))
        .route(
            "/v1/repos/{owner}/{name}/roles/{role}",
            put(repo_api::set_role),
        )
        .route(
            "/v1/repos/{owner}/{name}/members",
            get(repo_api::list_members),
        )
        .route(
            "/v1/repos/{owner}/{name}/members/{user}",
            put(repo_api::set_member),
        )
        .route("/v1/repos/{owner}/{name}/locks", get(list_locks))
        .route("/v1/repos/{owner}/{name}/files", get(list_files))
        .route("/v1/repos/{owner}/{name}/tree", get(tree))
        .route("/v1/repos/{owner}/{name}/summary", get(summary))
        .route("/v1/repos/{owner}/{name}/content", get(get_content))
        .route("/v1/repos/{owner}/{name}/checkout", post(checkout))
        .route("/v1/repos/{owner}/{name}/release", post(release))
        .route("/v1/repos/{owner}/{name}/checkin", post(checkin))
        .route("/v1/repos/{owner}/{name}/restore", post(restore))
        .route("/v1/repos/{owner}/{name}/force-unlock", post(force_unlock))
        .route("/v1/repos/{owner}/{name}/audit", get(audit))
        .route("/v1/repos/{owner}/{name}/objects", put(put_object))
        .route("/v1/repos/{owner}/{name}/history", get(history))
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
            | PynError::RepoExists(_)
            | PynError::KeyInUse
            | PynError::ConfirmationRequired { .. } => StatusCode::CONFLICT,
            PynError::NotLockHolder(_)
            | PynError::Forbidden(_)
            | PynError::RegistrationClosed
            | PynError::NotNamespaceOwner(_)
            | PynError::CsrfFailed => StatusCode::FORBIDDEN,
            PynError::TooManyAttempts { .. } => StatusCode::TOO_MANY_REQUESTS,
            PynError::Unauthenticated(_) => StatusCode::UNAUTHORIZED,
            PynError::TokenNotFound(_)
            | PynError::KeyNotFound(_)
            | PynError::RepoNotFound(_)
            | PynError::PathNotFound(_) => StatusCode::NOT_FOUND,
            PynError::RevisionNotFound { .. } => StatusCode::NOT_FOUND,
            PynError::InvalidPath(_)
            | PynError::InvalidRules(_)
            | PynError::InvalidRequest(_)
            | PynError::InvalidInvite(_)
            | PynError::InvalidRepoName(_)
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

/// Names the account behind the request's credential: a bearer token, the dev header or a session cookie.
pub(crate) async fn identify(state: &AppState, headers: &HeaderMap) -> ApiResult<Identity> {
    let dev_user = headers
        .get(api::DEV_USER_HEADER)
        .and_then(|v| v.to_str().ok());
    Ok(match (bearer_token(headers), &state.dev_auth, dev_user) {
        (Some(token), _, _) => state.auth.authenticate(token).await?,
        (None, Some(dev), Some(user)) => dev.authenticate(user).await?,
        _ => match session_api::session_cookie(headers) {
            Some(cookie) => state.access.authenticate_session(cookie).await?.0,
            None => return Err(PynError::Unauthenticated("missing credentials".into()).into()),
        },
    })
}

/// Finds a repository the identity may know about and what it may do there. Anyone with no part in a private
/// repository is told it does not exist.
pub(crate) async fn open_visible(
    state: &AppState,
    who: &Identity,
    owner: &str,
    name: &str,
) -> ApiResult<(OpenRepo, Principal)> {
    let hidden = || PynError::RepoNotFound(format!("{owner}/{name}"));
    let open = state.repos.open(owner, name).await?;
    let principal = match state.access.principal_in(&open.record.id, who).await {
        Ok(p) => p,
        Err(PynError::Unauthenticated(_)) => return Err(hidden().into()),
        Err(e) => return Err(e.into()),
    };
    if principal.permissions.is_empty() && open.record.visibility == pyn_core::Visibility::Private {
        return Err(hidden().into());
    }
    Ok((open, principal))
}

/// Authenticates the request and resolves `owner/name`, checking the caller holds `need` there.
pub(crate) async fn authorize_repo(
    state: &AppState,
    headers: &HeaderMap,
    owner: &str,
    name: &str,
    need: Option<Permission>,
) -> ApiResult<(OpenRepo, Principal)> {
    let who = identify(state, headers).await?;
    let (open, principal) = open_visible(state, &who, owner, name).await?;
    if let Some(permission) = need {
        principal.require(permission)?;
    }
    Ok((open, principal))
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

#[utoipa::path(get, path = "/v1/repos/{owner}/{name}/locks", params(RepoAddress),
    responses((status = 200, body = Vec<api::Lock>)))]
async fn list_locks(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
) -> ApiResult<Json<Vec<api::Lock>>> {
    let (repo, _) = authorize_repo(&s, &headers, &owner, &name, Some(Permission::Read)).await?;
    Ok(Json(
        repo.service
            .locks()
            .await?
            .into_iter()
            .map(lock_dto)
            .collect(),
    ))
}

#[utoipa::path(post, path = "/v1/repos/{owner}/{name}/checkout", params(RepoAddress), request_body = api::CheckoutRequest, responses(
    (status = 200, body = api::Lock),
    (status = 409, body = api::ErrorBody, description = "lock_held or stale_base"),
    (status = 400, body = api::ErrorBody, description = "not_exclusive or invalid_path"),
))]
async fn checkout(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    Json(req): Json<api::CheckoutRequest>,
) -> ApiResult<Json<api::Lock>> {
    let (repo, who) = authorize_repo(&s, &headers, &owner, &name, Some(Permission::Lock)).await?;
    let user = who.user;
    let lock = repo
        .service
        .checkout(
            &RepoPath::new(req.path)?,
            &user,
            req.base_revision.map(RevisionId),
        )
        .await?;
    Ok(Json(lock_dto(lock)))
}

#[utoipa::path(post, path = "/v1/repos/{owner}/{name}/release", params(RepoAddress), request_body = api::ReleaseRequest, responses(
    (status = 204),
    (status = 403, body = api::ErrorBody, description = "not_lock_holder"),
))]
async fn release(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    Json(req): Json<api::ReleaseRequest>,
) -> ApiResult<StatusCode> {
    let (repo, who) = authorize_repo(&s, &headers, &owner, &name, Some(Permission::Lock)).await?;
    repo.service
        .release(&RepoPath::new(req.path)?, &who.user)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(post, path = "/v1/repos/{owner}/{name}/checkin", params(RepoAddress), request_body = api::CheckinRequest, responses(
    (status = 200, body = api::Revision),
    (status = 409, body = api::ErrorBody, description = "lock_required, lock_held or stale_base"),
    (status = 400, body = api::ErrorBody, description = "object_missing or invalid_path"),
))]
async fn checkin(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    Json(req): Json<api::CheckinRequest>,
) -> ApiResult<Json<api::Revision>> {
    let (repo, who) =
        authorize_repo(&s, &headers, &owner, &name, Some(Permission::Checkin)).await?;
    let user = who.user;
    let rev = repo
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

#[utoipa::path(post, path = "/v1/repos/{owner}/{name}/restore", params(RepoAddress), request_body = api::RestoreRequest, responses(
    (status = 200, body = api::Revision),
    (status = 403, body = api::ErrorBody, description = "needs the restore permission"),
    (status = 409, body = api::ErrorBody, description = "lock_required, lock_held, stale_base or confirmation_required"),
))]
async fn restore(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    Json(req): Json<api::RestoreRequest>,
) -> ApiResult<Json<api::Revision>> {
    let (repo, who) =
        authorize_repo(&s, &headers, &owner, &name, Some(Permission::Restore)).await?;
    let user = who.user;
    let rev = repo
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

#[utoipa::path(put, path = "/v1/repos/{owner}/{name}/objects", params(RepoAddress), request_body(content = Vec<u8>, content_type = "application/octet-stream"),
    responses((status = 200, body = api::PutObjectResponse)))]
async fn put_object(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    body: axum::body::Bytes,
) -> ApiResult<Json<api::PutObjectResponse>> {
    authorize_repo(&s, &headers, &owner, &name, Some(Permission::Checkin)).await?;
    let hash = s.objects.put(body.to_vec()).await?;
    Ok(Json(api::PutObjectResponse {
        content: hash.to_string(),
    }))
}

#[derive(Deserialize)]
struct HistoryQuery {
    path: String,
}

#[utoipa::path(get, path = "/v1/repos/{owner}/{name}/history", params(RepoAddress, ("path" = String, Query, description = "repo-relative path")),
    responses((status = 200, body = Vec<api::Revision>)))]
async fn history(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    Query(q): Query<HistoryQuery>,
) -> ApiResult<Json<Vec<api::Revision>>> {
    let (repo, _) = authorize_repo(&s, &headers, &owner, &name, Some(Permission::Read)).await?;
    let revs = repo.service.history(&RepoPath::new(q.path)?).await?;
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

#[utoipa::path(get, path = "/v1/repos/{owner}/{name}/files",
    params(RepoAddress, ("after" = Option<String>, Query, description = "return paths after this one"),
           ("limit" = Option<usize>, Query, description = "page size, default 200, max 1000")),
    responses((status = 200, body = api::FilePage)))]
async fn list_files(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    Query(q): Query<FilesQuery>,
) -> ApiResult<Json<api::FilePage>> {
    let (repo, _) = authorize_repo(&s, &headers, &owner, &name, Some(Permission::Read)).await?;
    let limit = q.limit.unwrap_or(200).clamp(1, 1000);
    let after = q.after.map(RepoPath::new).transpose()?;
    let mut files = repo.service.files(after.as_ref(), limit + 1).await?;
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
struct TreeQuery {
    path: Option<String>,
}

#[utoipa::path(get, path = "/v1/repos/{owner}/{name}/tree",
    params(RepoAddress, ("path" = Option<String>, Query, description = "folder to list; the root if omitted or empty")),
    responses((status = 200, body = api::TreeListing),
              (status = 400, body = api::ErrorBody, description = "invalid_path, or the path is a file"),
              (status = 404, body = api::ErrorBody, description = "path_not_found")))]
async fn tree(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    Query(q): Query<TreeQuery>,
) -> ApiResult<Json<api::TreeListing>> {
    let (repo, _) = authorize_repo(&s, &headers, &owner, &name, Some(Permission::Read)).await?;
    let raw = q.path.unwrap_or_default();
    let trimmed = raw.trim_end_matches('/');
    let dir = (!trimmed.is_empty())
        .then(|| RepoPath::new(trimmed))
        .transpose()?;
    let entries = repo.service.tree(dir.as_ref()).await?;
    Ok(Json(api::TreeListing {
        path: trimmed.to_string(),
        entries: entries.into_iter().map(tree_entry_dto).collect(),
    }))
}

fn tree_entry_dto(e: pyn_core::TreeEntry) -> api::TreeEntry {
    api::TreeEntry {
        name: e.name,
        path: e.path.to_string(),
        kind: match e.kind {
            pyn_core::EntryKind::File => api::TreeEntryKind::File,
            pyn_core::EntryKind::Folder => api::TreeEntryKind::Folder,
        },
        mode: match e.mode {
            pyn_core::EntryMode::Shared => api::TreeMode::Shared,
            pyn_core::EntryMode::Exclusive => api::TreeMode::Exclusive,
            pyn_core::EntryMode::Mixed => api::TreeMode::Mixed,
        },
        last_change: e.last_change.map(revision_dto),
        lock: e.lock.map(lock_dto),
    }
}

#[derive(Deserialize)]
struct SummaryQuery {
    activity: Option<usize>,
}

#[utoipa::path(get, path = "/v1/repos/{owner}/{name}/summary",
    params(RepoAddress, ("activity" = Option<usize>, Query, description = "recent activity entries, default 10, max 100")),
    responses((status = 200, body = api::RepoSummary)))]
async fn summary(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    Query(q): Query<SummaryQuery>,
) -> ApiResult<Json<api::RepoSummary>> {
    let (repo, _) = authorize_repo(&s, &headers, &owner, &name, Some(Permission::Read)).await?;
    let summary = repo
        .service
        .summary(q.activity.unwrap_or(10).clamp(1, 100))
        .await?;
    Ok(Json(api::RepoSummary {
        default_branch: summary.default_branch,
        branch_count: summary.branch_count,
        files: summary.files,
        exclusive_files: summary.exclusive_files,
        shared_files: summary.shared_files,
        updated_at: summary.updated_at,
        locks: summary.locks.into_iter().map(lock_dto).collect(),
        activity: summary
            .activity
            .into_iter()
            .map(|e| api::ActivityEntry {
                id: e.id,
                at: e.at,
                actor: e.actor.to_string(),
                action: e.action.to_string(),
                path: e.path.map(|p| p.to_string()),
            })
            .collect(),
    }))
}

#[derive(Deserialize)]
struct ContentQuery {
    path: String,
    revision: Option<u64>,
}

#[utoipa::path(get, path = "/v1/repos/{owner}/{name}/content",
    params(RepoAddress, ("path" = String, Query, description = "repo-relative path"),
           ("revision" = Option<u64>, Query, description = "revision number; the head if omitted")),
    responses((status = 200, content_type = "application/octet-stream", body = Vec<u8>),
              (status = 404, body = api::ErrorBody, description = "revision_not_found")))]
async fn get_content(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    Query(q): Query<ContentQuery>,
) -> ApiResult<Response> {
    let (repo, _) = authorize_repo(&s, &headers, &owner, &name, Some(Permission::Read)).await?;
    let (rev, bytes) = repo
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

#[utoipa::path(post, path = "/v1/repos/{owner}/{name}/force-unlock", params(RepoAddress), request_body = api::ForceUnlockRequest, responses(
    (status = 200, body = api::Lock, description = "the lock that was removed"),
    (status = 403, body = api::ErrorBody, description = "needs the force_unlock permission"),
    (status = 409, body = api::ErrorBody, description = "not_locked"),
))]
async fn force_unlock(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    Json(req): Json<api::ForceUnlockRequest>,
) -> ApiResult<Json<api::Lock>> {
    let (repo, who) =
        authorize_repo(&s, &headers, &owner, &name, Some(Permission::ForceUnlock)).await?;
    let actor = who.user;
    let lock = repo
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

#[utoipa::path(get, path = "/v1/repos/{owner}/{name}/audit",
    params(RepoAddress, ("path" = Option<String>, Query, description = "only events for this path"),
           ("actor" = Option<String>, Query, description = "only events by this user"),
           ("action" = Option<String>, Query, description = "checkout, release, checkin, restore, force_unlock, member_added, role_changed, role_permissions_changed, token_created, token_revoked, repo_created, repo_updated or repo_deleted"),
           ("before" = Option<i64>, Query, description = "events older than this id"),
           ("limit" = Option<usize>, Query, description = "page size, default 50, max 500")),
    responses((status = 200, body = api::AuditPage), (status = 403, body = api::ErrorBody, description = "needs view_audit")))]
async fn audit(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    Query(q): Query<AuditQueryParams>,
) -> ApiResult<Json<api::AuditPage>> {
    let (repo, _) =
        authorize_repo(&s, &headers, &owner, &name, Some(Permission::ViewAudit)).await?;
    let limit = q.limit.unwrap_or(50).clamp(1, 500);
    let query = AuditQuery {
        path: q.path.map(RepoPath::new).transpose()?,
        actor: q.actor.map(pyn_core::UserId::new),
        action: q.action.map(|a| a.parse::<AuditAction>()).transpose()?,
        before: q.before,
        limit: limit + 1,
    };
    let mut events = repo.service.audit(&query).await?;
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
