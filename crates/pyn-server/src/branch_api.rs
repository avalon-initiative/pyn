//! Branches: listing, creating, renaming and deleting them.

use axum::Json;
use axum::extract::{Path, Query};
use axum::http::StatusCode;
use pyn_core::repositories::OpenRepo;
use pyn_core::{Branch, COMPARE_MAX_COMMITS, COMPARE_MAX_PATHS, ChangeKind};
use pyn_proto as api;
use serde::Deserialize;

use crate::caller::{RepoCaller, RepoReader};
use crate::{ApiResult, RepoAddress};

/// The branch with the committed time of its head, which the store's branch does not carry.
async fn branch_dto(repo: &OpenRepo, b: Branch) -> ApiResult<api::Branch> {
    let updated_at = repo.service.branch_updated_at(&b).await?;
    Ok(api::Branch {
        name: b.name,
        commit: b.head.map(|h| h.to_hex()),
        default: b.default,
        created_at: b.created_at,
        created_by: b.created_by.to_string(),
        updated_at,
    })
}

/// Readable without credentials when the repository is public; otherwise 404 unless the caller has a role.
#[utoipa::path(get, path = "/v1/repos/{owner}/{name}/branches", params(RepoAddress),
    responses((status = 200, body = Vec<api::Branch>, description = "the default branch first, then by name")))]
pub(crate) async fn list_branches(
    RepoReader { repo, .. }: RepoReader,
) -> ApiResult<Json<Vec<api::Branch>>> {
    let mut out = Vec::new();
    for b in repo.service.branches().await? {
        out.push(branch_dto(&repo, b).await?);
    }
    Ok(Json(out))
}

#[utoipa::path(get, path = "/v1/repos/{owner}/{name}/branches/{branch}",
    params(RepoAddress, ("branch" = String, Path, description = "the branch's name; a slash may be sent as is or as %2F")),
    responses((status = 200, body = api::Branch),
              (status = 404, body = api::ErrorBody, description = "branch_not_found")))]
pub(crate) async fn get_branch(
    RepoReader { repo, .. }: RepoReader,
    Path((_, _, branch)): Path<(String, String, String)>,
) -> ApiResult<Json<api::Branch>> {
    Ok(Json(
        branch_dto(&repo, repo.service.branch(&branch).await?).await?,
    ))
}

#[utoipa::path(post, path = "/v1/repos/{owner}/{name}/branches", params(RepoAddress),
    request_body = api::CreateBranchRequest,
    responses((status = 200, body = api::Branch),
              (status = 400, body = api::ErrorBody, description = "invalid_branch_name, commit_not_found or ambiguous_commit for the start point, or invalid_request when the repository has no commit to start from"),
              (status = 403, body = api::ErrorBody, description = "needs the checkin permission"),
              (status = 409, body = api::ErrorBody, description = "branch_exists: the name is taken, or is a folder of another branch's name")))]
/// Create a branch at a commit.
pub(crate) async fn create_branch(
    RepoCaller {
        repo,
        principal: who,
        ..
    }: RepoCaller,
    Json(req): Json<api::CreateBranchRequest>,
) -> ApiResult<Json<api::Branch>> {
    let made = repo
        .service
        .create_branch(&who, &req.name, req.start.as_deref())
        .await?;
    Ok(Json(branch_dto(&repo, made).await?))
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
    RepoCaller {
        repo,
        principal: who,
        ..
    }: RepoCaller,
    Path((_, _, branch)): Path<(String, String, String)>,
    Query(q): Query<DeleteQuery>,
) -> ApiResult<StatusCode> {
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
    RepoCaller {
        repo,
        principal: who,
        ..
    }: RepoCaller,
    Path((_, _, branch)): Path<(String, String, String)>,
    Json(req): Json<api::RenameBranchRequest>,
) -> ApiResult<Json<api::Branch>> {
    let renamed = repo.service.rename_branch(&who, &branch, &req.name).await?;
    Ok(Json(branch_dto(&repo, renamed).await?))
}

#[derive(Deserialize)]
pub(crate) struct CompareQuery {
    base: Option<String>,
    head: String,
}

/// Readable without credentials when the repository is public; otherwise 404 unless the caller has a role.
#[utoipa::path(get, path = "/v1/repos/{owner}/{name}/compare",
    params(RepoAddress,
           ("base" = Option<String>, Query, description = "a branch, or a commit id or abbreviation; the default branch if omitted"),
           ("head" = String, Query, description = "a branch, or a commit id or abbreviation")),
    responses((status = 200, body = api::BranchComparison),
              (status = 400, body = api::ErrorBody, description = "invalid_request for a branch with no commits or a history too long to walk, ambiguous_commit"),
              (status = 404, body = api::ErrorBody, description = "commit_not_found: neither a branch nor a commit")))]
/// How far `head` is from `base`: commits ahead and behind, and the paths it changes since their merge base.
pub(crate) async fn compare(
    RepoReader { repo, .. }: RepoReader,
    Query(q): Query<CompareQuery>,
) -> ApiResult<Json<api::BranchComparison>> {
    let c = repo.service.compare(q.base.as_deref(), &q.head).await?;
    let status = match (c.ahead, c.behind) {
        (0, 0) => api::CompareStatus::Identical,
        (_, 0) => api::CompareStatus::Ahead,
        (0, _) => api::CompareStatus::Behind,
        _ => api::CompareStatus::Diverged,
    };
    Ok(Json(api::BranchComparison {
        base: c.base,
        base_commit: c.base_commit.to_hex(),
        head: c.head,
        head_commit: c.head_commit.to_hex(),
        merge_base: c.merge_base.map(|m| m.to_hex()),
        status,
        ahead: c.ahead,
        behind: c.behind,
        max_commits: COMPARE_MAX_COMMITS as u32,
        counts_capped: c.counts_capped,
        changes: c
            .changes
            .into_iter()
            .map(|c| api::CompareChange {
                path: c.path.to_string(),
                kind: match c.kind {
                    ChangeKind::Added => api::CompareChangeKind::Added,
                    ChangeKind::Modified => api::CompareChangeKind::Modified,
                    ChangeKind::Deleted => api::CompareChangeKind::Deleted,
                },
            })
            .collect(),
        max_changes: COMPARE_MAX_PATHS as u32,
        truncated: c.changes_truncated,
    }))
}
