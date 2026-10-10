//! HTTP API over `pyn_core::RepoService`. Handlers translate; they hold no policy.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::header::RETRY_AFTER;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use pyn_core::repositories::OpenRepo;
use pyn_core::{
    AccessService, AuditAction, AuditQuery, AuthProvider, ContentHash, HistoryCursor, Identity,
    ObjectStore, PathFilter, Permission, Principal, PynError, RepoPath, Repositories, RevisionId,
};
use pyn_proto as api;
use serde::Deserialize;
use utoipa::openapi::RefOr;
use utoipa::openapi::schema::{KnownFormat, ObjectBuilder, Schema, SchemaFormat, Type};
use utoipa::{IntoParams, OpenApi, PartialSchema, ToSchema};

mod access_api;
mod admin_api;
pub mod auth;
mod client;
pub mod email;
mod org_api;
pub mod passwords;
mod repo_api;
mod repo_policy_api;
mod session_api;
mod setup_api;
mod team_api;

#[derive(Clone)]
pub struct AppState {
    pub repos: Arc<Repositories>,
    pub objects: Arc<dyn ObjectStore>,
    pub access: Arc<AccessService>,
    /// Authenticates bearer tokens.
    pub auth: Arc<dyn AuthProvider>,
    /// Accepts the `X-Pyn-User` header; present only when development auth is switched on.
    pub dev_auth: Option<Arc<dyn AuthProvider>>,
    /// Takes the caller's address from `X-Forwarded-For` (the last entry), for a server behind a trusted proxy.
    pub trust_forwarded: bool,
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
        setup_api::setup_status,
        setup_api::complete_setup,
        access_api::registration,
        access_api::register,
        access_api::verify_email,
        access_api::resend_verification,
        access_api::login,
        session_api::sign_in,
        session_api::current,
        session_api::sign_out,
        access_api::change_password,
        access_api::add_key,
        access_api::list_keys,
        access_api::delete_key,
        access_api::me,
        repo_api::my_locks,
        admin_api::list_accounts,
        admin_api::approve,
        admin_api::disable,
        admin_api::enable,
        admin_api::grant_admin,
        admin_api::revoke_admin,
        admin_api::audit,
        admin_api::create_org,
        admin_api::delete_org,
        admin_api::create_service_credential,
        admin_api::list_service_credentials,
        admin_api::revoke_service_credential,
        access_api::create_token,
        access_api::list_tokens,
        access_api::revoke_token,
        org_api::create_org,
        org_api::list_orgs,
        org_api::get_org,
        org_api::delete_org,
        org_api::org_audit,
        org_api::my_orgs,
        org_api::list_members,
        org_api::add_member,
        org_api::set_member_role,
        org_api::remove_member,
        repo_policy_api::get_repo_policy,
        repo_policy_api::set_repo_policy,
        repo_policy_api::set_creation_rule,
        repo_policy_api::remove_creation_rule,
        team_api::list_teams,
        team_api::create_team,
        team_api::get_team,
        team_api::update_team,
        team_api::delete_team,
        team_api::add_team_member,
        team_api::remove_team_member,
        team_api::list_repo_teams,
        team_api::set_repo_team,
        team_api::remove_repo_team,
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
        api::HistoryPage,
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
        api::SetupStatus,
        api::SetupRequest,
        api::SetupCompleted,
        api::RegistrationInfo,
        api::RegisterRequest,
        api::Registered,
        api::VerifyEmailRequest,
        api::ResendVerificationRequest,
        api::AccountInfo,
        api::DisableAccountRequest,
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
        api::MyLock,
        api::Me,
        api::OrgInfo,
        api::CreateOrgRequest,
        api::AdminCreateOrgRequest,
        api::CreateServiceCredentialRequest,
        api::ServiceCredentialInfo,
        api::CreatedServiceCredential,
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
        api::SetMemberRequest,
        api::RepoPolicyInfo,
        api::CreationRuleInfo,
        api::SetRepoPolicyRequest,
        api::SetCreationRuleRequest,
        api::TeamInfo,
        api::TeamRepo,
        api::CreateTeamRequest,
        api::UpdateTeamRequest,
        api::RepoTeam,
        api::SetTeamRoleRequest
    ))
)]
pub struct ApiDoc;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(health))
        .route("/openapi.json", get(|| async { Json(ApiDoc::openapi()) }))
        .route(
            "/v1/setup",
            get(setup_api::setup_status).post(setup_api::complete_setup),
        )
        .route("/v1/registration", get(access_api::registration))
        .route("/v1/register", post(access_api::register))
        .route("/v1/register/verify", post(access_api::verify_email))
        .route("/v1/register/resend", post(access_api::resend_verification))
        .route("/v1/admin/users", get(admin_api::list_accounts))
        .route("/v1/admin/users/{user}/approve", post(admin_api::approve))
        .route("/v1/admin/users/{user}/disable", post(admin_api::disable))
        .route("/v1/admin/users/{user}/enable", post(admin_api::enable))
        .route(
            "/v1/admin/users/{user}/admin",
            put(admin_api::grant_admin).delete(admin_api::revoke_admin),
        )
        .route("/v1/admin/audit", get(admin_api::audit))
        .route("/v1/admin/orgs", post(admin_api::create_org))
        .route("/v1/admin/orgs/{org}", delete(admin_api::delete_org))
        .route(
            "/v1/admin/service-credentials",
            get(admin_api::list_service_credentials).post(admin_api::create_service_credential),
        )
        .route(
            "/v1/admin/service-credentials/{name}",
            delete(admin_api::revoke_service_credential),
        )
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
        .route("/v1/me/locks", get(repo_api::my_locks))
        .route(
            "/v1/tokens",
            get(access_api::list_tokens).post(access_api::create_token),
        )
        .route(
            "/v1/tokens/{id}",
            axum::routing::delete(access_api::revoke_token),
        )
        .route(
            "/v1/orgs",
            get(org_api::list_orgs).post(org_api::create_org),
        )
        .route(
            "/v1/orgs/{org}",
            get(org_api::get_org).delete(org_api::delete_org),
        )
        .route("/v1/orgs/{org}/audit", get(org_api::org_audit))
        .route(
            "/v1/orgs/{org}/members",
            get(org_api::list_members).post(org_api::add_member),
        )
        .route(
            "/v1/orgs/{org}/members/{user}",
            axum::routing::patch(org_api::set_member_role).delete(org_api::remove_member),
        )
        .route(
            "/v1/orgs/{org}/repo-policy",
            get(repo_policy_api::get_repo_policy).put(repo_policy_api::set_repo_policy),
        )
        .route(
            "/v1/orgs/{org}/repo-policy/rules/{effect}/{kind}/{subject}",
            put(repo_policy_api::set_creation_rule).delete(repo_policy_api::remove_creation_rule),
        )
        .route(
            "/v1/orgs/{org}/teams",
            get(team_api::list_teams).post(team_api::create_team),
        )
        .route(
            "/v1/orgs/{org}/teams/{team}",
            get(team_api::get_team)
                .patch(team_api::update_team)
                .delete(team_api::delete_team),
        )
        .route(
            "/v1/orgs/{org}/teams/{team}/members/{user}",
            put(team_api::add_team_member).delete(team_api::remove_team_member),
        )
        .route("/v1/me/orgs", get(org_api::my_orgs))
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
        .route(
            "/v1/repos/{owner}/{name}/teams",
            get(team_api::list_repo_teams),
        )
        .route(
            "/v1/repos/{owner}/{name}/teams/{team}",
            put(team_api::set_repo_team).delete(team_api::remove_repo_team),
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
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            setup_api::guard,
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
            | PynError::LockLimitReached { .. }
            | PynError::StaleBase { .. }
            | PynError::LockRequired(_)
            | PynError::NotLocked(_)
            | PynError::UserExists(_)
            | PynError::RepoExists(_)
            | PynError::OrgNotEmpty(_)
            | PynError::OrgDeleting(_)
            | PynError::UserNotOrgMember { .. }
            | PynError::AlreadyOrgMember { .. }
            | PynError::TeamExists { .. }
            | PynError::LastOrgOwner(_)
            | PynError::LastServerAdmin
            | PynError::KeyInUse
            | PynError::AlreadyInitialised
            | PynError::ServiceCredentialExists(_)
            | PynError::ConfirmationRequired { .. } => StatusCode::CONFLICT,
            PynError::NotLockHolder(_)
            | PynError::Forbidden(_)
            | PynError::RegistrationClosed
            | PynError::InvalidSetupToken
            | PynError::AccountInactive(_)
            | PynError::ServerAdminRequired
            | PynError::ServiceScopeRequired(_)
            | PynError::ServiceCredentialNotAllowed
            | PynError::NotNamespaceOwner(_)
            | PynError::NotOrgOwner(_)
            | PynError::NotOrgMember(_)
            | PynError::RepoCreateForbidden { .. }
            | PynError::CsrfFailed => StatusCode::FORBIDDEN,
            PynError::TooManyAttempts { .. } => StatusCode::TOO_MANY_REQUESTS,
            PynError::NotInitialised => StatusCode::SERVICE_UNAVAILABLE,
            PynError::Unauthenticated(_) => StatusCode::UNAUTHORIZED,
            PynError::TokenNotFound(_)
            | PynError::ServiceCredentialNotFound(_)
            | PynError::UserNotFound(_)
            | PynError::KeyNotFound(_)
            | PynError::RepoNotFound(_)
            | PynError::OrgNotFound(_)
            | PynError::OrgMemberNotFound { .. }
            | PynError::TeamNotFound { .. }
            | PynError::TeamMemberNotFound { .. }
            | PynError::CreationRuleNotFound { .. }
            | PynError::PathNotFound(_) => StatusCode::NOT_FOUND,
            PynError::RevisionNotFound { .. } => StatusCode::NOT_FOUND,
            PynError::InvalidPath(_)
            | PynError::InvalidRules(_)
            | PynError::InvalidRequest(_)
            | PynError::InvalidInvite(_)
            | PynError::InvalidVerification(_)
            | PynError::InvalidRepoName(_)
            | PynError::ReservedName(_)
            | PynError::NotOrgRepo(_)
            | PynError::NotExclusive(_)
            | PynError::ObjectMissing(_) => StatusCode::BAD_REQUEST,
            PynError::Storage(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let body = api::ErrorBody {
            code: self.0.code().to_string(),
            message: self.0.to_string(),
        };
        let mut res = (status, Json(body)).into_response();
        if let PynError::TooManyAttempts { retry_after_secs } = self.0 {
            res.headers_mut()
                .insert(RETRY_AFTER, HeaderValue::from(retry_after_secs));
        }
        res
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
/// A service credential is refused; only the admin routes take one.
pub(crate) async fn identify(state: &AppState, headers: &HeaderMap) -> ApiResult<Identity> {
    let who = identify_admin(state, headers).await?;
    if matches!(who.credential, pyn_core::Credential::Service(_)) {
        return Err(PynError::ServiceCredentialNotAllowed.into());
    }
    Ok(who)
}

/// Like `identify`, but a service credential is accepted too.
pub(crate) async fn identify_admin(state: &AppState, headers: &HeaderMap) -> ApiResult<Identity> {
    let dev_user = headers
        .get(api::DEV_USER_HEADER)
        .and_then(|v| v.to_str().ok());
    Ok(match (bearer_token(headers), &state.dev_auth, dev_user) {
        (Some(token), _, _) => state.auth.authenticate(token).await?,
        (None, Some(dev), Some(user)) => {
            let who = dev.authenticate(user).await?;
            state.access.reject_org(&who.user).await?;
            who
        }
        _ => match session_api::session_cookie(headers) {
            Some(cookie) => state.access.authenticate_session(cookie).await?.0,
            None => return Err(PynError::Unauthenticated("missing credentials".into()).into()),
        },
    })
}

/// Like `identify`, but a request with no credential at all is anonymous (`None`); a bad one still fails.
pub(crate) async fn identify_optional(
    state: &AppState,
    headers: &HeaderMap,
) -> ApiResult<Option<Identity>> {
    let dev = state.dev_auth.is_some() && headers.contains_key(api::DEV_USER_HEADER);
    if bearer_token(headers).is_none() && !dev && session_api::session_cookie(headers).is_none() {
        return Ok(None);
    }
    identify(state, headers).await.map(Some)
}

/// Finds a repository the caller may know about and what they may do there. Anyone with no part in a private
/// repository is told it does not exist.
pub(crate) async fn open_visible(
    state: &AppState,
    who: Option<&Identity>,
    owner: &str,
    name: &str,
) -> ApiResult<(OpenRepo, Principal)> {
    let open = state.repos.open(owner, name).await?;
    match state.repos.principal_for(&open.record, who).await? {
        Some(principal) => Ok((open, principal)),
        None => Err(PynError::RepoNotFound(format!("{owner}/{name}")).into()),
    }
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
    let (open, principal) = open_visible(state, Some(&who), owner, name).await?;
    if let Some(permission) = need {
        principal.require(permission)?;
    }
    Ok((open, principal))
}

/// For read endpoints: like `authorize_repo`, but a public repository can be read without signing in.
pub(crate) async fn authorize_read(
    state: &AppState,
    headers: &HeaderMap,
    owner: &str,
    name: &str,
) -> ApiResult<OpenRepo> {
    let who = identify_optional(state, headers).await?;
    let (open, principal) = open_visible(state, who.as_ref(), owner, name).await?;
    principal.require(Permission::Read)?;
    Ok(open)
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
        mode: r.mode.map(mode_dto),
    }
}

#[utoipa::path(get, path = "/healthz", responses((status = 200, body = String)))]
async fn health() -> &'static str {
    "ok"
}

