//! Server administration: approving, disabling and enabling accounts, and the log of those actions.

use std::str::FromStr;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use pyn_core::{AccountRecord, AccountStatus, AuditQuery, UserId};
use pyn_proto as api;
use serde::Deserialize;

use crate::{ApiResult, AppState, identify};

fn info(a: AccountRecord) -> api::AccountInfo {
    api::AccountInfo {
        status: a.status().to_string(),
        user: a.user.to_string(),
        email: a.email,
        email_verified: a.email_verified_at.is_some(),
        disabled_at: a.disabled_at,
        disabled_reason: a.disabled_reason,
        admin: a.is_admin,
        created_at: a.created_at,
    }
}

#[derive(Deserialize)]
pub(crate) struct AccountsQuery {
    status: Option<String>,
    limit: Option<usize>,
}

#[utoipa::path(get, path = "/v1/admin/users",
    params(("status" = Option<String>, Query, description = "only accounts in this state: pending_verification, pending_approval, active or disabled"),
           ("limit" = Option<usize>, Query, description = "default 100, max 500")),
    responses((status = 200, body = Vec<api::AccountInfo>, description = "oldest first"),
              (status = 403, body = api::ErrorBody, description = "server_admin_required")))]
pub(crate) async fn list_accounts(
    State(s): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<AccountsQuery>,
) -> ApiResult<Json<Vec<api::AccountInfo>>> {
    let who = identify(&s, &headers).await?;
    let status = q
        .status
        .as_deref()
        .map(AccountStatus::from_str)
        .transpose()?;
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let accounts = s.access.list_accounts(&who, status, limit).await?;
    Ok(Json(accounts.into_iter().map(info).collect()))
}

#[utoipa::path(post, path = "/v1/admin/users/{user}/approve", responses(
    (status = 200, body = api::AccountInfo),
    (status = 400, body = api::ErrorBody, description = "invalid_request: the account is not waiting for approval"),
    (status = 403, body = api::ErrorBody, description = "server_admin_required"),
    (status = 404, body = api::ErrorBody, description = "user_not_found"),
))]
pub(crate) async fn approve(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(user): Path<String>,
) -> ApiResult<Json<api::AccountInfo>> {
    let who = identify(&s, &headers).await?;
    let account = s.access.approve_account(&who, &UserId::new(user)).await?;
    Ok(Json(info(account)))
}

#[utoipa::path(post, path = "/v1/admin/users/{user}/disable", request_body = api::DisableAccountRequest, responses(
    (status = 200, body = api::AccountInfo, description = "sessions end at once; tokens and keys stop working"),
    (status = 400, body = api::ErrorBody, description = "invalid_request: one's own account"),
    (status = 403, body = api::ErrorBody, description = "server_admin_required"),
    (status = 404, body = api::ErrorBody, description = "user_not_found"),
))]
pub(crate) async fn disable(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(user): Path<String>,
    Json(req): Json<api::DisableAccountRequest>,
) -> ApiResult<Json<api::AccountInfo>> {
    let who = identify(&s, &headers).await?;
    let account = s
        .access
        .disable_account(&who, &UserId::new(user), req.reason.as_deref())
        .await?;
    Ok(Json(info(account)))
}

#[utoipa::path(post, path = "/v1/admin/users/{user}/enable", responses(
    (status = 200, body = api::AccountInfo),
    (status = 400, body = api::ErrorBody, description = "invalid_request: the account is not disabled"),
    (status = 403, body = api::ErrorBody, description = "server_admin_required"),
    (status = 404, body = api::ErrorBody, description = "user_not_found"),
))]
pub(crate) async fn enable(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(user): Path<String>,
) -> ApiResult<Json<api::AccountInfo>> {
    let who = identify(&s, &headers).await?;
    let account = s.access.enable_account(&who, &UserId::new(user)).await?;
    Ok(Json(info(account)))
}

#[derive(Deserialize)]
pub(crate) struct AuditParams {
    before: Option<i64>,
    limit: Option<usize>,
}

#[utoipa::path(get, path = "/v1/admin/audit",
    params(("before" = Option<i64>, Query, description = "events older than this id"),
           ("limit" = Option<usize>, Query, description = "page size, default 50, max 500")),
    responses((status = 200, body = api::AuditPage, description = "account_approved, account_disabled and account_enabled events"),
              (status = 403, body = api::ErrorBody, description = "server_admin_required")))]
pub(crate) async fn audit(
    State(s): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<AuditParams>,
) -> ApiResult<Json<api::AuditPage>> {
    let who = identify(&s, &headers).await?;
    let limit = q.limit.unwrap_or(50).clamp(1, 500);
    let query = AuditQuery {
        before: q.before,
        limit: limit + 1,
        ..Default::default()
    };
    let mut events = s.access.server_audit(&who, &query).await?;
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
            path: None,
            detail: e.detail,
        })
        .collect();
    Ok(Json(api::AuditPage {
        entries,
        next_before,
    }))
}
