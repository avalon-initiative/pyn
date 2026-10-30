//! The Git LFS Locks API over pyn locks, mounted under `/v1/repos/{owner}/{name}/info/lfs`. See docs/concepts/lfs.md.

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE, WWW_AUTHENTICATE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use pyn_core::{Lock, Permission, PynError, RepoPath, is_lfs_oid};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};

use crate::{
    ApiError, AppState, MAX_OBJECT_BYTES, authorize_repo, is_internal, parse_range, public_message,
    range_not_satisfiable, status_for, upload,
};

const LFS_TYPE: &str = "application/vnd.git-lfs+json";
const DEFAULT_LIMIT: usize = 100;
const MAX_LIMIT: usize = 1000;

#[derive(Serialize)]
struct LfsLock {
    id: String,
    path: String,
    locked_at: DateTime<Utc>,
    owner: LfsOwner,
}

#[derive(Serialize)]
struct LfsOwner {
    name: String,
}

#[derive(Serialize)]
struct LockBody {
    lock: LfsLock,
}

#[derive(Serialize)]
struct Conflict {
    lock: Option<LfsLock>,
    message: String,
}

#[derive(Serialize)]
struct LockList {
    locks: Vec<LfsLock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_cursor: Option<String>,
}

#[derive(Serialize)]
struct VerifyList {
    ours: Vec<LfsLock>,
    theirs: Vec<LfsLock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_cursor: Option<String>,
}

#[derive(Serialize)]
struct ErrorBody {
    message: String,
}

#[derive(Deserialize)]
struct CreateRequest {
    path: String,
}

#[derive(Deserialize, Default)]
struct VerifyRequest {
    cursor: Option<String>,
    limit: Option<usize>,
}

#[derive(Deserialize, Default)]
struct UnlockRequest {
    #[serde(default)]
    force: bool,
}

#[derive(Deserialize)]
pub(crate) struct ListQuery {
    path: Option<String>,
    id: Option<String>,
    cursor: Option<String>,
    limit: Option<usize>,
}

/// A response in the LFS media type.
struct Lfs<T>(StatusCode, T);

impl<T: Serialize> IntoResponse for Lfs<T> {
    fn into_response(self) -> Response {
        let mut res = (self.0, serde_json::to_vec(&self.1).unwrap_or_default()).into_response();
        res.headers_mut()
            .insert(CONTENT_TYPE, HeaderValue::from_static(LFS_TYPE));
        res
    }
}

pub(crate) enum LfsError {
    Pyn(PynError),
    Malformed(String),
    NotFound(String),
    UnsupportedMedia,
}

impl LfsError {
    fn message(&self) -> String {
        match self {
            Self::Malformed(m) | Self::NotFound(m) => m.clone(),
            Self::Pyn(e) => public_message(e),
            Self::UnsupportedMedia => format!("the request body must be {LFS_TYPE}"),
        }
    }
}

impl From<PynError> for LfsError {
    fn from(e: PynError) -> Self {
        Self::Pyn(e)
    }
}

impl From<ApiError> for LfsError {
    fn from(e: ApiError) -> Self {
        Self::Pyn(e.0)
    }
}

impl IntoResponse for LfsError {
    fn into_response(self) -> Response {
        let (status, message, internal) = match &self {
            Self::Pyn(e) => (lfs_status(e), public_message(e), is_internal(e)),
            Self::Malformed(m) => (StatusCode::UNPROCESSABLE_ENTITY, m.clone(), false),
            Self::NotFound(m) => (StatusCode::NOT_FOUND, m.clone(), false),
            Self::UnsupportedMedia => (
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                format!("the request body must be {LFS_TYPE}"),
                false,
            ),
        };
        let mut res = Lfs(status, ErrorBody { message }).into_response();
        if status == StatusCode::UNAUTHORIZED {
            res.headers_mut().insert(
                WWW_AUTHENTICATE,
                HeaderValue::from_static("Basic realm=\"pyn\""),
            );
        }
        if let (true, Self::Pyn(e)) = (internal, &self) {
            res.extensions_mut()
                .insert(crate::InternalDetail(e.to_string()));
        }
        res
    }
}

type LfsResult = Result<Response, LfsError>;

