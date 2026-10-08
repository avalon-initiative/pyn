//! Endpoints for the caller's identity, tokens, members and roles.

use std::collections::BTreeSet;
use std::str::FromStr;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use chrono::Duration;
use pyn_core::{InviteId, InviteRecord, Permission, PynError, Role, TokenId, TokenRecord, UserId};
use pyn_proto as api;
use serde::Deserialize;

use crate::{ApiResult, AppState, authorize};

fn names(permissions: &BTreeSet<Permission>) -> Vec<String> {
    permissions.iter().map(|p| p.as_str().to_string()).collect()
}

fn parse_permissions(names: &[String]) -> Result<BTreeSet<Permission>, PynError> {
    names.iter().map(|n| Permission::from_str(n)).collect()
}

fn token_info(t: TokenRecord) -> api::TokenInfo {
    api::TokenInfo {
        id: t.id.to_string(),
        user: t.user.to_string(),
        name: t.name,
        permissions: names(&t.permissions),
        repos: t.repos.iter().map(|r| r.to_string()).collect(),
        created_at: t.created_at,
        expires_at: t.expires_at,
        revoked_at: t.revoked_at,
        last_used_at: t.last_used_at,
    }
}

#[utoipa::path(get, path = "/v1/me", responses((status = 200, body = api::Me), (status = 401, body = api::ErrorBody)))]
pub(crate) async fn me(State(s): State<AppState>, headers: HeaderMap) -> ApiResult<Json<api::Me>> {
    let who = authorize(&s, &headers, None).await?;
    Ok(Json(api::Me {
        user: who.user.to_string(),
        permissions: names(&who.permissions),
    }))
}

#[utoipa::path(post, path = "/v1/tokens", request_body = api::CreateTokenRequest, responses(
    (status = 200, body = api::CreatedToken),
    (status = 403, body = api::ErrorBody, description = "asked for a permission the caller lacks"),
))]
pub(crate) async fn create_token(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<api::CreateTokenRequest>,
) -> ApiResult<Json<api::CreatedToken>> {
    let who = authorize(&s, &headers, None).await?;
    let permissions = parse_permissions(&req.permissions)?;
    let repos = vec![s.service.repo().clone()];
    let (record, token) = s
        .access
        .create_token(&who, &req.name, permissions, repos, req.expires_at)
        .await?;
    Ok(Json(api::CreatedToken {
        token,
        info: token_info(record),
    }))
}

#[derive(Deserialize)]
pub(crate) struct TokensQuery {
    user: Option<String>,
}

#[utoipa::path(get, path = "/v1/tokens",
    params(("user" = Option<String>, Query, description = "another user's tokens; needs manage_users")),
    responses((status = 200, body = Vec<api::TokenInfo>)))]
pub(crate) async fn list_tokens(
    State(s): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<TokensQuery>,
) -> ApiResult<Json<Vec<api::TokenInfo>>> {
    let who = authorize(&s, &headers, None).await?;
    let user = q.user.map_or_else(|| who.user.clone(), UserId::new);
    let tokens = s.access.list_tokens(&who, &user).await?;
    Ok(Json(tokens.into_iter().map(token_info).collect()))
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
    let who = authorize(&s, &headers, None).await?;
    s.access.revoke_token(&who, &TokenId(id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(get, path = "/v1/roles", responses((status = 200, body = Vec<api::RoleGrant>)))]
pub(crate) async fn list_roles(
    State(s): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<Vec<api::RoleGrant>>> {
    authorize(&s, &headers, None).await?;
    let defs = s.access.role_definitions(s.service.repo()).await?;
    let grants = Role::ALL
        .into_iter()
        .map(|role| api::RoleGrant {
            role: role.to_string(),
            permissions: names(defs.get(role)),
        })
        .collect();
    Ok(Json(grants))
}

#[utoipa::path(put, path = "/v1/roles/{role}", request_body = api::SetRoleRequest, responses(
    (status = 204),
    (status = 403, body = api::ErrorBody, description = "needs manage_roles"),
))]
pub(crate) async fn set_role(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(role): Path<String>,
    Json(req): Json<api::SetRoleRequest>,
) -> ApiResult<StatusCode> {
    let who = authorize(&s, &headers, None).await?;
    let permissions = parse_permissions(&req.permissions)?;
    s.access
        .set_role_permissions(&who, s.service.repo(), Role::from_str(&role)?, permissions)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(get, path = "/v1/members", responses((status = 200, body = Vec<api::Member>)))]
