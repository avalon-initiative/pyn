//! Organizations: namespaces that own repositories.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use pyn_core::{AccountRecord, AuditQuery, OrgRole, UserId};
use pyn_proto as api;
use serde::Deserialize;

use crate::{ApiResult, AppState, identify};

fn org_info(org: AccountRecord, role: Option<OrgRole>) -> api::OrgInfo {
    api::OrgInfo {
        name: org.user.to_string(),
        created_at: org.created_at,
        role: role.map(|r| r.to_string()),
    }
}

#[utoipa::path(post, path = "/v1/orgs", request_body = api::CreateOrgRequest, responses(
    (status = 201, body = api::OrgInfo, description = "the caller becomes its first owner"),
    (status = 400, body = api::ErrorBody, description = "invalid_request (bad name) or reserved_name"),
    (status = 403, body = api::ErrorBody, description = "server_admin_required when the server limits creation to administrators, or a token without manage_roles or limited to repositories"),
    (status = 409, body = api::ErrorBody, description = "user_exists: the name is taken by a user or organization"),
))]
pub(crate) async fn create_org(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<api::CreateOrgRequest>,
) -> ApiResult<(StatusCode, Json<api::OrgInfo>)> {
    let who = identify(&s, &headers).await?;
    let org = s.access.create_org(&who, &req.name).await?;
    Ok((
        StatusCode::CREATED,
        Json(org_info(org, Some(OrgRole::Owner))),
    ))
}

#[utoipa::path(get, path = "/v1/orgs",
    responses((status = 200, body = Vec<api::OrgInfo>, description = "the organizations the caller belongs to, ordered by name"),
              (status = 401, body = api::ErrorBody)))]
pub(crate) async fn list_orgs(
    State(s): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<Vec<api::OrgInfo>>> {
    let who = identify(&s, &headers).await?;
    let orgs = s.access.orgs_of(&who.user).await?;
    Ok(Json(
        orgs.into_iter()
            .map(|(org, role)| org_info(org, Some(role)))
            .collect(),
    ))
}

#[utoipa::path(get, path = "/v1/orgs/{org}",
    params(("org" = String, Path, description = "the organization's name")),
    responses((status = 200, body = api::OrgInfo, description = "any signed-in account can look an organization up"),
              (status = 404, body = api::ErrorBody, description = "org_not_found (also for a user name)")))]
pub(crate) async fn get_org(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> ApiResult<Json<api::OrgInfo>> {
    let who = identify(&s, &headers).await?;
    let name = UserId::new(name);
    let org = s.access.org(&name).await?;
    let role = s.access.org_role(&name, &who.user).await?;
    Ok(Json(org_info(org, role)))
}

#[utoipa::path(delete, path = "/v1/orgs/{org}",
    params(("org" = String, Path, description = "the organization's name")),
    responses((status = 204, description = "its audit log is kept"),
              (status = 403, body = api::ErrorBody, description = "not_org_owner"),
              (status = 404, body = api::ErrorBody, description = "org_not_found"),
              (status = 409, body = api::ErrorBody, description = "org_not_empty: it still owns repositories")))]
pub(crate) async fn delete_org(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> ApiResult<StatusCode> {
    let who = identify(&s, &headers).await?;
    s.access.delete_org(&who, &UserId::new(name)).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub(crate) struct OrgAuditParams {
    before: Option<i64>,
    limit: Option<usize>,
}

#[utoipa::path(get, path = "/v1/orgs/{org}/audit",
    params(("org" = String, Path, description = "the organization's name"),
           ("before" = Option<i64>, Query, description = "events older than this id"),
           ("limit" = Option<usize>, Query, description = "page size, default 50, max 500")),
    responses((status = 200, body = api::AuditPage, description = "org_created and org_deleted events, newest first"),
              (status = 403, body = api::ErrorBody, description = "not_org_owner"),
              (status = 404, body = api::ErrorBody, description = "org_not_found")))]
pub(crate) async fn org_audit(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Query(q): Query<OrgAuditParams>,
) -> ApiResult<Json<api::AuditPage>> {
    let who = identify(&s, &headers).await?;
    let limit = q.limit.unwrap_or(50).clamp(1, 500);
    let query = AuditQuery {
        before: q.before,
        limit: limit + 1,
        ..Default::default()
    };
    let mut events = s.access.org_audit(&who, &UserId::new(name), &query).await?;
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
