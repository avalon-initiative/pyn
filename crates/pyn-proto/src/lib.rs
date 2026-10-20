//! Wire types for the HTTP API, independent of `pyn-core`. The OpenAPI document is generated from them.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

mod events;
pub use events::*;
mod webhooks;
pub use webhooks::*;

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

/// Git's file mode for a file: regular, executable or symbolic link (whose content is the link target).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub enum FileMode {
    #[default]
    #[serde(rename = "100644")]
    Regular,
    #[serde(rename = "100755")]
    Executable,
    #[serde(rename = "120000")]
    Symlink,
}

impl FileMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Regular => "100644",
            Self::Executable => "100755",
            Self::Symlink => "120000",
        }
    }
}

/// A commit's author or committer, apart from the time (the commit's `created_at`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Person {
    pub name: String,
    pub email: String,
    /// Minutes east of UTC.
    pub offset_minutes: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Revision {
    pub id: u64,
    pub path: String,
    /// The content's git blob id (SHA-256 hex of `blob <len>\0` followed by the content).
    pub content: String,
    /// The content string this revision's `hash` covers, when it differs from `content`: revisions that predate blob
    /// ids, whose hash covers the SHA-256 hex of the bare content.
    #[serde(default)]
    pub legacy_content: Option<String>,
    pub author: String,
    /// The structured author the v2 hash covers; absent for a v1 revision, whose hash predates it.
    #[serde(default)]
    pub author_identity: Option<Person>,
    /// The encoding `hash` uses: 1 or 2. Version 2 requires `author_identity`, version 1 forbids it.
    pub hash_version: u32,
    pub message: String,
    pub created_at: DateTime<Utc>,
    /// Set when this revision restored the content of an earlier one.
    pub restored_from: Option<u64>,
    /// The path's mode when the revision was made; absent for revisions that predate recording it.
    pub mode: Option<Mode>,
    /// Shared by the revisions checked in together as one changeset; absent for a single-path checkin.
    pub changeset: Option<String>,
    /// The git file mode of the content.
    pub file_mode: FileMode,
    /// The revision removes the path from its branch; `content` and `file_mode` are those of the version removed.
    /// Its `hash_version` is 3.
    #[serde(default)]
    pub deleted: bool,
    /// The id of the commit this revision belongs to; see `Commit`.
    pub commit: String,
    /// SHA-256 hex over the revision's canonical encoding, by `hash_version` (docs/concepts/integrity.md).
    pub hash: String,
    /// The hash of the path's previous revision by number; absent for the first.
    pub previous: Option<String>,
    /// The revision this one was based on, the committing branch's revision of the path; absent when the branch had
    /// none. Not covered by `hash`; `pyn verify` checks that it names an older revision of the path.
    #[serde(default)]
    pub base: Option<u64>,
    /// Only in the response to checking in or restoring `.pyn/pyn.toml` on the default branch: the diverged shared
    /// versions on other branches that the change stranded. Empty, and omitted, everywhere else.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stranded: Vec<StrandedVersion>,
}

/// A shared version a branch holds of a path that became exclusive on the default branch (decision #38). It stays in
/// history but can no longer merge into the path's single line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct StrandedVersion {
    pub path: String,
    /// The branch that holds the version.
    pub branch: String,
    /// Who created the branch; the audit log tells them (`version_stranded`).
    pub branch_owner: String,
    /// The stranded revision of the path.
    pub revision: u64,
    /// The commit the revision belongs to.
    pub commit: String,
    /// Who made the stranded revision.
    pub author: String,
    pub created_at: DateTime<Utc>,
    /// The conversion revision the default branch made when the path became exclusive; absent in a preview.
    pub conversion: Option<u64>,
}