/// LFS clients expect 413 for a store that is full and 422 for content that is not what was declared.
fn lfs_status(e: &PynError) -> StatusCode {
    match e {
        PynError::StorageLimitReached { .. } | PynError::RepoStorageLimitReached { .. } => {
            StatusCode::PAYLOAD_TOO_LARGE
        }
        PynError::LengthMismatch { .. } => StatusCode::UNPROCESSABLE_ENTITY,
        other => status_for(other),
    }
}

/// A write must declare a JSON type, so a cross-site form post (which cannot) is refused before it is read.
fn require_json(headers: &HeaderMap) -> Result<(), LfsError> {
    let essence = headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .map(|v| v.trim().to_ascii_lowercase());
    match essence.as_deref() {
        Some(LFS_TYPE | "application/json") => Ok(()),
        _ => Err(LfsError::UnsupportedMedia),
    }
}

/// git-lfs sends the token as the Basic password; the rest of the server reads a Bearer one.
fn with_bearer(headers: &HeaderMap) -> HeaderMap {
    let mut out = headers.clone();
    let token = headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("basic"))
        .and_then(|(_, b64)| STANDARD.decode(b64.trim()).ok())
        .and_then(|raw| String::from_utf8(raw).ok())
        .and_then(|cred| cred.split_once(':').map(|(_, pw)| pw.to_string()));
    if let Some(value) = token.and_then(|t| HeaderValue::from_str(&format!("Bearer {t}")).ok()) {
        out.insert(AUTHORIZATION, value);
    }
    out
}

/// `name.git` addresses the repository `name`, as git-lfs derives the endpoint from a remote URL.
fn repo_name(name: &str) -> &str {
    name.strip_suffix(".git").unwrap_or(name)
}

fn parse<T: DeserializeOwned + Default>(body: &Bytes) -> Result<T, LfsError> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(T::default());
    }
    serde_json::from_slice(body).map_err(|e| LfsError::Malformed(format!("invalid request: {e}")))
}

/// A stable id: the same path always has the same id.
fn lock_id(path: &RepoPath) -> String {
    let digest = Sha256::digest([b"pyn-lfs-lock\0", path.as_str().as_bytes()].concat());
    hex::encode(&digest[..16])
}

fn lfs_lock(l: &Lock) -> LfsLock {
    LfsLock {
        id: lock_id(&l.path),
        path: l.path.to_string(),
        locked_at: l.acquired_at,
        owner: LfsOwner {
            name: l.owner.to_string(),
        },
    }
}

fn encode_cursor(path: &RepoPath) -> String {
    URL_SAFE_NO_PAD.encode(path.as_str())
}

fn decode_cursor(cursor: &str) -> Result<String, LfsError> {
    URL_SAFE_NO_PAD
        .decode(cursor)
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .ok_or_else(|| PynError::InvalidRequest(format!("invalid cursor {cursor:?}")).into())
}

/// One page of `locks` ordered by path, after `cursor`, and the cursor of the next page.
fn page(
    mut locks: Vec<Lock>,
    cursor: Option<&str>,
    limit: Option<usize>,
) -> Result<(Vec<Lock>, Option<String>), LfsError> {
    locks.sort_by(|a, b| a.path.cmp(&b.path));
    if let Some(after) = cursor.filter(|c| !c.is_empty()).map(decode_cursor) {
        let after = after?;
        locks.retain(|l| l.path.as_str() > after.as_str());
    }
    let limit = limit
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_LIMIT)
        .min(MAX_LIMIT);
    let next = (locks.len() > limit).then(|| encode_cursor(&locks[limit - 1].path));
    locks.truncate(limit);
    Ok((locks, next))
}

fn conflict(lock: Option<Lock>) -> Response {
    let message = match &lock {
        Some(l) => format!("{} is locked by {}", l.path, l.owner),
        None => "the path could not be locked".to_string(),
    };
    Lfs(
        StatusCode::CONFLICT,
        Conflict {
            lock: lock.as_ref().map(lfs_lock),
            message,
        },
    )
    .into_response()
}

pub(crate) async fn create_lock(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    body: Bytes,
) -> LfsResult {
    require_json(&headers)?;
    let req: CreateRequest = serde_json::from_slice(&body)
        .map_err(|e| LfsError::Malformed(format!("invalid request: {e}")))?;
    let (repo, who) = authorize_repo(
        &s,
        &with_bearer(&headers),
        &owner,
        repo_name(&name),
        Some(Permission::Lock),
    )
    .await?;
    let path = RepoPath::new(req.path)?;
    if let Some(existing) = repo.service.lock_of(&path).await? {
        return Ok(conflict(Some(existing)));
    }
    match repo.service.lock_path(&path, &who.user).await {
        Ok(lock) => Ok(Lfs(
            StatusCode::CREATED,
            LockBody {
                lock: lfs_lock(&lock),
            },
        )
        .into_response()),
        Err(PynError::LockHeld { .. }) => Ok(conflict(repo.service.lock_of(&path).await?)),
        Err(e) => Err(e.into()),
    }
}

