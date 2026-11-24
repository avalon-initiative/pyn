//! Tags: listing, creating and deleting immutable names that point at a commit.

use axum::Json;
use axum::extract::Path;
use axum::http::StatusCode;
use pyn_core::Tag;
use pyn_core::repositories::OpenRepo;
use pyn_proto as api;

use crate::caller::{RepoCaller, RepoReader};
use crate::{ApiResult, RepoAddress};

/// The tag with the committed time of its commit, which the store's tag does not carry.
async fn tag_dto(repo: &OpenRepo, t: Tag) -> ApiResult<api::Tag> {
    let committed_at = repo.service.commit_time(&t.commit).await?;
    Ok(api::Tag {
        name: t.name,
        commit: t.commit.to_hex(),
        message: t.message,
        created_at: t.created_at,
        created_by: t.created_by.to_string(),
        committed_at,
    })
}

/// Readable without credentials when the repository is public; otherwise 404 unless the caller has a role.
#[utoipa::path(get, path = "/v1/repos/{owner}/{name}/tags", params(RepoAddress),
    responses((status = 200, body = Vec<api::Tag>, description = "every tag, ordered by name")))]
pub(crate) async fn list_tags(
    RepoReader { repo, .. }: RepoReader,
) -> ApiResult<Json<Vec<api::Tag>>> {
    let mut out = Vec::new();
    for t in repo.service.tags().await? {
        out.push(tag_dto(&repo, t).await?);
    }
    Ok(Json(out))
}

#[utoipa::path(get, path = "/v1/repos/{owner}/{name}/tags/{tag}",
    params(RepoAddress, ("tag" = String, Path, description = "the tag's name; a slash may be sent as is or as %2F")),
    responses((status = 200, body = api::Tag),
              (status = 404, body = api::ErrorBody, description = "tag_not_found")))]
pub(crate) async fn get_tag(
    RepoReader { repo, .. }: RepoReader,
    Path((_, _, tag)): Path<(String, String, String)>,
) -> ApiResult<Json<api::Tag>> {
    Ok(Json(tag_dto(&repo, repo.service.tag(&tag).await?).await?))
}

#[utoipa::path(post, path = "/v1/repos/{owner}/{name}/tags", params(RepoAddress),
    request_body = api::CreateTagRequest,
    responses((status = 200, body = api::Tag),
              (status = 400, body = api::ErrorBody, description = "invalid_tag_name, ambiguous_commit for the target, or invalid_request when the repository has no commit to tag"),
              (status = 404, body = api::ErrorBody, description = "commit_not_found: the target names no commit"),
              (status = 403, body = api::ErrorBody, description = "needs the checkin permission"),
              (status = 409, body = api::ErrorBody, description = "tag_exists: the name is taken, or is a folder of another tag's name; a tag is never moved")))]
/// Create a tag at a commit.
pub(crate) async fn create_tag(
    RepoCaller {
        repo,
        principal: who,
        ..
    }: RepoCaller,
    Json(req): Json<api::CreateTagRequest>,
) -> ApiResult<Json<api::Tag>> {
    let made = repo
        .service
        .create_tag(
            &who,
            &req.name,
            req.target.as_deref(),
            req.message.as_deref(),
        )
        .await?;
    Ok(Json(tag_dto(&repo, made).await?))
}

#[utoipa::path(delete, path = "/v1/repos/{owner}/{name}/tags/{tag}",
    params(RepoAddress, ("tag" = String, Path, description = "the tag's name; a slash may be sent as is or as %2F")),
    responses((status = 204),
              (status = 403, body = api::ErrorBody, description = "needs the checkin permission"),
              (status = 404, body = api::ErrorBody, description = "tag_not_found")))]
/// Delete a tag's name; its commit stays.
pub(crate) async fn delete_tag(
    RepoCaller {
        repo,
        principal: who,
        ..
    }: RepoCaller,
    Path((_, _, tag)): Path<(String, String, String)>,
) -> ApiResult<StatusCode> {
    repo.service.delete_tag(&who, &tag).await?;
    Ok(StatusCode::NO_CONTENT)
}