/// One atomic write: a single checkin or a whole changeset, recorded as a git commit on a branch. `id` is the git
/// commit id, so a commit can be fetched with a unique abbreviation of at least 4 digits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Commit {
    pub id: String,
    /// Empty for the first commit of the repository, otherwise the commit it was made on top of.
    pub parents: Vec<String>,
    /// The id of the root git tree of the repository as of this commit.
    pub tree: String,
    pub author: Person,
    pub committer: Person,
    /// The committer time, in whole seconds.
    pub created_at: DateTime<Utc>,
    /// The author time, in whole seconds; its timezone is `author.offset_minutes`. Absent from servers that predate it.
    #[serde(default)]
    pub authored_at: Option<DateTime<Utc>>,
    /// The message decoded lossily; a commit's exact bytes are never altered.
    pub message: String,
    /// The commit's `gpgsig` header, when it is signed.
    pub signature: Option<String>,
    /// Set when the commit was made as a changeset.
    pub changeset: Option<String>,
    /// The revisions the commit made, ordered by path.
    pub revisions: Vec<Revision>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CommitPage {
    /// Newest first.
    pub commits: Vec<Commit>,
    /// Pass as `before` to fetch the next, older page; null on the last page.
    #[schema(required = true)]
    pub next_before: Option<String>,
}

/// A path as of a commit: the revision the commit's ancestry gives it. `mode` is what `.pyn/pyn.toml` says at that commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SnapshotEntry {
    pub path: String,
    pub mode: Mode,
    pub revision: u64,
    /// The content's git blob id at that revision.
    pub content: String,
    pub file_mode: FileMode,
}

/// The whole repository as of one commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SnapshotPage {
    /// The id of the commit the entries are as of; null for a repository with no commits.
    #[schema(required = true)]
    pub commit: Option<String>,
    /// Ordered by path.
    pub entries: Vec<SnapshotEntry>,
    /// Pass as `after` to fetch the next page; null on the last page.
    #[schema(required = true)]
    pub next_after: Option<String>,
}

/// A branch: a name pointing at a commit. The default branch's `.pyn/pyn.toml` is the repository's policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Branch {
    pub name: String,
    /// The commit the branch points at; null only for the default branch of a repository with no commits.
    #[schema(required = true)]
    pub commit: Option<String>,
    pub default: bool,
    pub created_at: DateTime<Utc>,
    pub created_by: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateBranchRequest {
    /// Git ref rules apply: no spaces, none of `~ ^ : ? * [ \`, no `..`.
    pub name: String,
    /// A branch name, or a commit id or abbreviation of at least 4 digits; the default branch's head if omitted.
    pub start: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RenameBranchRequest {
    pub name: String,
}

/// Whether a merge may or must fast-forward.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MergeFastForward {
    /// Fast-forward when the target is behind the source, otherwise make a merge commit.
    #[default]
    Allow,
    /// Always make a merge commit when the histories differ.
    Never,
    /// Refuse (`not_fast_forward`) unless the target can fast-forward.
    Only,
}

/// A source (a branch, or a commit) merged into a target branch.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MergeRequest {
    /// A branch name, or a commit id or abbreviation of at least 4 digits.
    pub source: String,
    /// The branch that moves; the default branch if omitted.
    pub target: Option<String>,
    pub fast_forward: Option<MergeFastForward>,
    /// The merge commit's message; `Merge branch '<source>' into <target>` if omitted.
    pub message: Option<String>,
    /// The commit the caller saw as the target's head; if the branch has moved the answer is `merge_stale`.
    pub expect_target: Option<String>,
    /// The content chosen for each conflicted path (and optionally for a path that merged cleanly).
    #[serde(default)]
    pub resolutions: Vec<MergeResolution>,
}

/// The content chosen for one path of a merge: an uploaded object, by its blob id.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MergeResolution {
    pub path: String,
    pub content: String,
    /// Git's file mode of the content; regular if omitted.
    pub file_mode: Option<FileMode>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MergeOutcome {
    /// The source is already in the target.
    UpToDate,
    /// The target moves to the source's commit; no commit is made.
    FastForward,
    /// A merge commit with two parents.
    Merge,
    /// A preview only: paths conflict and need a resolution each.
    Conflicted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MergeConflictKind {
    /// Both sides changed the same lines of a text file.
    Content,
    /// Both sides added the path with different content.
    BothAdded,
    /// Both sides changed a file that cannot be merged line by line; the whole file is the unit of conflict.
    Binary,
    /// One side removed the path and the other changed it.
    DeleteModify,
    /// Both sides changed the git file mode differently.
    FileMode,
    /// A shared path changed on a branch that meets the path's conversion to exclusive (decision #195 item 4).
    ExclusiveConversion,
}

/// One version of a path in a merge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MergeSide {
    /// The revision that holds this version; null for the virtual base of a criss-cross merge.
    #[schema(required = true)]
    pub revision: Option<u64>,
    /// The git blob id of the content.
    pub content: String,
    pub file_mode: FileMode,
}

