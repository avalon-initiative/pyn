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
    /// Pass as `after` to fetch the next page; null on the last page.
    #[schema(required = true)]
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
    #[schema(value_type = Option<OrgRole>)]
    pub role: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateOrgRequest {
    /// Shares the namespace with user names: 2 to 39 lowercase letters, digits, `-` or `_`, not reserved.
    pub name: String,
}

/// A server administrator or service credential creating an organization for an existing user.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AdminCreateOrgRequest {
    /// Same rules as `CreateOrgRequest.name`.
    pub name: String,
    /// An existing active user, who becomes the organization's first owner.
    pub owner: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateServiceCredentialRequest {
    /// 1 to 64 lowercase letters, digits, `-` or `_`; unique, and never reused after revocation.
    pub name: String,
    /// At least one of `manage_accounts`, `manage_organizations`, `manage_limits`.
    pub scopes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ServiceCredentialInfo {
    pub name: String,
    pub scopes: Vec<String>,
    /// The server administrator who created it.
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub last_used_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreatedServiceCredential {
    /// Presented as a bearer credential. Shown once and never stored.
    pub secret: String,
    pub info: ServiceCredentialInfo,
}

// Fields typed by these are `String` in Rust and enums in the schema (`value_type`), so an unknown value in a
// request still reaches the handler and gets `invalid_request`.
/// An account's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AccountStatus {
    PendingVerification,
    PendingApproval,
    Active,
    Disabled,
}

/// Who may create an account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum RegistrationMode {
    Open,
    Invite,
    Closed,
}

/// A person's standing in an organization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum OrgRole {
    Owner,
    Member,
}

/// Whether a creation rule permits or forbids.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum CreationEffect {
    Allow,
    Deny,
}

/// What a creation rule's subject names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum CreationSubjectKind {
    Team,
    User,
    Role,
}

/// Which repository visibilities a rule or policy covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum CreationScope {
    Public,
    Private,
    Both,
}

/// What members may create when no rule applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum MemberCreation {
    None,
    Private,
    Both,
}

/// What decides a member's effective role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MemberSource {
    Direct,
    Team,
    OrgOwner,
}

/// A person in an organization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OrgMember {
    pub user: String,
    #[schema(value_type = OrgRole)]
    pub role: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AddOrgMemberRequest {
    /// An existing user account.
    pub user: String,
    /// Defaults to `member`.
    #[serde(default)]
    #[schema(value_type = Option<OrgRole>)]
    pub role: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SetOrgRoleRequest {
    #[schema(value_type = OrgRole)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RepoPage {
    /// Ordered by owner, then name.
    pub repos: Vec<RepoInfo>,
    /// Pass as `after` to fetch the next page; null on the last page.
    #[schema(required = true)]
    pub next_after: Option<String>,
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
fn present<'de, T: Deserialize<'de>, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<Option<T>>, D::Error> {
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
    #[schema(value_type = MemberSource)]
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
    /// What members may create when no rule applies; `none` (owners only) is the default.
    #[schema(value_type = MemberCreation)]
    pub member_creation: String,
    /// Ordered by kind, subject, then effect.
    pub rules: Vec<CreationRuleInfo>,
}

/// A subject holds at most one rule per effect. A matching `deny` beats any `allow`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreationRuleInfo {
    #[schema(value_type = CreationEffect)]
    pub effect: String,
    #[schema(value_type = CreationSubjectKind)]
    pub kind: String,
    /// The team slug, user name, or organization role (`owner` or `member`).
    pub subject: String,
    #[schema(value_type = CreationScope)]
    pub scope: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SetRepoPolicyRequest {
    #[schema(value_type = MemberCreation)]
    pub member_creation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SetCreationRuleRequest {
    #[schema(value_type = CreationScope)]
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
    /// Pass as `before` to fetch the next, older page; null on the last page and with `path`.
    #[schema(required = true)]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AuditPage {
    /// Newest first.
    pub entries: Vec<AuditEntry>,
    /// Pass as `before` to fetch the next, older page; null on the last page.
    #[schema(required = true)]
    pub next_before: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RegistrationInfo {
    #[schema(value_type = RegistrationMode)]
    pub registration: String,
    /// With `open` registration: sign-up needs an email address, and the account stays inactive until its
    /// link is followed.
    #[serde(default)]
    pub email_verification: bool,
    /// With `open` registration: an administrator approves each new account after it is verified.
    #[serde(default)]
    pub approval: bool,
}

/// Whether first-run setup has happened; carries no secrets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SetupStatus {
    pub initialised: bool,
    /// Set once setup recorded a name.
    pub server_name: Option<String>,
    /// What setup recorded; before setup, the address the server is configured with.
    pub public_url: String,
    /// What setup recorded, or the configured default before setup.
    #[schema(value_type = RegistrationMode)]
    pub registration: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SetupRequest {
    /// The one-time setup token from the server's log or its `PYN_SETUP_TOKEN` setting.
    pub token: String,
    /// The first administrator's user name.
    pub username: String,
    pub password: String,
    pub email: Option<String>,
    pub server_name: Option<String>,
    /// The address people reach the web app at; verification links use it.
    pub public_url: Option<String>,
    #[schema(value_type = RegistrationMode)]
    pub registration: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SetupCompleted {
    /// The first administrator, who can now sign in with the password.
    pub user: String,
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
    #[schema(value_type = AccountStatus)]
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
    #[schema(value_type = AccountStatus)]
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

/// What an owner (a user or an organization) is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum OwnerKind {
    User,
    Org,
}

/// Limits on one owner. Every limit is opt-in: a null field is unlimited.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Limits {
    /// Repositories the owner may have.
    #[schema(required = true)]
    pub repositories: Option<u64>,
    /// Members an organization may have; always null for a user.
    #[schema(required = true)]
    pub members: Option<u64>,
    /// Bytes of distinct content across the owner's repositories.
    #[schema(required = true)]
    pub storage_bytes: Option<u64>,
}

/// An owner's limits. Nothing is limited unless an operator sets a limit or a server default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OwnerLimits {
    pub kind: OwnerKind,
    /// The limits in force: the owner's own where set, else the server default, else null (unlimited).
    pub effective: Limits,
    /// Limits set for this owner alone; a null field follows the server default.
    pub own: Limits,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OwnerLimitsEntry {
    pub owner: String,
    pub limits: OwnerLimits,
}

/// The server default and every owner that has limits of its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LimitsListing {
    /// Applies to owners with nothing of their own; all null unless the operator configures it.
    pub defaults: Limits,
    /// Ordered by name.
    pub owners: Vec<OwnerLimitsEntry>,
}

/// Fields left out stay as they are; a number sets the limit and `null` returns the field to the server default.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct SetLimitsRequest {
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(value_type = Option<u64>)]
    pub repositories: Option<Option<u64>>,
    /// Organizations only; a user has no member limit.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(value_type = Option<u64>)]
    pub members: Option<Option<u64>>,
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(value_type = Option<u64>)]
    pub storage_bytes: Option<Option<u64>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Usage {
    pub repositories: u64,
    /// Organization members; null for a user.
    #[schema(required = true)]
    pub members: Option<u64>,
    /// Bytes of distinct content, counted once per repository.
    pub stored_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RepoUsage {
    /// `owner/name`.
    pub repository: String,
    pub stored_bytes: u64,
    /// Paths with a head revision.
    pub files: u64,
    pub revisions: u64,
}

/// What an owner uses against its limits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OwnerUsage {
    pub owner: String,
    pub limits: OwnerLimits,
    pub usage: Usage,
    /// Ordered by name.
    pub repositories: Vec<RepoUsage>,
}
