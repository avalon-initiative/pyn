//! Endpoints for the caller's identity, tokens, members and roles.

use std::collections::BTreeSet;
use std::str::FromStr;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use pyn_core::{Permission, PynError, Role, TokenId, TokenRecord, UserId};
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
