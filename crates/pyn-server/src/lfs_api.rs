//! The Git LFS Locks API over pyn locks, mounted under `/v1/repos/{owner}/{name}/info/lfs`. See docs/concepts/lfs.md.

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE, WWW_AUTHENTICATE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use pyn_core::{Lock, Permission, PynError, RepoPath};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};

use crate::{ApiError, AppState, authorize_repo, is_internal, public_message, status_for};

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
            Self::Pyn(e) => (status_for(e), public_message(e), is_internal(e)),
            Self::Malformed(m) => (StatusCode::UNPROCESSABLE_ENTITY, m.clone(), false),
            Self::NotFound(m) => (StatusCode::NOT_FOUND, m.clone(), false),
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
