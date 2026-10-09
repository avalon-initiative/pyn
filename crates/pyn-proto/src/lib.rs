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
    /// Set when this revision restored the content of an earlier one.
    pub restored_from: Option<u64>,
    /// The path's mode when the revision was made; absent for revisions that predate recording it.
    pub mode: Option<Mode>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CheckoutRequest {
    pub path: String,
    /// Revision the caller's copy is at (omit if none); must equal the head.
    pub base_revision: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RestoreRequest {
    pub path: String,
    /// The older revision whose content becomes the new head.
    pub revision: u64,
    /// The current head the caller is looking at; must still be the head.
    pub base_revision: u64,
    /// `<path>@r<base_revision>`, confirming the caller knows which head is being replaced.
    pub confirm: String,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ReleaseRequest {
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CheckinRequest {
    pub path: String,
    /// Hash returned by `PUT /v1/repos/{owner}/{name}/objects`.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum TreeEntryKind {
    File,
    Folder,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum TreeMode {
    Shared,
    Exclusive,
    /// A folder whose files do not all have the same mode.
    Mixed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TreeEntry {
    /// The last path segment.
    pub name: String,
    /// Repo-relative path; pass it as `path` to list a folder.
    pub path: String,
    pub kind: TreeEntryKind,
    pub mode: TreeMode,
    /// The newest revision at or under the entry (`path` on it names the file). Absent for a file with a lock but no revision.
    pub last_change: Option<Revision>,
    /// The live lock on a file; always absent for folders.
    pub lock: Option<Lock>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TreeListing {
    /// The listed folder; empty for the root.
    pub path: String,
    /// Folders first, then files, each by name.
    pub entries: Vec<TreeEntry>,
}

/// A file or lock event from the audit log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ActivityEntry {
    pub id: i64,
    pub at: DateTime<Utc>,
    pub actor: String,
    /// checkout, release, checkin, restore or force_unlock.
    pub action: String,
    pub path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RepoSummary {
    /// Provisional: the single mainline until branches arrive.
    pub default_branch: String,
    /// Provisional: always 1 until branches arrive.
    pub branch_count: u32,
    /// Paths with at least one revision.
    pub files: u64,
    pub exclusive_files: u64,
    pub shared_files: u64,
    /// Time of the newest revision; absent for an empty repository.
    pub updated_at: Option<DateTime<Utc>>,
    /// Live locks with their holders, ordered by path.
    pub locks: Vec<Lock>,
    /// Newest first.
    pub activity: Vec<ActivityEntry>,
}

/// The signed-in account, independent of any repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Account {
    pub user: String,
}

/// The caller and what they may do in one repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Me {
    pub user: String,
    pub permissions: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    Public,
    Private,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RepoInfo {
    pub owner: String,
    pub name: String,
    pub visibility: Visibility,
    /// How long a checkout lasts before it expires unless renewed.
    pub lease_hours: u32,
    pub created_at: DateTime<Utc>,
    /// The caller's role in the repository; absent when they have none.
    pub role: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateRepoRequest {
    /// The namespace to create it in; must be the caller's own account name. Defaults to the caller.
    pub owner: Option<String>,
    pub name: String,
    /// Defaults to `private`.
    pub visibility: Option<Visibility>,
    /// Defaults to 8.
    pub lease_hours: Option<u32>,
}

/// Fields left out stay as they are.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct UpdateRepoRequest {
    pub name: Option<String>,
    pub visibility: Option<Visibility>,
    pub lease_hours: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateTokenRequest {
    pub name: String,
    pub permissions: Vec<String>,
    /// Repositories the token is valid for, as `owner/name`; at least one.
    pub repos: Vec<String>,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TokenInfo {
    pub id: String,
    pub user: String,
    pub name: String,
    pub permissions: Vec<String>,
    /// `owner/name` of each repository the token is limited to; empty means every repository.
    pub repos: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub last_used_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreatedToken {
    /// The token to present as a bearer credential. Shown once and never stored.
    pub token: String,
    pub info: TokenInfo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RoleGrant {
    pub role: String,
    pub permissions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SetRoleRequest {
    pub permissions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Member {
    pub user: String,
    pub role: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SetMemberRequest {
    pub role: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ForceUnlockRequest {
    pub path: String,
    /// Why the lock is being removed; required and recorded in the audit log.
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AuditEntry {
    pub id: i64,
    pub at: DateTime<Utc>,
    pub actor: String,
    pub action: String,
    pub path: Option<String>,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct HistoryPage {
    /// With `path`: all of that path's revisions, oldest first. Without: newest first.
    pub revisions: Vec<Revision>,
    /// Pass as `before` to fetch the next, older page; absent on the last page and with `path`.
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AuditPage {
    /// Newest first.
    pub entries: Vec<AuditEntry>,
    /// Pass as `before` to fetch the next, older page; absent on the last page.
    pub next_before: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RegistrationInfo {
    /// `open`, `invite` or `closed`.
    pub registration: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RegisterRequest {
    pub username: String,
    pub password: String,
    /// Required when the server is invite only.
    pub invite: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Registered {
    pub user: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ChangePasswordRequest {
    /// Required when the account already has a password.
    pub current: Option<String>,
    pub new: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AddUserRequest {
    pub username: String,
    pub password: String,
    pub role: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateInviteRequest {
    pub role: String,
    /// How long the invitation can be used, in hours.
    pub hours: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct InviteInfo {
    pub id: String,
    pub role: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub used_at: Option<DateTime<Utc>>,
    pub used_by: Option<String>,
    pub revoked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreatedInvite {
    /// Give this to the person being invited. Shown once and never stored.
    pub code: String,
    pub info: InviteInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AddKeyRequest {
    /// A name for the key; defaults to the key's own comment.
    pub title: Option<String>,
    /// The public key as an OpenSSH line.
    pub key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SshKeyInfo {
    pub id: String,
    pub user: String,
    pub title: String,
    pub algorithm: String,
    /// `SHA256:...`, as `ssh-keygen -l` prints it.
    pub fingerprint: String,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
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

/// Cookie holding the web session.
pub const SESSION_COOKIE: &str = "pyn_session";

/// Header carrying the CSRF token on state-changing requests made with a session cookie.
pub const CSRF_HEADER: &str = "x-pyn-csrf";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SessionInfo {
    pub user: String,
    /// Send as the `X-Pyn-CSRF` header on every POST, PUT and DELETE.
    pub csrf_token: String,
    pub expires_at: DateTime<Utc>,
}
