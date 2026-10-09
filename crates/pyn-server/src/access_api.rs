//! Account endpoints: registration, sign-in, the caller's tokens and keys. None of them name a repository.

use std::collections::BTreeSet;
use std::str::FromStr;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use pyn_core::{
    Permission, PynError, Registration, RepoId, SignUp, SshKeyRecord, TokenId, TokenRecord, UserId,
};
use pyn_proto as api;
use serde::Deserialize;

use crate::client::Client;
use crate::{ApiResult, AppState, identify, open_visible};

pub(crate) fn names(permissions: &BTreeSet<Permission>) -> Vec<String> {
    permissions.iter().map(|p| p.as_str().to_string()).collect()
}

fn parse_permissions(names: &[String]) -> Result<BTreeSet<Permission>, PynError> {
    names.iter().map(|n| Permission::from_str(n)).collect()
}

async fn token_info(s: &AppState, t: TokenRecord) -> ApiResult<api::TokenInfo> {
    let mut repos = Vec::new();
    for id in &t.repos {
        repos.push(s.repos.address_of(id).await?);
    }
    Ok(api::TokenInfo {
        id: t.id.to_string(),
        user: t.user.to_string(),
        name: t.name,
        permissions: names(&t.permissions),
        repos,
        created_at: t.created_at,
        expires_at: t.expires_at,
        revoked_at: t.revoked_at,
        last_used_at: t.last_used_at,
    })
}

#[utoipa::path(get, path = "/v1/me", responses((status = 200, body = api::Account), (status = 401, body = api::ErrorBody)))]
pub(crate) async fn me(
    State(s): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<api::Account>> {
    let who = identify(&s, &headers).await?;
    Ok(Json(api::Account {
        user: who.user.to_string(),
        admin: s.access.is_server_admin(&who).await?,
    }))
}

#[utoipa::path(post, path = "/v1/tokens", request_body = api::CreateTokenRequest, responses(
    (status = 200, body = api::CreatedToken),
    (status = 403, body = api::ErrorBody, description = "asked for a permission the caller lacks in a listed repository"),
    (status = 404, body = api::ErrorBody, description = "repo_not_found"),
))]
pub(crate) async fn create_token(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<api::CreateTokenRequest>,
) -> ApiResult<Json<api::CreatedToken>> {
    let who = identify(&s, &headers).await?;
    let permissions = parse_permissions(&req.permissions)?;
    let mut repos: Vec<RepoId> = Vec::new();
    for address in &req.repos {
        let (owner, name) = address.split_once('/').ok_or_else(|| {
            PynError::InvalidRequest(format!("{address:?} is not an owner/name repository"))
        })?;
        let (open, _) = open_visible(&s, Some(&who), owner, name).await?;
        repos.push(open.record.id);
    }
    let (record, token) = s
        .access
        .create_token(&who, &req.name, permissions, repos, req.expires_at)
        .await?;
    Ok(Json(api::CreatedToken {
        token,
        info: token_info(&s, record).await?,
    }))
}

#[derive(Deserialize)]
pub(crate) struct TokensQuery {
    user: Option<String>,
}

#[utoipa::path(get, path = "/v1/tokens",
    params(("user" = Option<String>, Query, description = "another user's tokens; needs manage_users in a repository that user belongs to")),
    responses((status = 200, body = Vec<api::TokenInfo>)))]
pub(crate) async fn list_tokens(
    State(s): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<TokensQuery>,
) -> ApiResult<Json<Vec<api::TokenInfo>>> {
    let who = identify(&s, &headers).await?;
    let user = q.user.map_or_else(|| who.user.clone(), UserId::new);
    let mut infos = Vec::new();
    for t in s.access.list_tokens(&who, &user).await? {
        infos.push(token_info(&s, t).await?);
    }
    Ok(Json(infos))
}

#[utoipa::path(delete, path = "/v1/tokens/{id}", responses(
    (status = 204),
    (status = 404, body = api::ErrorBody, description = "token_not_found"),
))]
pub(crate) async fn revoke_token(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    let who = identify(&s, &headers).await?;
    s.access.revoke_token(&who, &TokenId(id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(get, path = "/v1/registration", responses((status = 200, body = api::RegistrationInfo)))]
pub(crate) async fn registration(State(s): State<AppState>) -> Json<api::RegistrationInfo> {
    let open = s.access.registration_mode() == pyn_core::RegistrationMode::Open;
    Json(api::RegistrationInfo {
        registration: s.access.registration_mode().as_str().to_string(),
        email_verification: open && s.access.requires_email_verification(),
        approval: open && s.access.requires_approval(),
    })
}

fn registered(up: SignUp) -> api::Registered {
    api::Registered {
        user: up.user.to_string(),
        status: up.status.to_string(),
    }
}

#[utoipa::path(post, path = "/v1/register", request_body = api::RegisterRequest, responses(
    (status = 201, body = api::Registered, description = "status says what the account still needs; with an unverified address the answer is the same whether or not the address is already in use"),
    (status = 400, body = api::ErrorBody, description = "invalid_request or invalid_invite"),
    (status = 403, body = api::ErrorBody, description = "registration_closed"),
    (status = 409, body = api::ErrorBody, description = "user_exists"),
    (status = 429, body = api::ErrorBody, description = "too_many_attempts; Retry-After says how many seconds to wait"),
))]
pub(crate) async fn register(
    State(s): State<AppState>,
    Client(client): Client,
    Json(req): Json<api::RegisterRequest>,
) -> ApiResult<(StatusCode, Json<api::Registered>)> {
    let mut request = Registration::new(&req.username, &req.password);
    request.email = req.email.as_deref();
    request.invite = req.invite.as_deref();
    request.client = client.as_deref();
    let up = s.access.register(request).await?;
    Ok((StatusCode::CREATED, Json(registered(up))))
}