pub(crate) async fn list_locks(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    Query(q): Query<ListQuery>,
) -> LfsResult {
    let (repo, _) = authorize_repo(
        &s,
        &with_bearer(&headers),
        &owner,
        repo_name(&name),
        Some(Permission::Read),
    )
    .await?;
    let mut locks = repo.service.locks().await?;
    if let Some(path) = &q.path {
        locks.retain(|l| l.path.as_str() == path);
    }
    if let Some(id) = &q.id {
        locks.retain(|l| &lock_id(&l.path) == id);
    }
    let (locks, next_cursor) = page(locks, q.cursor.as_deref(), q.limit)?;
    Ok(Lfs(
        StatusCode::OK,
        LockList {
            locks: locks.iter().map(lfs_lock).collect(),
            next_cursor,
        },
    )
    .into_response())
}

pub(crate) async fn verify_locks(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    body: Bytes,
) -> LfsResult {
    require_json(&headers)?;
    let req: VerifyRequest = parse(&body)?;
    let (repo, who) = authorize_repo(
        &s,
        &with_bearer(&headers),
        &owner,
        repo_name(&name),
        Some(Permission::Read),
    )
    .await?;
    let (locks, next_cursor) = page(
        repo.service.locks().await?,
        req.cursor.as_deref(),
        req.limit,
    )?;
    let (ours, theirs): (Vec<_>, Vec<_>) = locks.iter().partition(|l| l.owner == who.user);
    Ok(Lfs(
        StatusCode::OK,
        VerifyList {
            ours: ours.into_iter().map(lfs_lock).collect(),
            theirs: theirs.into_iter().map(lfs_lock).collect(),
            next_cursor,
        },
    )
    .into_response())
}

pub(crate) async fn unlock(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name, id)): Path<(String, String, String)>,
    body: Bytes,
) -> LfsResult {
    require_json(&headers)?;
    let req: UnlockRequest = parse(&body)?;
    let (repo, who) = authorize_repo(
        &s,
        &with_bearer(&headers),
        &owner,
        repo_name(&name),
        Some(Permission::Lock),
    )
    .await?;
    let lock = repo
        .service
        .locks()
        .await?
        .into_iter()
        .find(|l| lock_id(&l.path) == id)
        .ok_or_else(|| LfsError::NotFound(format!("no lock with id {id}")))?;
    if req.force && lock.owner != who.user {
        who.require(Permission::ForceUnlock)?;
        repo.service
            .force_unlock(&lock.path, &who.user, "forced through the LFS API")
            .await?;
    } else {
        repo.service.release(&lock.path, &who.user).await?;
    }
    Ok(Lfs(
        StatusCode::OK,
        LockBody {
            lock: lfs_lock(&lock),
        },
    )
    .into_response())
}

const MAX_BATCH_OBJECTS: usize = 1000;

#[derive(Deserialize)]
pub(crate) struct BatchRequest {
    operation: String,
    #[serde(default)]
    transfers: Vec<String>,
    #[serde(default)]
    objects: Vec<ObjectSpec>,
    hash_algo: Option<String>,
}

#[derive(Deserialize)]
struct ObjectSpec {
    oid: String,
    size: i64,
}

#[derive(Serialize)]
struct BatchResponse {
    transfer: &'static str,
    objects: Vec<BatchObject>,
    hash_algo: &'static str,
}

#[derive(Serialize)]
struct BatchObject {
    oid: String,
    size: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    authenticated: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    actions: Option<Actions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ObjectError>,
}

#[derive(Serialize, Default)]
struct Actions {
    #[serde(skip_serializing_if = "Option::is_none")]
    download: Option<Action>,
    #[serde(skip_serializing_if = "Option::is_none")]
    upload: Option<Action>,
    #[serde(skip_serializing_if = "Option::is_none")]
    verify: Option<Action>,
}

#[derive(Serialize)]
struct Action {
    href: String,
}

