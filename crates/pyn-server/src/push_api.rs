//! Receiving client-built commits, listing what a client lacks, and reading a blob by id.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::Response;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use pyn_core::{FetchQuery, ObjectId, Permission, PushObjects, PynError};
use pyn_proto as api;

use crate::{
    ApiResult, AppState, FileContent, RepoAddress, authorize_read, authorize_repo, branch_of,
    parse_object_id, parse_range, range_not_satisfiable, revision_dto, stranded_dto,
};

/// The largest push body: the encoded commits and trees, never file content.
pub(crate) const MAX_PUSH_BYTES: usize = 64 * 1024 * 1024;

fn decode(objects: Vec<api::PushObject>) -> Result<Vec<(ObjectId, Vec<u8>)>, PynError> {
    objects
        .into_iter()
        .map(|o| {
            let bytes = STANDARD.decode(o.data.as_bytes()).map_err(|_| {
                PynError::InvalidRequest(format!("the data of {} is not base64", o.id))
            })?;
            Ok((parse_object_id(&o.id)?, bytes))
        })
        .collect()
}

/// Append client-built commits to a branch, all or none.
#[utoipa::path(post, path = "/v1/repos/{owner}/{name}/push", params(RepoAddress), request_body = api::PushRequest, responses(
    (status = 200, body = api::PushResponse),
    (status = 409, body = api::ErrorBody, description = "not_fast_forward when the first commit is not a child of the branch's head (or a racing push won); lock_required or lock_held for an exclusive path the pusher does not hold a live lock on; stale_base; storage_limit_reached"),
    (status = 400, body = api::ErrorBody, description = "push_invalid for an id that does not match its bytes, bytes that are not a git object, a missing tree or a commit already stored; object_missing for a blob that was not uploaded; invalid_request for an empty push or bad base64; invalid_rules for a policy file on the default branch that does not validate"),
    (status = 422, body = api::ErrorBody, description = "push_unsupported: a merge commit, a submodule, an empty commit, or the removal of the policy file from the default branch"),
    (status = 404, body = api::ErrorBody, description = "branch_not_found or commit_not_found for an unknown parent"),
    (status = 403, body = api::ErrorBody, description = "forbidden without the checkin permission, or without the policy permission for a commit that changes the policy file on the default branch"),
))]
pub(crate) async fn push(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    Json(req): Json<api::PushRequest>,
) -> ApiResult<Json<api::PushResponse>> {
    let (repo, who) =
        authorize_repo(&s, &headers, &owner, &name, Some(Permission::Checkin)).await?;
    let branch = branch_of(req.branch.as_deref());
    let objects = PushObjects {
        commits: decode(req.commits)?,
        trees: decode(req.trees)?,
    };
    let pushed = repo
        .service
        .push_commits_recorded(&who, branch, objects)
        .await?;
    let head = repo
        .service
        .head_of(branch)
        .await?
        .ok_or_else(|| PynError::BranchNotFound(branch.to_string()))?;
    Ok(Json(api::PushResponse {
        branch: branch.to_string(),
        head: head.to_hex(),
        revisions: pushed.revisions.into_iter().map(revision_dto).collect(),
        stranded: pushed.stranded.into_iter().map(stranded_dto).collect(),
    }))
}

/// The commits and trees a client lacks, oldest first, a page at a time.
#[utoipa::path(post, path = "/v1/repos/{owner}/{name}/fetch", params(RepoAddress), request_body = api::FetchRequest, responses(
    (status = 200, body = api::FetchResponse),
    (status = 400, body = api::ErrorBody, description = "invalid_request for a malformed id, a cursor this fetch did not give or too many haves"),
    (status = 404, body = api::ErrorBody, description = "branch_not_found for a branch the repository does not have"),
))]
pub(crate) async fn fetch(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    Json(req): Json<api::FetchRequest>,
) -> ApiResult<Json<api::FetchResponse>> {
    let repo = authorize_read(&s, &headers, &owner, &name).await?;
    let query = FetchQuery {
        branches: req.branches,
        haves: req
            .haves
            .iter()
            .map(|h| parse_object_id(h))
            .collect::<Result<_, _>>()?,
        cursor: req.cursor,
        limit: req.limit.map(|l| l as usize),
    };
    let page = repo.service.fetch(&query).await?;
    let encode = |bytes: &[u8]| STANDARD.encode(bytes);
    Ok(Json(api::FetchResponse {
        refs: page
            .refs
            .into_iter()
            .map(|(branch, head)| api::FetchRef {
                branch,
                head: head.map(|h| h.to_hex()),
            })
            .collect(),
        commits: page
            .commits
            .into_iter()
            .map(|c| api::FetchCommit {
                id: c.id.to_hex(),
                data: encode(&c.raw),
                pointers: c.pointers.into_iter().map(revision_dto).collect(),
                trees: c
                    .trees
                    .iter()
                    .map(|(id, body)| api::PushObject {
                        id: id.to_hex(),
                        data: encode(body),
                    })
                    .collect(),
            })
            .collect(),
        next: page.next,
    }))
}

/// Readable without credentials when the repository is public; otherwise 404 unless the caller has a role.
#[utoipa::path(get, path = "/v1/repos/{owner}/{name}/objects/{id}",
    params(RepoAddress, ("id" = String, Path, description = "the blob's git id")),
    responses((status = 200, content_type = "application/octet-stream", body = inline(FileContent)),
              (status = 206, content_type = "application/octet-stream", body = inline(FileContent), description = "the part named by a single-range `Range: bytes=` header"),
              (status = 416, description = "the range lies outside the content; `Content-Range: bytes */<length>`"),
              (status = 400, body = api::ErrorBody, description = "invalid_request for a malformed id"),
              (status = 404, body = api::ErrorBody, description = "object_not_found when no revision of the repository has the blob")))]
pub(crate) async fn get_object(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name, id)): Path<(String, String, String)>,
) -> ApiResult<Response> {
    let repo = authorize_read(&s, &headers, &owner, &name).await?;
    let id = parse_object_id(&id)?;
    let wanted = headers
        .get(axum::http::header::RANGE)
        .and_then(|v| v.to_str().ok());
    let range = match wanted {
        Some(text) => {
            let found = repo.service.read_blob(&id, Some(0..0)).await?;
            match parse_range(text, found.total) {
                Some(r) => Some(r),
                None => return Ok(range_not_satisfiable(found.total)),
            }
        }
        None => None,
    };
    let found = repo.service.read_blob(&id, range.clone()).await?;
    let mut res = Response::new(axum::body::Body::from_stream(
        tokio_util::io::ReaderStream::new(found.reader),
    ));
    if range.is_some() {
        *res.status_mut() = StatusCode::PARTIAL_CONTENT;
    }
    let h = res.headers_mut();
    h.insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    h.insert(
        axum::http::HeaderName::from_static(api::CONTENT_HASH_HEADER),
        HeaderValue::from_str(&id.to_hex()).expect("hex is ASCII"),
    );
    h.insert(
        axum::http::header::ACCEPT_RANGES,
        HeaderValue::from_static("bytes"),
    );
    h.insert(
        axum::http::header::CONTENT_LENGTH,
        HeaderValue::from(found.range.end - found.range.start),
    );
    if range.is_some() {
        let content_range = format!(
            "bytes {}-{}/{}",
            found.range.start,
            found.range.end.saturating_sub(1),
            found.total
        );
        h.insert(
            axum::http::header::CONTENT_RANGE,
            HeaderValue::from_str(&content_range).expect("ASCII"),
        );
    }
    Ok(res)
}
