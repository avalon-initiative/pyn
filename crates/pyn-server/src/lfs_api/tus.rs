//! The tus 1.0 core protocol (creation, HEAD, PATCH, termination) serving the LFS `tus` transfer adapter, one upload
//! per oid at `.../objects/{oid}/tus`. See docs/concepts/lfs.md.

use axum::extract::{Path, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, LOCATION};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use pyn_core::{Permission, PynError, TusError, TusStatus};

use super::{LfsError, authorize_repo, check_oid, repo_name, request_origin, with_bearer};
use crate::{AppState, MAX_OBJECT_BYTES, upload};

const TUS_VERSION: &str = "1.0.0";
const OFFSET_TYPE: &str = "application/offset+octet-stream";
const RESUMABLE: HeaderName = HeaderName::from_static("tus-resumable");
const UPLOAD_OFFSET: HeaderName = HeaderName::from_static("upload-offset");
const UPLOAD_LENGTH: HeaderName = HeaderName::from_static("upload-length");
const UPLOAD_EXPIRES: HeaderName = HeaderName::from_static("upload-expires");

/// The tus headers every answer carries.
fn tus(mut res: Response) -> Response {
    let h = res.headers_mut();
    h.insert(RESUMABLE, HeaderValue::from_static(TUS_VERSION));
    h.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

fn status_headers(res: &mut Response, st: &TusStatus) {
    let h = res.headers_mut();
    h.insert(UPLOAD_OFFSET, HeaderValue::from(st.offset));
    h.insert(UPLOAD_LENGTH, HeaderValue::from(st.size));
    let expires = st.expires.format("%a, %d %b %Y %H:%M:%S GMT").to_string();
    if let Ok(v) = HeaderValue::from_str(&expires) {
        h.insert(UPLOAD_EXPIRES, v);
    }
}

fn number(headers: &HeaderMap, name: &HeaderName) -> Option<u64> {
    headers.get(name)?.to_str().ok()?.trim().parse().ok()
}

/// A request that speaks another tus version is refused with the version served.
fn version_refusal(headers: &HeaderMap) -> Option<Response> {
    match headers.get(RESUMABLE).and_then(|v| v.to_str().ok()) {
        Some(TUS_VERSION) => None,
        _ => {
            let mut res = StatusCode::PRECONDITION_FAILED.into_response();
            res.headers_mut()
                .insert("tus-version", HeaderValue::from_static(TUS_VERSION));
            Some(tus(res))
        }
    }
}

fn refuse(e: TusError) -> Response {
    let res = match e {
        TusError::NotFound => StatusCode::NOT_FOUND.into_response(),
        TusError::OffsetMismatch { .. } | TusError::Busy => StatusCode::CONFLICT.into_response(),
        TusError::TooLong => StatusCode::PAYLOAD_TOO_LARGE.into_response(),
        TusError::Pyn(e) => LfsError::Pyn(e).into_response(),
    };
    tus(res)
}

fn wrong(message: &str) -> Response {
    tus(LfsError::Malformed(message.into()).into_response())
}

pub(crate) async fn options() -> Response {
    let mut res = StatusCode::NO_CONTENT.into_response();
    let h = res.headers_mut();
    h.insert("tus-version", HeaderValue::from_static(TUS_VERSION));
    h.insert(
        "tus-extension",
        HeaderValue::from_static("creation,termination"),
    );
    tus(res)
}

pub(crate) async fn head(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name, oid)): Path<(String, String, String)>,
) -> Result<Response, LfsError> {
    let (repo, _) = authorize_repo(
        &s,
        &with_bearer(&headers),
        &owner,
        repo_name(&name),
        Some(Permission::Checkin),
    )
    .await?;
    check_oid(&oid)?;
    let Some(st) = repo.service.lfs_tus_status(&oid).await else {
        return Ok(tus(StatusCode::NOT_FOUND.into_response()));
    };
    let mut res = StatusCode::OK.into_response();
    status_headers(&mut res, &st);
    Ok(tus(res))
}

pub(crate) async fn create(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name, oid)): Path<(String, String, String)>,
) -> Result<Response, LfsError> {
    if let Some(res) = version_refusal(&headers) {
        return Ok(res);
    }
    let (repo, who) = authorize_repo(
        &s,
        &with_bearer(&headers),
        &owner,
        repo_name(&name),
        Some(Permission::Checkin),
    )
    .await?;
    check_oid(&oid)?;
    let Some(size) = number(&headers, &UPLOAD_LENGTH) else {
        return Ok(wrong("Upload-Length is required"));
    };
    let st = match repo
        .service
        .lfs_tus_create(&who.user, &oid, size, MAX_OBJECT_BYTES)
        .await
    {
        Ok(st) => st,
        Err(e) => return Ok(refuse(TusError::Pyn(e))),
    };
    let location = format!(
        "{}/v1/repos/{owner}/{name}/info/lfs/objects/{oid}/tus",
        request_origin(&s, &headers)
    );
    let mut res = StatusCode::CREATED.into_response();
    status_headers(&mut res, &st);
    if let Ok(v) = HeaderValue::from_str(&location) {
        res.headers_mut().insert(LOCATION, v);
    }
    Ok(tus(res))
}

pub(crate) async fn patch(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name, oid)): Path<(String, String, String)>,
    body: axum::body::Body,
) -> Result<Response, LfsError> {
    if let Some(res) = version_refusal(&headers) {
        return Ok(res);
    }
    let (repo, who) = authorize_repo(
        &s,
        &with_bearer(&headers),
        &owner,
        repo_name(&name),
        Some(Permission::Checkin),
    )
    .await?;
    check_oid(&oid)?;
    let is_offset_type = headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.trim().eq_ignore_ascii_case(OFFSET_TYPE));
    if !is_offset_type {
        return Ok(tus(StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response()));
    }
    let Some(offset) = number(&headers, &UPLOAD_OFFSET) else {
        return Ok(wrong("Upload-Offset is required"));
    };
    let permit = s.repos.upload_permit(&who.user)?;
    let (reader, len) = upload(&headers, body)?;
    // Detached, so a client that disconnects mid-body still has what arrived recorded.
    let service = repo.service.clone();
    let task = tokio::spawn(async move {
        let _permit = permit;
        service
            .lfs_tus_append(&who.user, &oid, offset, reader, len, MAX_OBJECT_BYTES)
            .await
    });
    let appended = match task.await {
        Ok(Ok(appended)) => appended,
        Ok(Err(e)) => return Ok(refuse(e)),
        Err(_) => return Err(PynError::Storage("the upload task failed".into()).into()),
    };
    let mut res = StatusCode::NO_CONTENT.into_response();
    status_headers(&mut res, &appended.status);
    res.headers_mut().remove(UPLOAD_LENGTH);
    Ok(tus(res))
}

pub(crate) async fn terminate(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name, oid)): Path<(String, String, String)>,
) -> Result<Response, LfsError> {
    if let Some(res) = version_refusal(&headers) {
        return Ok(res);
    }
    let (repo, who) = authorize_repo(
        &s,
        &with_bearer(&headers),
        &owner,
        repo_name(&name),
        Some(Permission::Checkin),
    )
    .await?;
    check_oid(&oid)?;
    match repo.service.lfs_tus_cancel(&who.user, &oid).await {
        Ok(()) => Ok(tus(StatusCode::NO_CONTENT.into_response())),
        Err(e) => Ok(refuse(e)),
    }
}