#[derive(Serialize)]
struct ObjectError {
    code: u16,
    message: String,
}

#[derive(Deserialize)]
struct VerifyObject {
    oid: String,
    size: i64,
}

fn object_error(code: StatusCode, message: impl Into<String>) -> ObjectError {
    ObjectError {
        code: code.as_u16(),
        message: message.into(),
    }
}

/// `scheme://host` the client reached the server by: the forwarded values when proxies are trusted, else `Host`.
fn request_origin(s: &AppState, headers: &HeaderMap) -> String {
    let named = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(',').next())
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty() && !v.contains(['/', '@', ' ']))
    };
    let host = s
        .trust_forwarded
        .then(|| named("x-forwarded-host"))
        .flatten()
        .or_else(|| named("host"));
    let public = s.access.public_url();
    let public = public.trim_end_matches('/');
    let Some(host) = host else {
        return public.to_string();
    };
    let scheme = s
        .trust_forwarded
        .then(|| named("x-forwarded-proto"))
        .flatten()
        .filter(|p| matches!(p.as_str(), "http" | "https"))
        .unwrap_or_else(|| match public.split_once("://") {
            Some((scheme, rest)) if rest.eq_ignore_ascii_case(&host) => scheme.to_string(),
            _ => "http".to_string(),
        });
    format!("{scheme}://{host}")
}

fn check_oid(oid: &str) -> Result<(), LfsError> {
    if is_lfs_oid(oid) {
        Ok(())
    } else {
        Err(LfsError::Malformed(format!(
            "{oid:?} is not a sha256 oid (64 lowercase hex digits)"
        )))
    }
}

fn check_size(size: i64) -> Result<u64, LfsError> {
    u64::try_from(size).map_err(|_| LfsError::Malformed(format!("invalid size {size}")))
}

pub(crate) async fn batch(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    body: Bytes,
) -> LfsResult {
    require_json(&headers)?;
    let req: BatchRequest = serde_json::from_slice(&body)
        .map_err(|e| LfsError::Malformed(format!("invalid request: {e}")))?;
    let upload = match req.operation.as_str() {
        "upload" => true,
        "download" => false,
        other => {
            return Err(LfsError::Malformed(format!(
                "operation must be upload or download, not {other:?}"
            )));
        }
    };
    if !req.transfers.is_empty() && !req.transfers.iter().any(|t| t == "basic") {
        return Err(LfsError::Malformed(
            "only the basic transfer adapter is supported".into(),
        ));
    }
    if req.hash_algo.as_deref().is_some_and(|h| h != "sha256") {
        return Ok(Lfs(
            StatusCode::CONFLICT,
            ErrorBody {
                message: "only the sha256 hash algorithm is supported".into(),
            },
        )
        .into_response());
    }
    if req.objects.len() > MAX_BATCH_OBJECTS {
        return Err(LfsError::Malformed(format!(
            "a batch names at most {MAX_BATCH_OBJECTS} objects"
        )));
    }
    let need = if upload {
        Permission::Checkin
    } else {
        Permission::Read
    };
    let (repo, _) = authorize_repo(
        &s,
        &with_bearer(&headers),
        &owner,
        repo_name(&name),
        Some(need),
    )
    .await?;
    if upload {
        repo.service.ensure_writable().await?;
    }
    let base = format!(
        "{}/v1/repos/{owner}/{name}/info/lfs/objects",
        request_origin(&s, &headers)
    );
    let mut objects = Vec::with_capacity(req.objects.len());
    let mut pending = 0u64;
    for spec in req.objects {
        let mut out = BatchObject {
            oid: spec.oid.clone(),
            size: spec.size,
            authenticated: None,
            actions: None,
            error: None,
        };
        let size = match check_oid(&spec.oid).and_then(|()| check_size(spec.size)) {
            Ok(size) => size,
            Err(e) => {
                out.error = Some(object_error(StatusCode::UNPROCESSABLE_ENTITY, e.message()));
                objects.push(out);
                continue;
            }
        };
        let have = repo.service.lfs_object(&spec.oid).await?;
        match (upload, have) {
            (_, Some(have)) if have.size != size => {
                out.error = Some(object_error(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    format!("{} has {} bytes, not {size}", spec.oid, have.size),
                ));
            }
            (true, Some(_)) => {}
            (false, Some(_)) => {
                out.authenticated = Some(true);
                out.actions = Some(Actions {
                    download: Some(Action {
                        href: format!("{base}/{}", spec.oid),
                    }),
                    ..Actions::default()
                });
            }
            (false, None) => {
                out.error = Some(object_error(
                    StatusCode::NOT_FOUND,
                    format!("object {} not found", spec.oid),
                ));
            }
            (true, None) if size > MAX_OBJECT_BYTES => {
                out.error = Some(object_error(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    format!("an object is at most {MAX_OBJECT_BYTES} bytes"),
                ));
            }
            (true, None) => match repo.service.lfs_admit(pending.saturating_add(size)).await {
                Ok(()) => {
                    pending = pending.saturating_add(size);
                    out.authenticated = Some(true);
                    out.actions = Some(Actions {
                        upload: Some(Action {
                            href: format!("{base}/{}", spec.oid),
                        }),
                        verify: Some(Action {
                            href: format!("{base}/verify"),
                        }),
                        ..Actions::default()
                    });
                }
                Err(e) => {
                    let (status, message) = (lfs_status(&e), public_message(&e));
                    out.error = Some(object_error(status, message));
                }
            },
        }
        objects.push(out);
    }
    Ok(Lfs(
        StatusCode::OK,
        BatchResponse {
            transfer: "basic",
            objects,
            hash_algo: "sha256",
        },
    )
    .into_response())
}

