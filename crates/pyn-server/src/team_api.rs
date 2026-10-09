//! Teams of an organization and the roles they hold on its repositories.

use std::str::FromStr;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use pyn_core::{Role, TeamDetail, UserId};
use pyn_proto as api;

use crate::{ApiResult, AppState, RepoAddress, authorize_repo, identify};

fn team_info(d: TeamDetail) -> api::TeamInfo {
    api::TeamInfo {
        slug: d.team.slug,
        name: d.team.name,
        description: d.team.description,
        created_at: d.team.created_at,
        members: d.members.into_iter().map(|u| u.to_string()).collect(),
        repos: d
            .repos
            .into_iter()
            .map(|(repo, role)| api::TeamRepo {
                repo: repo.address(),
                role: role.to_string(),
            })
            .collect(),
    }
}

#[utoipa::path(get, path = "/v1/orgs/{org}/teams",
    params(("org" = String, Path, description = "the organization's name")),
    responses((status = 200, body = Vec<api::TeamInfo>, description = "ordered by slug; any member of the organization may list"),
              (status = 403, body = api::ErrorBody, description = "not_org_member"),
              (status = 404, body = api::ErrorBody, description = "org_not_found")))]
pub(crate) async fn list_teams(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(org): Path<String>,
) -> ApiResult<Json<Vec<api::TeamInfo>>> {
    let who = identify(&s, &headers).await?;
    let teams = s.access.teams(&who, &UserId::new(org)).await?;
    Ok(Json(teams.into_iter().map(team_info).collect()))
}

#[utoipa::path(post, path = "/v1/orgs/{org}/teams",
    params(("org" = String, Path, description = "the organization's name")),
    request_body = api::CreateTeamRequest,
    responses((status = 201, body = api::TeamInfo),
              (status = 400, body = api::ErrorBody, description = "invalid_request: bad slug, name or description"),
              (status = 403, body = api::ErrorBody, description = "not_org_owner, or a token without manage_roles or limited to repositories"),
              (status = 404, body = api::ErrorBody, description = "org_not_found"),
              (status = 409, body = api::ErrorBody, description = "team_exists")))]
pub(crate) async fn create_team(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(org): Path<String>,
    Json(req): Json<api::CreateTeamRequest>,
) -> ApiResult<(StatusCode, Json<api::TeamInfo>)> {
    let who = identify(&s, &headers).await?;
    let team = s
        .access
        .create_team(
            &who,
            &UserId::new(org),
            &req.slug,
            req.name.as_deref(),
            req.description.as_deref(),
        )
        .await?;
    Ok((StatusCode::CREATED, Json(team_info(team))))
}

#[utoipa::path(get, path = "/v1/orgs/{org}/teams/{team}",
    params(("org" = String, Path, description = "the organization's name"),
           ("team" = String, Path, description = "the team's slug")),
    responses((status = 200, body = api::TeamInfo),
              (status = 403, body = api::ErrorBody, description = "not_org_member"),
              (status = 404, body = api::ErrorBody, description = "org_not_found or team_not_found")))]
pub(crate) async fn get_team(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((org, team)): Path<(String, String)>,
) -> ApiResult<Json<api::TeamInfo>> {
    let who = identify(&s, &headers).await?;
    let team = s.access.team(&who, &UserId::new(org), &team).await?;
    Ok(Json(team_info(team)))
}

#[utoipa::path(patch, path = "/v1/orgs/{org}/teams/{team}",
    params(("org" = String, Path, description = "the organization's name"),
           ("team" = String, Path, description = "the team's slug")),
    request_body = api::UpdateTeamRequest,
    responses((status = 200, body = api::TeamInfo),
              (status = 400, body = api::ErrorBody, description = "invalid_request: bad name or description"),
              (status = 403, body = api::ErrorBody, description = "not_org_owner, or a token without manage_roles or limited to repositories"),
              (status = 404, body = api::ErrorBody, description = "org_not_found or team_not_found")))]
pub(crate) async fn update_team(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((org, team)): Path<(String, String)>,
    Json(req): Json<api::UpdateTeamRequest>,
) -> ApiResult<Json<api::TeamInfo>> {
    let who = identify(&s, &headers).await?;
    let team = s
        .access
        .update_team(
            &who,
            &UserId::new(org),
            &team,
            req.name.as_deref(),
            req.description.as_deref(),
        )
        .await?;
    Ok(Json(team_info(team)))
}

#[utoipa::path(delete, path = "/v1/orgs/{org}/teams/{team}",
    params(("org" = String, Path, description = "the organization's name"),
           ("team" = String, Path, description = "the team's slug")),
    responses((status = 204, description = "also removes the team's members and its roles on repositories"),
              (status = 403, body = api::ErrorBody, description = "not_org_owner, or a token without manage_roles or limited to repositories"),
              (status = 404, body = api::ErrorBody, description = "org_not_found or team_not_found")))]