pub(crate) async fn list_members(
    State(s): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<Vec<api::Member>>> {
    let who = authorize(&s, &headers, None).await?;
    let members = s.access.members(&who, s.service.repo()).await?;
    Ok(Json(
        members
            .into_iter()
            .map(|(u, r)| api::Member {
                user: u.to_string(),
                role: r.to_string(),
            })
            .collect(),
    ))
}

#[utoipa::path(put, path = "/v1/members/{user}", request_body = api::SetMemberRequest, responses(
    (status = 204),
    (status = 403, body = api::ErrorBody, description = "needs manage_users and every permission the role grants"),
))]
pub(crate) async fn set_member(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(user): Path<String>,
    Json(req): Json<api::SetMemberRequest>,
) -> ApiResult<StatusCode> {
    let who = authorize(&s, &headers, None).await?;
    s.access
        .set_user_role(
            &who,
            s.service.repo(),
            &UserId::new(user),
            Role::from_str(&req.role)?,
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

fn invite_info(i: InviteRecord) -> api::InviteInfo {
    api::InviteInfo {
        id: i.id.to_string(),
        role: i.role.to_string(),
        created_by: i.created_by.to_string(),
        created_at: i.created_at,
        expires_at: i.expires_at,
        used_at: i.used_at,
        used_by: i.used_by.map(|u| u.to_string()),
        revoked_at: i.revoked_at,
    }
}

#[utoipa::path(get, path = "/v1/registration", responses((status = 200, body = api::RegistrationInfo)))]
pub(crate) async fn registration(State(s): State<AppState>) -> Json<api::RegistrationInfo> {
    Json(api::RegistrationInfo {
        registration: s.access.registration_mode().as_str().to_string(),
    })
}

#[utoipa::path(post, path = "/v1/register", request_body = api::RegisterRequest, responses(
    (status = 201, body = api::Registered),
    (status = 400, body = api::ErrorBody, description = "invalid_request or invalid_invite"),
    (status = 403, body = api::ErrorBody, description = "registration_closed"),
    (status = 409, body = api::ErrorBody, description = "user_exists"),
))]
pub(crate) async fn register(
    State(s): State<AppState>,
    Json(req): Json<api::RegisterRequest>,
) -> ApiResult<(StatusCode, Json<api::Registered>)> {
    let user = s
        .access
        .register(
            s.service.repo(),
            &req.username,
            &req.password,
            req.invite.as_deref(),
        )
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(api::Registered {
            user: user.to_string(),
        }),
    ))
}

#[utoipa::path(post, path = "/v1/login", request_body = api::LoginRequest, responses(
    (status = 200, body = api::CreatedToken),
    (status = 401, body = api::ErrorBody, description = "unauthenticated"),
    (status = 429, body = api::ErrorBody, description = "too_many_attempts"),
))]
pub(crate) async fn login(
    State(s): State<AppState>,
    Json(req): Json<api::LoginRequest>,
) -> ApiResult<Json<api::CreatedToken>> {
    let (record, token) = s
        .access
        .login(s.service.repo(), &req.username, &req.password)
        .await?;
    Ok(Json(api::CreatedToken {
        token,
        info: token_info(record),
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
    let who = authorize(&s, &headers, None).await?;
    s.access
        .change_password(&who, req.current.as_deref(), &req.new)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(post, path = "/v1/users", request_body = api::AddUserRequest, responses(
    (status = 201, body = api::Registered),
    (status = 403, body = api::ErrorBody, description = "needs manage_users"),
    (status = 409, body = api::ErrorBody, description = "user_exists"),
))]
pub(crate) async fn add_user(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<api::AddUserRequest>,
) -> ApiResult<(StatusCode, Json<api::Registered>)> {
    let who = authorize(&s, &headers, None).await?;
    let role = Role::from_str(&req.role)?;
    let user = s
        .access
        .add_user(&who, s.service.repo(), &req.username, &req.password, role)
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(api::Registered {
            user: user.to_string(),
        }),
    ))
}

#[utoipa::path(post, path = "/v1/invites", request_body = api::CreateInviteRequest, responses(
    (status = 200, body = api::CreatedInvite),
    (status = 403, body = api::ErrorBody, description = "needs manage_users and every permission the role grants"),
))]
pub(crate) async fn create_invite(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<api::CreateInviteRequest>,
) -> ApiResult<Json<api::CreatedInvite>> {
    let who = authorize(&s, &headers, None).await?;
    let role = Role::from_str(&req.role)?;
    let (record, code) = s
        .access
        .create_invite(&who, s.service.repo(), role, Duration::hours(req.hours))
        .await?;
    Ok(Json(api::CreatedInvite {
        code,
        info: invite_info(record),
    }))
}

#[utoipa::path(get, path = "/v1/invites", responses((status = 200, body = Vec<api::InviteInfo>)))]
pub(crate) async fn list_invites(
    State(s): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<Vec<api::InviteInfo>>> {
    let who = authorize(&s, &headers, None).await?;
    let invites = s.access.list_invites(&who, s.service.repo()).await?;
    Ok(Json(invites.into_iter().map(invite_info).collect()))
}

#[utoipa::path(delete, path = "/v1/invites/{id}", responses((status = 204), (status = 400, body = api::ErrorBody)))]
pub(crate) async fn revoke_invite(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    let who = authorize(&s, &headers, None).await?;
    s.access.revoke_invite(&who, &InviteId(id)).await?;
    Ok(StatusCode::NO_CONTENT)
}