pub(crate) async fn upload_object(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name, oid)): Path<(String, String, String)>,
    body: axum::body::Body,
) -> LfsResult {
    let (repo, who) = authorize_repo(
        &s,
        &with_bearer(&headers),
        &owner,
        repo_name(&name),
        Some(Permission::Checkin),
    )
    .await?;
    check_oid(&oid)?;
    let _permit = s.repos.upload_permit(&who.user)?;
    let (reader, size) = upload(&headers, body)?;
    repo.service
        .lfs_upload(&who.user, &oid, reader, size, Some(MAX_OBJECT_BYTES))
        .await?;
    Ok(StatusCode::OK.into_response())
}

pub(crate) async fn download(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name, oid)): Path<(String, String, String)>,
) -> LfsResult {
    let (repo, _) = authorize_repo(
        &s,
        &with_bearer(&headers),
        &owner,
        repo_name(&name),
        Some(Permission::Read),
    )
    .await?;
    check_oid(&oid)?;
    let object = repo
        .service
        .lfs_object(&oid)
        .await?
        .ok_or_else(|| LfsError::NotFound(format!("object {oid} not found")))?;
    let range = match headers
        .get(axum::http::header::RANGE)
        .and_then(|v| v.to_str().ok())
    {
        Some(text) => match parse_range(text, object.size) {
            Some(r) => Some(r),
            None => return Ok(range_not_satisfiable(object.size)),
        },
        None => None,
    };
    let (_, found) = repo.service.lfs_read(&oid, range.clone()).await?;
    let mut res = Response::new(axum::body::Body::from_stream(
        tokio_util::io::ReaderStream::new(found.reader),
    ));
    let h = res.headers_mut();
    h.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
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
        *res.status_mut() = StatusCode::PARTIAL_CONTENT;
        let content_range = format!(
            "bytes {}-{}/{}",
            found.range.start,
            found.range.end.saturating_sub(1),
            found.total
        );
        res.headers_mut().insert(
            axum::http::header::CONTENT_RANGE,
            HeaderValue::from_str(&content_range).expect("ASCII"),
        );
    }
    Ok(res)
}

pub(crate) async fn verify_object(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    body: Bytes,
) -> LfsResult {
    require_json(&headers)?;
    let req: VerifyObject = serde_json::from_slice(&body)
        .map_err(|e| LfsError::Malformed(format!("invalid request: {e}")))?;
    let (repo, _) = authorize_repo(
        &s,
        &with_bearer(&headers),
        &owner,
        repo_name(&name),
        Some(Permission::Checkin),
    )
    .await?;
    check_oid(&req.oid)?;
    let size = check_size(req.size)?;
    let object = repo
        .service
        .lfs_object(&req.oid)
        .await?
        .ok_or_else(|| LfsError::NotFound(format!("object {} not found", req.oid)))?;
    if object.size != size {
        return Err(LfsError::Malformed(format!(
            "{} has {} bytes, not {size}",
            req.oid, object.size
        )));
    }
    Ok(StatusCode::OK.into_response())
}
