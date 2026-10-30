//! Organizations: namespaces that own repositories.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use std::str::FromStr;

use pyn_core::{AccountRecord, AuditQuery, OrgAdd, OrgInvitation, OrgRole, UserId};
use pyn_proto as api;
use serde::Deserialize;

use crate::{ApiResult, AppState, identify};

fn org_info(org: AccountRecord, role: Option<OrgRole>) -> api::OrgInfo {
    api::OrgInfo {
        id: org.id.to_string(),
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
              (status = 401, body = api::ErrorBody),
              (status = 403, body = api::ErrorBody, description = "a token that is not full-scope")))]
pub(crate) async fn list_orgs(
    State(s): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<Vec<api::OrgInfo>>> {
    let who = identify(&s, &headers).await?;
    let orgs = s.access.orgs_for(&who).await?;
    Ok(Json(
        orgs.into_iter()
            .map(|(org, role)| org_info(org, Some(role)))
            .collect(),
    ))
}

#[utoipa::path(get, path = "/v1/me/orgs",
    responses((status = 200, body = Vec<api::OrgInfo>, description = "the organizations the caller belongs to, each with the caller's role, ordered by name"),
              (status = 401, body = api::ErrorBody),
              (status = 403, body = api::ErrorBody, description = "a token that is not full-scope")))]
pub(crate) async fn my_orgs(
    state: State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<Vec<api::OrgInfo>>> {
    list_orgs(state, headers).await
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
              (status = 409, body = api::ErrorBody, description = "org_not_empty: it still owns repositories; org_deleting: another delete of it is in flight")))]
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
    responses((status = 200, body = api::AuditPage, description = "organization events (created, deleted, member added, removed or changed), newest first"),
              (status = 403, body = api::ErrorBody, description = "not_org_owner, or a token that is not full-scope"),
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
        .map(|e| crate::audit_entry_dto(e, false))
        .collect();
    Ok(Json(api::AuditPage {
        entries,
        next_before,
    }))
}

#[utoipa::path(get, path = "/v1/orgs/{org}/members", operation_id = "org_list_members",
    params(("org" = String, Path, description = "the organization's name")),
    responses((status = 200, body = Vec<api::OrgMember>, description = "ordered by user name; any member may list"),
              (status = 403, body = api::ErrorBody, description = "not_org_member, or a token that is not full-scope"),
              (status = 404, body = api::ErrorBody, description = "org_not_found")))]
pub(crate) async fn list_members(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(org): Path<String>,
) -> ApiResult<Json<Vec<api::OrgMember>>> {
    let who = identify(&s, &headers).await?;
    let members = s.access.org_members(&who, &UserId::new(org)).await?;
    Ok(Json(members.into_iter().map(member_info).collect()))
}

fn member_info((user, role): (UserId, OrgRole)) -> api::OrgMember {
    api::OrgMember {
        user: user.to_string(),
        role: role.to_string(),
    }
}

#[utoipa::path(post, path = "/v1/orgs/{org}/members",
    params(("org" = String, Path, description = "the organization's name")),
    request_body = api::AddOrgMemberRequest,
    responses((status = 202, body = api::OrgInvitation, description = "invited: the user becomes a member when they accept"),
              (status = 201, body = api::OrgMember, description = "added without an invitation: only a server administrator, and only when the server's org member add setting is `direct`"),
              (status = 400, body = api::ErrorBody, description = "invalid_request: bad role, or the name is an organization"),
              (status = 403, body = api::ErrorBody, description = "not_org_owner, or a token without manage_roles or limited to repositories"),
              (status = 404, body = api::ErrorBody, description = "org_not_found, or user_not_found"),
              (status = 409, body = api::ErrorBody, description = "already_org_member, or already_invited")))]
pub(crate) async fn add_member(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(org): Path<String>,
    Json(req): Json<api::AddOrgMemberRequest>,
) -> ApiResult<Response> {
    let who = identify(&s, &headers).await?;
    let role = req
        .role
        .as_deref()
        .map_or(Ok(OrgRole::Member), parse_role)?;
    let user = UserId::new(req.user);
    let added = s
        .access
        .add_org_member(&who, &UserId::new(org), &user, role)
        .await?;
    Ok(match added {
        OrgAdd::Added => (StatusCode::CREATED, Json(member_info((user, role)))).into_response(),
        OrgAdd::Invited(i) => (StatusCode::ACCEPTED, Json(invitation_info(i))).into_response(),
    })
}

fn invitation_info(i: OrgInvitation) -> api::OrgInvitation {
    api::OrgInvitation {
        org: i.org.to_string(),
        user: i.user.to_string(),
        role: i.role.to_string(),
        invited_by: i.invited_by.to_string(),
        created_at: i.created_at,
        expires_at: i.expires_at,
    }
}

#[utoipa::path(get, path = "/v1/orgs/{org}/invitations",
    params(("org" = String, Path, description = "the organization's name")),
    responses((status = 200, body = Vec<api::OrgInvitation>, description = "pending invitations ordered by user name; expired ones are gone"),
              (status = 403, body = api::ErrorBody, description = "not_org_owner, or a token that is not full-scope"),
              (status = 404, body = api::ErrorBody, description = "org_not_found")))]
