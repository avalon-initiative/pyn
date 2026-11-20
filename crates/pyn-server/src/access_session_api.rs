//! Administrator access sessions (decision #1024): an administrator acts as an account for at most an hour, and the
//! account lists the sessions on it.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use pyn_core::{AccessSessionRecord, AccessSessionView, TokenId, UserId};
use pyn_proto as api;
use serde::Deserialize;

use crate::caller::{AdminCaller, FullScopeCaller};
use crate::{ApiResult, AppState};

fn info(view: AccessSessionView) -> api::AccessSessionInfo {
    let r = view.record;
    api::AccessSessionInfo {
        id: r.id.to_string(),
        admin: r.admin.to_string(),
        user: r.user.to_string(),
        reason: r.reason.to_string(),
        notes: r.notes,
        started_at: r.started_at,
        expires_at: r.expires_at,
        ended_at: r.ended_at,
        state: view.state.to_string(),
        reached: view
            .reached
            .into_iter()
            .map(|c| api::AccessSessionReach {
                method: c.method,
                route: c.route,
                target: c.target,
                refused: c.refused,
                count: c.count,
                first_at: c.first_at,
                last_at: c.last_at,
            })
            .collect(),
    }
}

fn started(record: AccessSessionRecord) -> AccessSessionView {
    AccessSessionView {
        record,
        reached: Vec::new(),
        state: "active",
    }
}

#[utoipa::path(post, path = "/v1/admin/users/{user}/access-sessions", operation_id = "start_access_session",
    params(("user" = String, Path, description = "the account to act as")),
    request_body = api::StartAccessSessionRequest,
    responses(
    (status = 201, body = api::StartedAccessSession, description = "the secret is shown once; present it as a bearer token. The session ends by itself after an hour and is never extended"),
    (status = 400, body = api::ErrorBody, description = "invalid_request: an unknown reason, `other` without notes, notes over 500 characters, or the administrator's own account"),
    (status = 403, body = api::ErrorBody, description = "server_admin_required, or access_session_not_allowed for an access session; account_inactive for an account that is not active"),
    (status = 404, body = api::ErrorBody, description = "user_not_found"),
))]
pub(crate) async fn start(
    State(s): State<AppState>,
    AdminCaller(who): AdminCaller,
    Path(user): Path<String>,
    Json(req): Json<api::StartAccessSessionRequest>,
) -> ApiResult<(StatusCode, Json<api::StartedAccessSession>)> {
    let (record, secret) = s
        .access
        .start_access_session(&who, &UserId::new(user), &req.reason, req.notes.as_deref())
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(api::StartedAccessSession {
            secret,
            info: info(started(record)),
        }),
    ))
}

#[derive(Deserialize)]
pub(crate) struct ListParams {
    user: Option<String>,
    limit: Option<usize>,
}

#[utoipa::path(get, path = "/v1/admin/access-sessions", operation_id = "list_access_sessions",
    params(("user" = Option<String>, Query, description = "only sessions on this account"),
           ("limit" = Option<usize>, Query, description = "default 50, max 200")),
    responses((status = 200, body = Vec<api::AccessSessionInfo>, description = "newest first, with the routes each called"),
              (status = 403, body = api::ErrorBody, description = "server_admin_required"),
              (status = 404, body = api::ErrorBody, description = "user_not_found")))]
pub(crate) async fn list(
    State(s): State<AppState>,
    AdminCaller(who): AdminCaller,
    Query(q): Query<ListParams>,
) -> ApiResult<Json<Vec<api::AccessSessionInfo>>> {
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let user = q.user.map(UserId::new);
    let views = s
        .access
        .list_access_sessions(&who, user.as_ref(), limit)
        .await?;
    Ok(Json(views.into_iter().map(info).collect()))
}

#[utoipa::path(post, path = "/v1/admin/access-sessions/{id}/end", operation_id = "end_access_session",
    params(("id" = String, Path, description = "the session id")),
    responses((status = 200, body = api::AccessSessionInfo, description = "stops working at once; ending a session that already stopped changes nothing. Any administrator may end any session"),
              (status = 403, body = api::ErrorBody, description = "server_admin_required"),
              (status = 404, body = api::ErrorBody, description = "access_session_not_found")))]
pub(crate) async fn end(
    State(s): State<AppState>,
    AdminCaller(who): AdminCaller,
    Path(id): Path<String>,
) -> ApiResult<Json<api::AccessSessionInfo>> {
    let view = s.access.end_access_session(&who, &TokenId(id)).await?;
    Ok(Json(info(view)))
}

#[utoipa::path(get, path = "/v1/me/access-sessions", operation_id = "my_access_sessions",
    params(("limit" = Option<usize>, Query, description = "default 50, max 200")),
    responses((status = 200, body = Vec<api::AccessSessionInfo>, description = "every administrator access session on the signed-in account, newest first: the notification that one started. It cannot be turned off or dismissed"),
              (status = 403, body = api::ErrorBody, description = "forbidden for a token that is not full scope; access_session_not_allowed for an access session")))]
pub(crate) async fn mine(
    State(s): State<AppState>,
    FullScopeCaller(who): FullScopeCaller,
    Query(q): Query<LimitParam>,
) -> ApiResult<Json<Vec<api::AccessSessionInfo>>> {
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let views = s.access.my_access_sessions(&who, limit).await?;
    Ok(Json(views.into_iter().map(info).collect()))
}

#[derive(Deserialize)]
pub(crate) struct LimitParam {
    limit: Option<usize>,
}