/// Readable without credentials when the repository is public; otherwise 404 unless the caller has a role.
#[utoipa::path(get, path = "/v1/repos/{owner}/{name}/locks", params(RepoAddress),
    responses((status = 200, body = Vec<api::Lock>)))]
async fn list_locks(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
) -> ApiResult<Json<Vec<api::Lock>>> {
    let repo = authorize_read(&s, &headers, &owner, &name).await?;
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
    (status = 409, body = api::ErrorBody, description = "lock_held, lock_limit_reached or stale_base"),
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
    (status = 400, body = api::ErrorBody, description = "object_missing, invalid_path, or invalid_rules for a malformed .pyn/pyn.toml"),
    (status = 403, body = api::ErrorBody, description = "checking in .pyn/pyn.toml needs the edit_policy permission"),
))]
async fn checkin(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    Json(req): Json<api::CheckinRequest>,
) -> ApiResult<Json<api::Revision>> {
    let (repo, who) =
        authorize_repo(&s, &headers, &owner, &name, Some(Permission::Checkin)).await?;
    let rev = repo
        .service
        .checkin(
            &who,
            &RepoPath::new(req.path)?,
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
    let rev = repo
        .service
        .restore(
            &who,
            &RepoPath::new(req.path)?,
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
    path: Option<String>,
    filter: Option<String>,
    before: Option<String>,
    limit: Option<usize>,
}

/// Readable without credentials when the repository is public; otherwise 404 unless the caller has a role.
#[utoipa::path(get, path = "/v1/repos/{owner}/{name}/history",
    params(RepoAddress,
           ("path" = Option<String>, Query, description = "one path's revisions, oldest first; excludes the other parameters"),
           ("filter" = Option<String>, Query, description = "glob over paths for repository history; no slash matches at any depth"),
           ("before" = Option<String>, Query, description = "the previous page's next_cursor"),
           ("limit" = Option<usize>, Query, description = "page size, default 50, max 200")),
    responses((status = 200, body = api::HistoryPage),
              (status = 400, body = api::ErrorBody, description = "invalid_request for a bad filter or cursor, or path combined with them")))]
