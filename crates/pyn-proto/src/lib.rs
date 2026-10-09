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

/// A live lock the caller holds, with the repository it is in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MyLock {
    pub owner: String,
    pub name: String,
    pub path: String,
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
    /// Runs the server: may approve and disable accounts under `/v1/admin`.
    #[serde(default)]
    pub admin: bool,
}

/// An organization: a namespace that owns repositories.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OrgInfo {
    pub name: String,
    pub created_at: DateTime<Utc>,
    /// The caller's standing in it (`owner` or `member`); absent when they have none.
    pub role: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateOrgRequest {
    /// Shares the namespace with user names: 2 to 39 lowercase letters, digits, `-` or `_`, not reserved.
    pub name: String,
}

/// A person in an organization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OrgMember {
    pub user: String,
    /// `owner` or `member`.
    pub role: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AddOrgMemberRequest {
    /// An existing user account.
    pub user: String,
    /// `owner` or `member`; defaults to `member`.
    #[serde(default)]
    pub role: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SetOrgRoleRequest {
    /// `owner` or `member`.
    pub role: String,
}

/// The caller and what they may do in one repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Me {
    pub user: String,
    pub permissions: Vec<String>,
}

/// Public: anyone, signed in or not, may read the repository. Private: members only, and everyone else gets 404.
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
    /// Locks one user may hold in the repository: the policy file's `meta.max_locks_per_user` if it sets one, else
    /// the repository setting, else the server default.
    pub max_locks_per_user: u32,
    /// The repository setting alone; absent when it follows the server default. Not in force while the policy file sets one.
    pub max_locks_per_user_setting: Option<u32>,
    /// True when `.pyn/pyn.toml` sets the limit; the setting then cannot be changed.
    pub max_locks_set_by_policy: bool,
    pub created_at: DateTime<Utc>,
    /// The caller's role in the repository; absent when they have none.
    pub role: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateRepoRequest {
    /// The namespace to create it in: the caller's own account name, or an organization whose repository-creation
    /// policy lets them. Defaults to the caller.
    pub owner: Option<String>,
    pub name: String,
    /// Defaults to `private`.
    pub visibility: Option<Visibility>,
    /// Defaults to 8.
    pub lease_hours: Option<u32>,
    /// Locks one user may hold; defaults to the server's. A limit in the repository's policy file overrides it.
    pub max_locks_per_user: Option<u32>,
}

/// Fields left out stay as they are.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct UpdateRepoRequest {
    pub name: Option<String>,
    /// Changing it needs the owner with admin rights and is recorded in the audit log.
    pub visibility: Option<Visibility>,
    pub lease_hours: Option<u32>,
    /// A number sets the limit, `null` returns to the server default. Refused while the policy file sets one.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(value_type = Option<u32>)]
    pub max_locks_per_user: Option<Option<u32>>,
}

/// Tells an explicit `null` (`Some(None)`) from an absent field (`None`).
fn present<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Option<u32>>, D::Error> {
    Option::deserialize(d).map(Some)
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
    /// The effective role: the highest of the direct grant, team grants and organization ownership.
    pub role: String,
    /// What decides `role`: `direct`, `team` or `org_owner`.
    pub source: String,
}

/// A team of the owning organization and its role in a repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RepoTeam {
    pub slug: String,
    pub name: String,
    pub description: String,
    pub role: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SetTeamRoleRequest {
    pub role: String,
}

/// A repository where a team holds a role.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TeamRepo {
    /// `owner/name`.
    pub repo: String,
    pub role: String,
}

/// A team of an organization with its members and repository roles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TeamInfo {
    pub slug: String,
    pub name: String,
    pub description: String,
    pub created_at: DateTime<Utc>,
    /// User names, ordered.
    pub members: Vec<String>,
    /// Ordered by repository address.
    pub repos: Vec<TeamRepo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateTeamRequest {
    /// 1 to 39 lowercase letters, digits, `-` or `_`, starting with a letter or digit; unique in the organization.
    pub slug: String,
    /// 1 to 100 characters; defaults to the slug.
    #[serde(default)]
    pub name: Option<String>,
    /// At most 500 characters; defaults to empty.
    #[serde(default)]
    pub description: Option<String>,
}

/// Fields left out stay as they are.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct UpdateTeamRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

/// An organization's repository-creation policy. Owners can always create repositories and are not listed in it.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RepoPolicyInfo {
    /// What members may create when no rule applies: `none` (owners only, the default), `private` or `both`.
    pub member_creation: String,
    /// Ordered by kind, subject, then effect.
    pub rules: Vec<CreationRuleInfo>,
}

/// A subject holds at most one rule per effect. A matching `deny` beats any `allow`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreationRuleInfo {
    /// `allow` or `deny`.
    pub effect: String,
    /// `team`, `user` or `role`.
    pub kind: String,
    /// The team slug, user name, or organization role (`owner` or `member`).
    pub subject: String,
    /// `public`, `private` or `both`.
    pub scope: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SetRepoPolicyRequest {
    /// `none`, `private` or `both`.
    pub member_creation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SetCreationRuleRequest {
    /// `public`, `private` or `both`.
    pub scope: String,
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
    /// With `open` registration: sign-up needs an email address, and the account stays inactive until its
    /// link is followed.
    #[serde(default)]
    pub email_verification: bool,
    /// With `open` registration: an administrator approves each new account after it is verified.
    #[serde(default)]
    pub approval: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RegisterRequest {
    pub username: String,
    pub password: String,
    /// Required when the server is invite only.
    pub invite: Option<String>,
    /// Required when the server is open and verifies email addresses (`RegistrationInfo.email_verification`).
    pub email: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Registered {
    pub user: String,
    /// `active`, or what the account still needs: `pending_verification` (follow the link in the email) or
    /// `pending_approval` (an administrator must approve it).
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct VerifyEmailRequest {
    /// The token from the verification link.
    pub token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ResendVerificationRequest {
    pub email: String,
}

/// An account as an administrator sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AccountInfo {
    pub user: String,
    pub email: Option<String>,
    pub email_verified: bool,
    /// `pending_verification`, `pending_approval`, `active` or `disabled`.
    pub status: String,
    pub disabled_at: Option<DateTime<Utc>>,
    pub disabled_reason: Option<String>,
    pub admin: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct DisableAccountRequest {
    /// Shown to administrators; not to the account holder.
    pub reason: Option<String>,
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