pub(crate) async fn delete_team(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((org, team)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let who = identify(&s, &headers).await?;
    s.access.delete_team(&who, &UserId::new(org), &team).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(put, path = "/v1/orgs/{org}/teams/{team}/members/{user}",
    params(("org" = String, Path, description = "the organization's name"),
           ("team" = String, Path, description = "the team's slug"),
           ("user" = String, Path, description = "an organization member's user name")),
    responses((status = 204, description = "adding someone already in the team changes nothing"),
              (status = 403, body = api::ErrorBody, description = "not_org_owner, or a token without manage_roles or limited to repositories"),
              (status = 404, body = api::ErrorBody, description = "org_not_found or team_not_found"),
              (status = 409, body = api::ErrorBody, description = "user_not_org_member: add them to the organization first")))]
pub(crate) async fn add_team_member(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((org, team, user)): Path<(String, String, String)>,
) -> ApiResult<StatusCode> {
    let who = identify(&s, &headers).await?;
    s.access
        .add_team_member(&who, &UserId::new(org), &team, &UserId::new(user))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(delete, path = "/v1/orgs/{org}/teams/{team}/members/{user}",
    params(("org" = String, Path, description = "the organization's name"),
           ("team" = String, Path, description = "the team's slug"),
           ("user" = String, Path, description = "the member's user name")),
    responses((status = 204, description = "ends the access the person had through the team"),
              (status = 403, body = api::ErrorBody, description = "not_org_owner, or a token without manage_roles or limited to repositories"),
              (status = 404, body = api::ErrorBody, description = "org_not_found, team_not_found or team_member_not_found")))]
pub(crate) async fn remove_team_member(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((org, team, user)): Path<(String, String, String)>,
) -> ApiResult<StatusCode> {
    let who = identify(&s, &headers).await?;
    s.access
        .remove_team_member(&who, &UserId::new(org), &team, &UserId::new(user))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(get, path = "/v1/repos/{owner}/{name}/teams", params(RepoAddress),
    responses((status = 200, body = Vec<api::RepoTeam>, description = "the teams holding a role in the repository, ordered by slug; empty for a repository a user owns"),
              (status = 403, body = api::ErrorBody, description = "forbidden: needs manage_users"),
              (status = 404, body = api::ErrorBody, description = "repo_not_found")))]
pub(crate) async fn list_repo_teams(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
) -> ApiResult<Json<Vec<api::RepoTeam>>> {
    let (repo, who) = authorize_repo(&s, &headers, &owner, &name, None).await?;
    let teams = s.access.repo_teams(&who, &repo.record.id).await?;
    Ok(Json(
        teams
            .into_iter()
            .map(|(t, role)| api::RepoTeam {
                slug: t.slug,
                name: t.name,
                description: t.description,
                role: role.to_string(),
            })
            .collect(),
    ))
}

#[utoipa::path(put, path = "/v1/repos/{owner}/{name}/teams/{team}", params(RepoAddress,
    ("team" = String, Path, description = "the slug of a team of the owning organization")),
    request_body = api::SetTeamRoleRequest,
    responses((status = 204, description = "sets or changes the team's role"),
              (status = 400, body = api::ErrorBody, description = "invalid_request (bad role) or not_org_repo (a user owns the repository)"),
              (status = 403, body = api::ErrorBody, description = "forbidden: needs manage_users and every permission the role grants"),
              (status = 404, body = api::ErrorBody, description = "repo_not_found or team_not_found")))]
pub(crate) async fn set_repo_team(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name, team)): Path<(String, String, String)>,
    Json(req): Json<api::SetTeamRoleRequest>,
) -> ApiResult<StatusCode> {
    let (repo, who) = authorize_repo(&s, &headers, &owner, &name, None).await?;
    s.access
        .set_team_access(&who, &repo.record.id, &team, Role::from_str(&req.role)?)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(delete, path = "/v1/repos/{owner}/{name}/teams/{team}", params(RepoAddress,
    ("team" = String, Path, description = "the slug of a team of the owning organization")),
    responses((status = 204, description = "removing a team that holds no role changes nothing"),
              (status = 400, body = api::ErrorBody, description = "not_org_repo (a user owns the repository)"),
              (status = 403, body = api::ErrorBody, description = "forbidden: needs manage_users and every permission of the role being removed"),
              (status = 404, body = api::ErrorBody, description = "repo_not_found or team_not_found")))]
pub(crate) async fn remove_repo_team(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name, team)): Path<(String, String, String)>,
) -> ApiResult<StatusCode> {
    let (repo, who) = authorize_repo(&s, &headers, &owner, &name, None).await?;
    s.access
        .remove_team_access(&who, &repo.record.id, &team)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
