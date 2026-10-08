//! Wire types for the HTTP API, independent of `pyn-core`. The OpenAPI document is generated from them.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Lock {
    pub path: String,
    pub owner: String,
    pub acquired_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Revision {
    pub id: u64,
    pub path: String,
    /// SHA-256 hex of the content.
    pub content: String,
    pub author: String,
    pub message: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CheckoutRequest {
    pub path: String,
    /// Revision the caller's copy is at (omit if none); must equal the head.
    pub base_revision: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ReleaseRequest {
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CheckinRequest {
    pub path: String,
    /// Hash returned by `PUT /v1/objects`.
    pub content: String,
    pub base_revision: Option<u64>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PutObjectResponse {
    pub content: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Shared,
    Exclusive,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct FileEntry {
    pub path: String,
    pub mode: Mode,
    /// Head revision; absent if the path is locked but has no revision yet.
    pub revision: Option<u64>,
    pub lock: Option<Lock>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct FilePage {
    pub entries: Vec<FileEntry>,
    /// Pass as `after` to fetch the next page; absent on the last page.
    pub next_after: Option<String>,
}

/// Body of every non-2xx response; `code` is stable and machine-readable.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
}

/// Response header carrying the revision number of returned content.
pub const REVISION_HEADER: &str = "x-pyn-revision";

/// Dev-only identity header.
pub const DEV_USER_HEADER: &str = "x-pyn-user";