/// The versions a path's merge is made from; a side is null where the path does not exist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MergeInputs {
    #[schema(required = true)]
    pub base: Option<MergeSide>,
    #[schema(required = true)]
    pub ours: Option<MergeSide>,
    #[schema(required = true)]
    pub theirs: Option<MergeSide>,
    /// The base's bytes, base64, when no revision holds it (a virtual base), so a client can still merge against it.
    #[schema(required = true)]
    pub base_inline: Option<String>,
}

/// A path the merge changes in the target, with the content it ends up with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MergeChange {
    pub path: String,
    pub mode: Mode,
    /// The git blob id of the merged content.
    pub content: String,
    pub file_mode: FileMode,
    /// A revision of the path that holds exactly this content and file mode, when one does.
    #[schema(required = true)]
    pub revision: Option<u64>,
    /// The versions to merge line by line, present only when no revision holds the result.
    #[schema(required = true)]
    pub inputs: Option<MergeInputs>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MergeConflict {
    pub path: String,
    pub mode: Mode,
    pub kind: MergeConflictKind,
    pub inputs: MergeInputs,
    /// For `exclusive_conversion`: what happened to the path and when, when the history shows it.
    #[serde(default)]
    pub conversion: Option<MergeConversion>,
}

/// The conversion of a path to exclusive that a merge conflict met, and the shared version it stranded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MergeConversion {
    /// The conversion revision, the first of the path's exclusive line.
    pub revision: u64,
    /// The policy revision in force at the conversion.
    pub policy_revision: Option<u64>,
    /// Who made the policy change.
    pub by: String,
    pub at: DateTime<Utc>,
    /// The diverged shared revision the conversion stranded, with its author and time.
    pub diverged_revision: u64,
    pub diverged_by: String,
    pub diverged_at: DateTime<Utc>,
}

/// What a merge does, or did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MergeReport {
    pub target: String,
    /// The target's head the merge was computed on; null for a branch with no commits.
    #[schema(required = true)]
    pub target_commit: Option<String>,
    pub source: String,
    /// The merge base commits; more than one means a criss-cross history.
    pub bases: Vec<String>,
    /// True when the bases were merged into a virtual one.
    pub virtual_base: bool,
    pub outcome: MergeOutcome,
    /// Paths whose content changes in the target, ordered by path.
    pub changes: Vec<MergeChange>,
    pub conflicts: Vec<MergeConflict>,
    /// The commit the merge wrote; null for a preview and for an up-to-date merge.
    #[schema(required = true)]
    pub commit: Option<String>,
    /// Shared versions on other branches that the policy file a merge into the default branch brought stranded;
    /// empty for a preview and for every other merge.
    #[serde(default)]
    pub stranded: Vec<StrandedVersion>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CheckoutRequest {
    pub path: String,
    /// Revision the caller's copy is at (omit if none); must equal the head.
    pub base_revision: Option<u64>,
    /// The branch the lock is for: the path must be exclusive under that branch's effective policy. The default
    /// branch if omitted.
    #[serde(default)]
    pub branch: Option<String>,
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
    /// The branch the restore is committed on; the default branch if omitted.
    pub branch: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ReleaseRequest {
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CheckinRequest {
    pub path: String,
    /// Blob id returned by `PUT /v1/repos/{owner}/{name}/objects`.
    pub content: String,
    /// Defaults to `100644`.
    #[serde(default)]
    pub file_mode: Option<FileMode>,
    /// A shared path's revision on the branch, or an exclusive path's newest revision on any branch.
    pub base_revision: Option<u64>,
    pub message: String,
    /// The branch the commit is made on; the default branch if omitted.
    pub branch: Option<String>,
}

/// One path of a changeset.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ChangesetEntry {
    pub path: String,
    /// Blob id returned by `PUT /v1/repos/{owner}/{name}/objects`.
    pub content: String,
    /// Defaults to `100644`.
    #[serde(default)]
    pub file_mode: Option<FileMode>,
    pub base_revision: Option<u64>,
}

/// One path a changeset removes. The path stays in history as a deletion revision (`Revision.deleted`).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ChangesetDeletion {
    pub path: String,
    /// The path's revision on the branch: the one being removed.
    pub base_revision: Option<u64>,
}