async fn history(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    Query(q): Query<HistoryQuery>,
) -> ApiResult<Json<api::HistoryPage>> {
    let repo = authorize_read(&s, &headers, &owner, &name).await?;
    if let Some(path) = q.path {
        if q.filter.is_some() || q.before.is_some() || q.limit.is_some() {
            return Err(PynError::InvalidRequest(
                "path cannot be combined with filter, before or limit".into(),
            )
            .into());
        }
        let revs = repo.service.history(&RepoPath::new(path)?).await?;
        return Ok(Json(api::HistoryPage {
            revisions: revs.into_iter().map(revision_dto).collect(),
            next_cursor: None,
        }));
    }
    let filter = q.filter.as_deref().map(PathFilter::new).transpose()?;
    let before = q.before.as_deref().map(HistoryCursor::decode).transpose()?;
    let page = repo
        .service
        .repo_history(filter.as_ref(), before.as_ref(), q.limit)
        .await?;
    Ok(Json(api::HistoryPage {
        revisions: page.revisions.into_iter().map(revision_dto).collect(),
        next_cursor: page.next.map(|c| c.encode()),
    }))
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

/// Readable without credentials when the repository is public; otherwise 404 unless the caller has a role.
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
    let repo = authorize_read(&s, &headers, &owner, &name).await?;
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

/// Readable without credentials when the repository is public; otherwise 404 unless the caller has a role.
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
    let repo = authorize_read(&s, &headers, &owner, &name).await?;
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

/// Readable without credentials when the repository is public; otherwise 404 unless the caller has a role.
#[utoipa::path(get, path = "/v1/repos/{owner}/{name}/summary",
    params(RepoAddress, ("activity" = Option<usize>, Query, description = "recent activity entries, default 10, max 100")),
    responses((status = 200, body = api::RepoSummary)))]
