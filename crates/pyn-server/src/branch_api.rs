//! Branches: listing, creating, renaming and deleting them.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use pyn_core::{Branch, Permission};
use pyn_proto as api;
use serde::Deserialize;

use crate::{ApiResult, AppState, RepoAddress, authorize_read, authorize_repo};

pub(crate) fn branch_dto(b: Branch) -> api::Branch {
    api::Branch {
        name: b.name,
        commit: b.head.map(|h| h.to_hex()),
        default: b.default,
        created_at: b.created_at,
        created_by: b.created_by.to_string(),
    }
}

/// Readable without credentials when the repository is public; otherwise 404 unless the caller has a role.
#[utoipa::path(get, path = "/v1/repos/{owner}/{name}/branches", params(RepoAddress),
    responses((status = 200, body = Vec<api::Branch>, description = "the default branch first, then by name")))]
pub(crate) async fn list_branches(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
) -> ApiResult<Json<Vec<api::Branch>>> {
    let repo = authorize_read(&s, &headers, &owner, &name).await?;
    let branches = repo.service.branches().await?;
    Ok(Json(branches.into_iter().map(branch_dto).collect()))
}

#[utoipa::path(get, path = "/v1/repos/{owner}/{name}/branches/{branch}",
    params(RepoAddress, ("branch" = String, Path, description = "the branch's name; a slash may be sent as is or as %2F")),
    responses((status = 200, body = api::Branch),
              (status = 404, body = api::ErrorBody, description = "branch_not_found")))]
pub(crate) async fn get_branch(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name, branch)): Path<(String, String, String)>,
) -> ApiResult<Json<api::Branch>> {
    let repo = authorize_read(&s, &headers, &owner, &name).await?;
    Ok(Json(branch_dto(repo.service.branch(&branch).await?)))
}

#[utoipa::path(post, path = "/v1/repos/{owner}/{name}/branches", params(RepoAddress),
    request_body = api::CreateBranchRequest,
    responses((status = 200, body = api::Branch),
              (status = 400, body = api::ErrorBody, description = "invalid_branch_name, commit_not_found or ambiguous_commit for the start point, or invalid_request when the repository has no commit to start from"),
              (status = 403, body = api::ErrorBody, description = "needs the checkin permission"),
              (status = 409, body = api::ErrorBody, description = "branch_exists: the name is taken, or is a folder of another branch's name")))]
/// Create a branch at a commit.
pub(crate) async fn create_branch(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    Json(req): Json<api::CreateBranchRequest>,
) -> ApiResult<Json<api::Branch>> {
    let (repo, who) =
        authorize_repo(&s, &headers, &owner, &name, Some(Permission::Checkin)).await?;
    let made = repo
        .service
        .create_branch(&who, &req.name, req.start.as_deref())
        .await?;
    Ok(Json(branch_dto(made)))
}

#[derive(Deserialize)]
pub(crate) struct DeleteQuery {
    force: Option<bool>,
}

#[utoipa::path(delete, path = "/v1/repos/{owner}/{name}/branches/{branch}",
    params(RepoAddress,
           ("branch" = String, Path, description = "the branch's name; a slash may be sent as is or as %2F"),
           ("force" = Option<bool>, Query, description = "delete a branch whose commits are not all on the default branch")),
    responses((status = 204),
              (status = 400, body = api::ErrorBody, description = "invalid_request for the default branch"),
              (status = 403, body = api::ErrorBody, description = "needs the checkin permission"),
              (status = 404, body = api::ErrorBody, description = "branch_not_found"),
              (status = 409, body = api::ErrorBody, description = "branch_not_merged without force")))]
/// Delete a branch's name; its commits stay.
pub(crate) async fn delete_branch(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name, branch)): Path<(String, String, String)>,
    Query(q): Query<DeleteQuery>,
) -> ApiResult<StatusCode> {
    let (repo, who) =
        authorize_repo(&s, &headers, &owner, &name, Some(Permission::Checkin)).await?;
    repo.service
        .delete_branch(&who, &branch, q.force.unwrap_or(false))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(patch, path = "/v1/repos/{owner}/{name}/branches/{branch}",
    params(RepoAddress, ("branch" = String, Path, description = "the branch's name; a slash may be sent as is or as %2F")),
    request_body = api::RenameBranchRequest,
    responses((status = 200, body = api::Branch),
              (status = 400, body = api::ErrorBody, description = "invalid_branch_name, or invalid_request for the default branch"),
              (status = 403, body = api::ErrorBody, description = "needs the checkin permission"),
              (status = 404, body = api::ErrorBody, description = "branch_not_found"),
              (status = 409, body = api::ErrorBody, description = "branch_exists")))]
/// Rename a branch other than the default one.
pub(crate) async fn rename_branch(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name, branch)): Path<(String, String, String)>,
    Json(req): Json<api::RenameBranchRequest>,
) -> ApiResult<Json<api::Branch>> {
    let (repo, who) =
        authorize_repo(&s, &headers, &owner, &name, Some(Permission::Checkin)).await?;
    let renamed = repo.service.rename_branch(&who, &branch, &req.name).await?;
    Ok(Json(branch_dto(renamed)))
}