/// Paths checked in and removed together: every base and lock check passes or nothing is written. An exclusive path,
/// written or removed, needs the caller's live lock, which the commit consumes.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ChangesetRequest {
    pub entries: Vec<ChangesetEntry>,
    /// Paths to remove; a changeset needs at least one entry or deletion.
    #[serde(default)]
    pub deletions: Vec<ChangesetDeletion>,
    pub message: String,
    /// The branch the commit is made on; the default branch if omitted.
    pub branch: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ChangesetResponse {
    pub changeset: String,
    /// One revision per requested path: the entries in request order, then the deletions.
    pub revisions: Vec<Revision>,
}

/// One git object of a push: its id and its body (the bytes after git's `<kind> <length>` header), base64.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PushObject {
    /// The object id the body hashes to under the repository's algorithm (SHA-256).
    pub id: String,
    pub data: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PushRequest {
    /// The branch to append to; the default branch if omitted.
    pub branch: Option<String>,
    /// The commits, oldest first; the first is a child of the branch's head, each next one of the one before.
    pub commits: Vec<PushObject>,
    /// The trees the commits need that the server may not have. Blobs are uploaded beforehand through the object routes.
    pub trees: Vec<PushObject>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PushResponse {
    pub branch: String,
    /// The branch's head commit after the push.
    pub head: String,
    /// The revisions the commits made, in commit order, each commit's by path.
    pub revisions: Vec<Revision>,
    /// Shared versions on other branches that a policy file in the push stranded; omitted when there are none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stranded: Vec<StrandedVersion>,
    /// Live locks that a policy file in the push released because their paths became shared; omitted when there are none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub released_locks: Vec<Lock>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct FetchRequest {
    /// The branches to fetch; every branch if empty. Ignored when `cursor` is given.
    #[serde(default)]
    pub branches: Vec<String>,
    /// Commits the client already has, normally its branch heads; ids the repository does not know are ignored.
    #[serde(default)]
    pub haves: Vec<String>,
    /// The `next` of the previous page.
    #[serde(default)]
    pub cursor: Option<String>,
    /// Commits per page; the server's default and cap apply when omitted or larger.
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FetchRef {
    pub branch: String,
    /// The branch's head when the fetch started; null for a branch with no commit.
    pub head: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FetchCommit {
    pub id: String,
    /// Base64 of the commit's body, as in a push.
    pub data: String,
    /// The revisions the commit's pointer cache records: what it wrote and what a merge adopted, ordered by path.
    pub pointers: Vec<Revision>,
    /// The trees the commit adds against its first parent, as in a push.
    pub trees: Vec<PushObject>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FetchResponse {
    pub refs: Vec<FetchRef>,
    /// Parents before children.
    pub commits: Vec<FetchCommit>,
    /// Opaque; send it as `cursor` for the next page. Absent on the last page.
    pub next: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PutObjectResponse {
    /// The object's git blob id; for a chunk upload, the SHA-256 hex of its bytes.
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
    /// The head revision's git file mode; `100644` when there is no revision.
    pub file_mode: FileMode,
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
    pub default_branch: String,
    pub branch_count: u32,
    /// Paths with at least one revision on the default branch.
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
    /// At least one of `manage_accounts`, `manage_organizations`, `manage_limits`, `manage_webhooks`.
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
    /// How long a lock lasts before it expires unless renewed.
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

/// Chunk digests to check against the server's object store.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct MissingObjectsRequest {
    /// SHA-256 hex of each chunk's bytes; at most 10000.
    pub hashes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MissingObjectsResponse {
    /// The digests from the request that the server does not store.
    pub missing: Vec<String>,
}

/// Chunks already uploaded as objects, to be joined into one object.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AssembleObjectRequest {
    /// SHA-256 hex digests of stored chunks, in order; each at most 4 MiB.
    pub chunks: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PolicyPreviewRequest {
    /// The proposed contents of `.pyn/pyn.toml`.
    pub content: String,
}

/// What a proposed policy would change in the repository, without applying it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PolicyPreview {
    /// Paths that are exclusive now and would become shared.
    pub to_shared: Vec<String>,
    /// Paths that are shared now and would become exclusive.
    pub to_exclusive: Vec<String>,
    /// Live locks that would be released because their path becomes shared.
    pub released_locks: Vec<Lock>,
    /// Diverged shared versions on other branches of the paths that would become exclusive; each would be stranded.
    #[serde(default)]
    pub stranded: Vec<StrandedVersion>,
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
    /// SHA-256 hex over the event's canonical encoding, which includes `previous`.
    pub hash: String,
    /// The hash of the preceding event in the same log; absent for the first.
    pub previous: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct HistoryPage {
    /// With `path`: that path's revisions, oldest first. Without: newest first.
    pub revisions: Vec<Revision>,
    /// Pass as `before` to fetch the next page (the next newer one with `path`, the next older one without); null on the last page.
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

/// What this server supports beyond the base API; a client checks it before it relies on a newer route.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ServerCapabilities {
    /// The server accepts pushed commits and serves fetches (`/push`, `/fetch`), so a client may keep commits locally.
    pub local_commits: bool,
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
    /// The one-time setup token printed by the server on first run (stderr) or from its `PYN_SETUP_TOKEN` setting.
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
/// Carries the content's git blob id on `GET .../content`, so a client can verify what it downloaded.
pub const CONTENT_HASH_HEADER: &str = "x-pyn-content";

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

/// Cookie that ties an external sign-in to the browser that started it; only sent to `/v1/oidc`.
pub const OIDC_BINDING_COOKIE: &str = "pyn_oidc";

/// How people can sign in to this server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SignInOptions {
    /// Whether anyone may sign in with a password. When false, server administrators still can.
    pub password: bool,
    /// The configured external provider; null when the server has none.
    pub external: Option<ExternalProviderInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ExternalProviderInfo {
    /// What to call it on the sign-in button.
    pub name: String,
    /// Send the browser here (a full page navigation, with an optional `return_to` path) to start signing in.
    pub sign_in_url: String,
}

/// A provider identity linked to an account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ExternalIdentityInfo {
    pub id: String,
    pub issuer: String,
    pub subject: String,
    /// The address the provider last gave, if it vouched for one.
    pub email: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_sign_in_at: Option<DateTime<Utc>>,
}

/// Where to send the browser to link the provider to the signed-in account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OidcLink {
    pub url: String,
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

/// One break found by verification: `hash_mismatch` (a record's fields do not give its hash), `version_mismatch` (a revision's `hash_version` disagrees with its stored fields), `broken_link` (it does
/// not follow the record before it) or `base_mismatch` (a revision's base is not an older revision of its path). Revision breaks carry `path` and `revision`; audit breaks carry `event`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct VerifyProblem {
    pub kind: String,
    pub path: Option<String>,
    pub revision: Option<u64>,
    pub event: Option<i64>,
    /// The commit id, for a problem in a commit record.
    #[serde(default)]
    pub commit: Option<String>,
}

/// What one `verify` request checked. With `paged=true` a request does a bounded share of the work: add the counts and
/// problems of the steps together, and send `next_cursor` back as `cursor` until it is null.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct VerifyReport {
    pub paths: u64,
    pub revisions: u64,
    pub commits: u64,
    /// Events checked; null when the audit log was not checked (a single path, or no `view_audit`).
    #[schema(required = true)]
    pub audit_events: Option<u64>,
    pub problems: Vec<VerifyProblem>,
    /// Opaque; send it as `cursor` for the next step. Null when everything has been checked. Absent from servers that
    /// check in one request.
    #[serde(default)]
    pub next_cursor: Option<String>,
}