pub(crate) async fn list_invitations(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(org): Path<String>,
) -> ApiResult<Json<Vec<api::OrgInvitation>>> {
    let who = identify(&s, &headers).await?;
    let found = s.access.org_invitations(&who, &UserId::new(org)).await?;
    Ok(Json(found.into_iter().map(invitation_info).collect()))
}

#[utoipa::path(delete, path = "/v1/orgs/{org}/invitations/{user}",
    params(("org" = String, Path, description = "the organization's name"),
           ("user" = String, Path, description = "the invited user's name")),
    responses((status = 204, description = "the invitation is withdrawn"),
              (status = 403, body = api::ErrorBody, description = "not_org_owner, or a token without manage_roles or limited to repositories"),
              (status = 404, body = api::ErrorBody, description = "org_not_found, or org_invitation_not_found")))]
pub(crate) async fn cancel_invitation(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((org, user)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let who = identify(&s, &headers).await?;
    s.access
        .cancel_org_invitation(&who, &UserId::new(org), &UserId::new(user))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(get, path = "/v1/me/invitations",
    responses((status = 200, body = Vec<api::OrgInvitation>, description = "the caller's pending organization invitations, ordered by organization name"),
              (status = 401, body = api::ErrorBody),
              (status = 403, body = api::ErrorBody, description = "a token that is not full-scope")))]
pub(crate) async fn my_invitations(
    State(s): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<Vec<api::OrgInvitation>>> {
    let who = identify(&s, &headers).await?;
    let found = s.access.my_org_invitations(&who).await?;
    Ok(Json(found.into_iter().map(invitation_info).collect()))
}

#[utoipa::path(post, path = "/v1/me/invitations/{org}/accept",
    params(("org" = String, Path, description = "the inviting organization's name")),
    responses((status = 200, body = api::OrgInfo, description = "the caller is now a member, with the offered role"),
              (status = 403, body = api::ErrorBody, description = "a token without manage_roles or limited to repositories"),
              (status = 404, body = api::ErrorBody, description = "org_not_found, or org_invitation_not_found"),
              (status = 409, body = api::ErrorBody, description = "member_limit_reached: the organization is full; the invitation stays")))]
pub(crate) async fn accept_invitation(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(org): Path<String>,
) -> ApiResult<Json<api::OrgInfo>> {
    let who = identify(&s, &headers).await?;
    let (org, role) = s
        .access
        .accept_org_invitation(&who, &UserId::new(org))
        .await?;
    Ok(Json(org_info(org, Some(role))))
}

#[utoipa::path(post, path = "/v1/me/invitations/{org}/decline",
    params(("org" = String, Path, description = "the inviting organization's name")),
    responses((status = 204, description = "the invitation is gone"),
              (status = 403, body = api::ErrorBody, description = "a token without manage_roles or limited to repositories"),
              (status = 404, body = api::ErrorBody, description = "org_not_found, or org_invitation_not_found")))]
pub(crate) async fn decline_invitation(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(org): Path<String>,
) -> ApiResult<StatusCode> {
    let who = identify(&s, &headers).await?;
    s.access
        .decline_org_invitation(&who, &UserId::new(org))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

fn parse_role(role: &str) -> Result<OrgRole, pyn_core::PynError> {
    OrgRole::from_str(role).map_err(|_| {
        pyn_core::PynError::InvalidRequest(format!("unknown role {role:?}; use owner or member"))
    })
}

#[utoipa::path(patch, path = "/v1/orgs/{org}/members/{user}",
    params(("org" = String, Path, description = "the organization's name"),
           ("user" = String, Path, description = "the member's user name")),
    request_body = api::SetOrgRoleRequest,
    responses((status = 200, body = api::OrgMember),
              (status = 400, body = api::ErrorBody, description = "invalid_request: bad role"),
              (status = 403, body = api::ErrorBody, description = "not_org_owner, or a token without manage_roles or limited to repositories"),
              (status = 404, body = api::ErrorBody, description = "org_not_found, or org_member_not_found"),
              (status = 409, body = api::ErrorBody, description = "last_org_owner: the only owner cannot be demoted")))]
pub(crate) async fn set_member_role(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((org, user)): Path<(String, String)>,
    Json(req): Json<api::SetOrgRoleRequest>,
) -> ApiResult<Json<api::OrgMember>> {
    let who = identify(&s, &headers).await?;
    let role = parse_role(&req.role)?;
    let user = UserId::new(user);
    s.access
        .set_org_member_role(&who, &UserId::new(org), &user, role)
        .await?;
    Ok(Json(member_info((user, role))))
}

#[utoipa::path(delete, path = "/v1/orgs/{org}/members/{user}",
    params(("org" = String, Path, description = "the organization's name"),
           ("user" = String, Path, description = "the member's user name")),
    responses((status = 204, description = "also deletes their direct roles on the organization's repositories; a member may remove themselves"),
              (status = 403, body = api::ErrorBody, description = "not_org_owner (removing someone else), or a token without manage_roles or limited to repositories"),
              (status = 404, body = api::ErrorBody, description = "org_not_found, or org_member_not_found"),
              (status = 409, body = api::ErrorBody, description = "last_org_owner: the only owner cannot leave")))]
pub(crate) async fn remove_member(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((org, user)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let who = identify(&s, &headers).await?;
    s.access
        .remove_org_member(&who, &UserId::new(org), &UserId::new(user))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