async fn summary(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    Query(q): Query<SummaryQuery>,
) -> ApiResult<Json<api::RepoSummary>> {
    let repo = authorize_read(&s, &headers, &owner, &name).await?;
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

/// The raw bytes of a file revision.
struct FileContent;

impl PartialSchema for FileContent {
    fn schema() -> RefOr<Schema> {
        ObjectBuilder::new()
            .schema_type(Type::String)
            .format(Some(SchemaFormat::KnownFormat(KnownFormat::Binary)))
            .into()
    }
}

impl ToSchema for FileContent {}

#[derive(Deserialize)]
struct ContentQuery {
    path: String,
    revision: Option<u64>,
}

/// Readable without credentials when the repository is public; otherwise 404 unless the caller has a role.
#[utoipa::path(get, path = "/v1/repos/{owner}/{name}/content",
    params(RepoAddress, ("path" = String, Query, description = "repo-relative path"),
           ("revision" = Option<u64>, Query, description = "revision number; the head if omitted")),
    responses((status = 200, content_type = "application/octet-stream", body = inline(FileContent)),
              (status = 404, body = api::ErrorBody, description = "revision_not_found")))]
async fn get_content(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    Query(q): Query<ContentQuery>,
) -> ApiResult<Response> {
    let repo = authorize_read(&s, &headers, &owner, &name).await?;
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

#[utoipa::path(get, path = "/v1/repos/{owner}/{name}/audit", operation_id = "repo_audit",
    params(RepoAddress, ("path" = Option<String>, Query, description = "only events for this path"),
           ("actor" = Option<String>, Query, description = "only events by this user"),
           ("action" = Option<String>, Query, description = "checkout, release, checkin, restore, force_unlock, member_added, role_changed, role_permissions_changed, team_access_set, team_access_removed, token_created, token_revoked, repo_created, repo_updated, repo_deleted or policy_changed"),
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