#[utoipa::path(post, path = "/v1/register/verify", request_body = api::VerifyEmailRequest, responses(
    (status = 200, body = api::Registered, description = "the address is confirmed; status is active or pending_approval"),
    (status = 400, body = api::ErrorBody, description = "invalid_verification: the link is unknown, used, expired or its address belongs to another account"),
))]
pub(crate) async fn verify_email(
    State(s): State<AppState>,
    Json(req): Json<api::VerifyEmailRequest>,
) -> ApiResult<Json<api::Registered>> {
    Ok(Json(registered(s.access.verify_email(&req.token).await?)))
}

#[utoipa::path(post, path = "/v1/register/resend", request_body = api::ResendVerificationRequest, responses(
    (status = 202, description = "always, whether or not a message was sent"),
    (status = 429, body = api::ErrorBody, description = "too_many_attempts; Retry-After says how many seconds to wait"),
))]
pub(crate) async fn resend_verification(
    State(s): State<AppState>,
    Client(client): Client,
    Json(req): Json<api::ResendVerificationRequest>,
) -> ApiResult<StatusCode> {
    s.access
        .resend_verification(&req.email, client.as_deref())
        .await?;
    Ok(StatusCode::ACCEPTED)
}

#[utoipa::path(post, path = "/v1/login", request_body = api::LoginRequest, responses(
    (status = 200, body = api::CreatedToken),
    (status = 401, body = api::ErrorBody, description = "unauthenticated"),
    (status = 403, body = api::ErrorBody, description = "right password, but the account cannot sign in: email_not_verified, approval_pending or account_disabled"),
    (status = 429, body = api::ErrorBody, description = "too_many_attempts; Retry-After says how many seconds to wait"),
))]
pub(crate) async fn login(
    State(s): State<AppState>,
    Client(client): Client,
    Json(req): Json<api::LoginRequest>,
) -> ApiResult<Json<api::CreatedToken>> {
    let (record, token) = s
        .access
        .login(&req.username, &req.password, client.as_deref())
        .await?;
    Ok(Json(api::CreatedToken {
        token,
        info: token_info(&s, record).await?,
    }))
}

#[utoipa::path(put, path = "/v1/me/password", request_body = api::ChangePasswordRequest, responses(
    (status = 204),
    (status = 401, body = api::ErrorBody, description = "the current password is wrong"),
))]
pub(crate) async fn change_password(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<api::ChangePasswordRequest>,
) -> ApiResult<StatusCode> {
    let who = identify(&s, &headers).await?;
    s.access
        .change_password(&who, req.current.as_deref(), &req.new)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

fn key_info(k: SshKeyRecord) -> api::SshKeyInfo {
    api::SshKeyInfo {
        id: k.id,
        user: k.user.to_string(),
        title: k.title,
        algorithm: k.algorithm,
        fingerprint: k.fingerprint,
        created_at: k.created_at,
        last_used_at: k.last_used_at,
    }
}

#[derive(Deserialize)]
pub(crate) struct KeysQuery {
    user: Option<String>,
}

#[utoipa::path(post, path = "/v1/keys", request_body = api::AddKeyRequest, responses(
    (status = 200, body = api::SshKeyInfo),
    (status = 400, body = api::ErrorBody, description = "not a usable public key"),
    (status = 409, body = api::ErrorBody, description = "key_in_use"),
))]
pub(crate) async fn add_key(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<api::AddKeyRequest>,
) -> ApiResult<Json<api::SshKeyInfo>> {
    let who = identify(&s, &headers).await?;
    let key = s
        .access
        .add_ssh_key(&who, req.title.as_deref(), &req.key)
        .await?;
    Ok(Json(key_info(key)))
}

#[utoipa::path(get, path = "/v1/keys",
    params(("user" = Option<String>, Query, description = "another user's keys; needs manage_users")),
    responses((status = 200, body = Vec<api::SshKeyInfo>)))]
pub(crate) async fn list_keys(
    State(s): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<KeysQuery>,
) -> ApiResult<Json<Vec<api::SshKeyInfo>>> {
    let who = identify(&s, &headers).await?;
    let user = q.user.map_or_else(|| who.user.clone(), UserId::new);
    let keys = s.access.list_ssh_keys(&who, &user).await?;
    Ok(Json(keys.into_iter().map(key_info).collect()))
}

#[utoipa::path(delete, path = "/v1/keys/{id}",
    params(("user" = Option<String>, Query, description = "another user's key; needs manage_users")),
    responses((status = 204), (status = 404, body = api::ErrorBody, description = "key_not_found")))]
pub(crate) async fn delete_key(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<KeysQuery>,
) -> ApiResult<StatusCode> {
    let who = identify(&s, &headers).await?;
    let user = q.user.map_or_else(|| who.user.clone(), UserId::new);
    s.access.delete_ssh_key(&who, &user, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}
