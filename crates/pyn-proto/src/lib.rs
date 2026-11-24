//! Wire types for the HTTP API, independent of `pyn-core`. The OpenAPI document is generated from them.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

mod access_session;
pub use access_session::*;
mod account_deletion;
pub use account_deletion::*;
mod diff;
pub use diff::*;
mod events;
pub use events::*;
mod pulls;
pub use pulls::*;
mod readiness;
pub use readiness::*;
mod update;
pub use update::*;
mod server_invitations;
pub use server_invitations::*;
mod webhooks;
pub use webhooks::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Lock {
    pub path: String,
    pub owner: String,
    /// The owner's public identity; absent when the account is gone or not active.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<PublicIdentity>,
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
    /// The author's name now, when the account was renamed after writing this revision (decision #986). `author`
    /// stays as written because the hash covers it; show this one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_current: Option<String>,
    /// The author's public identity (the current name's account when renamed); absent when no active account holds it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_public: Option<PublicIdentity>,
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
    /// The author's name now, when it differs from `author` (decision #986).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_current: Option<String>,
    /// The author's public identity (the current name's account when renamed); absent when no active account holds it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_public: Option<PublicIdentity>,
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
    /// The author's account name now, when it was renamed after the commit (decision #986); `author.name` stays as
    /// written because the commit id covers it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_current: Option<String>,
    /// The author's public identity (the current name's account when renamed); absent when no active account holds it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_public: Option<PublicIdentity>,
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
    /// The committed time of the head commit; absent for a branch with no commits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<DateTime<Utc>>,
    /// The author of the head commit; absent for a branch with no commits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_by: Option<String>,
    /// Commits this branch has and the default branch lacks, at most 1000; 0 for the default branch.
    #[serde(default)]
    pub ahead: u32,
    /// Commits the default branch has and this branch lacks, at most 1000; 0 for the default branch.
    #[serde(default)]
    pub behind: u32,
    /// `ahead` or `behind` reached 1000, so the real number is larger.
    #[serde(default)]
    pub counts_capped: bool,
    /// The caller created the branch or wrote its head commit; always false without a sign-in.
    #[serde(default)]
    pub yours: bool,
    /// The head commit is older than the server's stale period (`PYN_BRANCH_STALE_DAYS`, 90 days by default). Never the default branch.
    #[serde(default)]
    pub stale: bool,
    /// Not stale: the default branch and every branch committed to within the stale period.
    #[serde(default)]
    pub active: bool,
}

/// A tag: an immutable name pointing at a commit. It never moves; deleting it frees the name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Tag {
    pub name: String,
    /// The commit the tag points at.
    pub commit: String,
    /// The note given when the tag was made; null when there was none.
    #[schema(required = true)]
    pub message: Option<String>,
    pub created_at: DateTime<Utc>,
    pub created_by: String,
    /// The committed time of the tagged commit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub committed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CompareStatus {
    /// Neither side has a commit the other lacks.
    Identical,
    /// The head has commits the base lacks and no more.
    Ahead,
    /// The base has commits the head lacks and no more.
    Behind,
    /// Each side has commits the other lacks.
    Diverged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CompareChangeKind {
    Added,
    Modified,
    Deleted,
}

/// A path the head changes relative to the merge base.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CompareChange {
    pub path: String,
    pub kind: CompareChangeKind,
}

/// How far a head is from a base. Commits are counted as git does: the head's ahead are those reachable from it and
/// not from the base, the behind the reverse.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct BranchComparison {
    /// The base as named in the request (the default branch when none was given).
    pub base: String,
    pub base_commit: String,
    pub head: String,
    pub head_commit: String,
    /// The first (by id) of the lowest common ancestors; `changes` are measured from it. Absent for unrelated histories.
    #[schema(required = true)]
    pub merge_base: Option<String>,
    pub status: CompareStatus,
    /// Commits in the head and not in the base, at most `max_commits`.
    pub ahead: u32,
    /// Commits in the base and not in the head, at most `max_commits`.
    pub behind: u32,
    /// The limit on `ahead`, `behind` and the commits whose paths are examined.
    pub max_commits: u32,
    /// Either count reached `max_commits`: the real number is larger and `changes` covers only the oldest commits.
    pub counts_capped: bool,
    /// Paths the head adds, modifies or deletes relative to the merge base, by path, at most `max_changes`.
    pub changes: Vec<CompareChange>,
    pub max_changes: u32,
    /// `changes` is incomplete: more paths changed than listed, or more commits than `max_commits`.
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateBranchRequest {
    /// Git ref rules apply: no spaces, none of `~ ^ : ? * [ \`, no `..`.
    pub name: String,
    /// A branch name, or a commit id or abbreviation of at least 4 digits; the default branch's head if omitted.
    pub start: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateTagRequest {
    /// Git ref rules apply: no spaces, none of `~ ^ : ? * [ \`, no `..`.
    pub name: String,
    /// A branch or tag name, or a commit id or abbreviation of at least 4 digits; the default branch's head if omitted.
    pub target: Option<String>,
    /// An optional note kept with the tag.
    pub message: Option<String>,
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
    /// The branch to append to; the default branch if omitted. A name the server does not have is created by the push,
    /// from the first commit's parent (any commit the server has, or none on an empty repository).
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
    /// The push created the branch; omitted when it already existed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub created: bool,
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
    /// The actor's name now, when it differs from `actor`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_current: Option<String>,
    /// The actor's public identity (the current name's account when renamed); absent when no active account holds it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_public: Option<PublicIdentity>,
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
    /// Stable, never reused: the name can be recycled after a delete, the id cannot.
    pub id: String,
    pub user: String,
    /// Runs the server: may approve and disable accounts under `/v1/admin`.
    #[serde(default)]
    pub admin: bool,
    /// Whether the account has a password. False for an account created through external sign-in until it sets
    /// one, in which case `PUT /v1/me/password` takes no current password. Absent for a credential that is not
    /// full-scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_password: Option<bool>,
    /// The account's profile; the email address is never part of it.
    #[serde(default)]
    pub profile: Profile,
    /// A second factor the account must still set up, with its deadline: the client's cue to nag. Absent when none is
    /// required or the account has one, and for a credential that is not full-scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub two_factor_required: Option<TwoFactorRequired>,
}

/// A requirement that the account hold a second factor, and what happens at the deadline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TwoFactorRequired {
    /// `server` (an administrator requires it of everyone) or `organization`.
    pub by: String,
    /// The organization that requires it, when `by` is `organization`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization: Option<String>,
    /// From this moment the account can only set up a second factor (`403 two_factor_enrolment_required`).
    pub deadline: DateTime<Utc>,
    /// True once the deadline has passed.
    pub enforced: bool,
}

/// Whether second factors are required, and the grace period.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TwoFactorSetting {
    pub required: bool,
    /// When the requirement was set; absent when none is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<DateTime<Utc>>,
    /// Days an account without a factor keeps full access after `since` (or after it was created).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grace_days: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SetTwoFactorRequest {
    pub required: bool,
    /// 0 to 90 days, default 7. Setting the same requirement again changes nothing; a different grace period starts
    /// the clock again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grace_days: Option<u32>,
}

/// A message for the account, shown by the web app until dismissed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Notice {
    pub id: String,
    /// `two_factor_reset`: an administrator reset the account's second factors.
    pub kind: String,
    pub detail: String,
    pub created_at: DateTime<Utc>,
}

/// The optional, user-edited part of an account: what others see on its profile page.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Profile {
    /// At most 64 characters, trimmed, no control characters.
    pub display_name: Option<String>,
    /// At most 160 characters; may span lines.
    pub bio: Option<String>,
    /// At most 64 characters.
    pub location: Option<String>,
    /// At most 64 characters.
    pub company: Option<String>,
    /// Up to 5 `http` or `https` URLs.
    #[serde(default)]
    pub links: Vec<String>,
    /// Where the avatar image is served, with a `v` parameter that changes when the image does; absent when the
    /// account has none.
    pub avatar_url: Option<String>,
    /// Which fields others may see. Present only for the account's own view; the other fields are already filtered
    /// by it, so a hidden one is absent (null, empty or no `avatar_url`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility: Option<ProfileVisibility>,
}

fn visible() -> bool {
    true
}

/// Per-field visibility of a profile. A missing field is visible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ProfileVisibility {
    #[serde(default = "visible")]
    pub display_name: bool,
    #[serde(default = "visible")]
    pub bio: bool,
    #[serde(default = "visible")]
    pub location: bool,
    #[serde(default = "visible")]
    pub company: bool,
    #[serde(default = "visible")]
    pub links: bool,
    #[serde(default = "visible")]
    pub avatar: bool,
    /// The list of repositories the account starred.
    #[serde(default = "visible")]
    pub starred: bool,
}

impl Default for ProfileVisibility {
    fn default() -> Self {
        Self {
            display_name: true,
            bio: true,
            location: true,
            company: true,
            links: true,
            avatar: true,
            starred: true,
        }
    }
}

/// The new profile text: every field is replaced, and one left out or blank is cleared.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UpdateProfileRequest {
    pub display_name: Option<String>,
    pub bio: Option<String>,
    pub location: Option<String>,
    pub company: Option<String>,
    #[serde(default)]
    pub links: Vec<String>,
    /// Left out, the stored visibility is kept; given, it replaces it (a field missing from it is visible).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility: Option<ProfileVisibility>,
}

/// An account's public profile as the caller may see it: no email address, and only the repositories and
/// organizations the caller may know about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PublicProfile {
    /// Stable, never reused.
    pub id: String,
    pub user: String,
    pub created_at: DateTime<Utc>,
    pub profile: Profile,
    /// The repositories the user owns that the caller may see: public ones, and private ones where the caller has
    /// a role. `role` is the caller's.
    pub repositories: Vec<RepoInfo>,
    /// How many repositories are listed.
    pub repository_count: u64,
    /// The organizations the user belongs to that the caller belongs to as well, with the caller's role; always
    /// empty when signed out.
    pub organizations: Vec<OrgInfo>,
}

/// An organization: a namespace that owns repositories.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OrgInfo {
    /// Stable and never reused: an organization recreated under a deleted name gets a new id.
    pub id: String,
    pub name: String,
    pub created_at: DateTime<Utc>,
    /// The caller's standing in it (`owner` or `member`); absent when they have none.
    #[schema(value_type = Option<OrgRole>)]
    pub role: Option<String>,
}

/// What an organization shows about itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OrgProfile {
    pub name: String,
    /// At most 256 characters; null when unset.
    pub description: Option<String>,
    /// 1 to 80 characters, shown beside the name; null when unset.
    #[serde(default)]
    pub display_name: Option<String>,
    /// An http or https website link of at most 200 characters, without credentials; null when unset.
    #[serde(default)]
    pub url: Option<String>,
    /// `/v1/orgs/{org}/avatar?v=` and the start of the image's hash; null when there is no avatar.
    pub avatar_url: Option<String>,
}

/// The organization profile text; each field that is blank or left out is cleared.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UpdateOrgProfileRequest {
    pub description: Option<String>,
    /// 1 to 80 characters, no control characters.
    #[serde(default)]
    pub display_name: Option<String>,
    /// http or https only, at most 200 characters, no user name or password.
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateOrgRequest {
    /// Normalised to a canonical name (trimmed, spaces become `-`, lowercased); shares the namespace with user names: 2 to 39 letters, digits, `-` or `_`, not reserved. The typed form becomes the display name when it differs.
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
    /// At least one of `manage_accounts`, `manage_organizations`, `manage_limits`, `manage_webhooks`, `manage_freeze`.
    pub scopes: Vec<String>,
    /// The credential is refused from this instant on; it must be in the future. Omit for no expiry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    /// Addresses or CIDR blocks (`192.0.2.7`, `10.0.0.0/8`, `2001:db8::/32`) the credential may be used from, at most 64.
    /// Omit or leave empty to allow any address.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_sources: Vec<String>,
    /// Requests per minute this credential may make, at least 1. Omit to use the server default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit_per_minute: Option<u32>,
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
    /// The address of the last accepted request; absent when the server could not tell.
    #[serde(default)]
    pub last_used_from: Option<String>,
    /// When set and not in the future, the credential is expired: refused, but still listed.
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    /// Empty means any address.
    #[serde(default)]
    pub allowed_sources: Vec<String>,
    /// The credential's own limit in requests per minute; absent means the server default.
    #[serde(default)]
    pub rate_limit_per_minute: Option<u32>,
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

/// Whether a signed-out caller can read anything: `private` shows nothing, `public` shows public things.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum AccessMode {
    Private,
    Public,
}

/// A role in a repository, lowest to highest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Reader,
    Writer,
    Maintainer,
    Admin,
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

/// What others may see of an account next to its name: the account name, display name and avatar address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PublicIdentity {
    pub user: String,
    pub display_name: Option<String>,
    /// Same form as `Profile.avatar_url`.
    pub avatar_url: Option<String>,
}

/// A person in the people directory: the public identity and the profile fields their owner shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PersonEntry {
    pub identity: PublicIdentity,
    pub bio: Option<String>,
    pub location: Option<String>,
    pub company: Option<String>,
    #[serde(default)]
    pub links: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PeoplePage {
    /// Ordered by account name.
    pub people: Vec<PersonEntry>,
    /// Pass as `after` to fetch the next page; null on the last page.
    #[schema(required = true)]
    pub next_after: Option<String>,
}

/// A person in an organization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OrgMember {
    pub user: String,
    /// The member's public identity; absent when the account is not active.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<PublicIdentity>,
    #[schema(value_type = OrgRole)]
    pub role: String,
}

/// A pending offer of organization membership. It becomes a membership with `role` when `user` accepts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OrgInvitation {
    pub org: String,
    /// The invited user.
    pub user: String,
    #[schema(value_type = OrgRole)]
    pub role: String,
    pub invited_by: String,
    pub created_at: DateTime<Utc>,
    /// After this the invitation is gone and cannot be accepted.
    pub expires_at: DateTime<Utc>,
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
    /// The id of `user`.
    pub user_id: String,
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

fn default_true() -> bool {
    true
}

/// Where a repository's read-only default came from, most specific first.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ReadOnlySource {
    Repository,
    Organization,
    #[default]
    Default,
}

/// An organization's repository defaults; a field that is absent or null is not set by the organization.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OrgSettingsInfo {
    /// Whether the organization's repositories make exclusive files read-only until their lock is held, where a
    /// repository's own `.pyn/pyn.toml` does not say; unset means the built-in default (on).
    #[serde(default)]
    pub exclusive_read_only: Option<bool>,
}

/// Replaces the organization's settings: a field left out or null is cleared.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct SetOrgSettingsRequest {
    #[serde(default)]
    pub exclusive_read_only: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RepoInfo {
    /// Stable, never reused and unchanged by a rename; a deleted repository's id is never issued again.
    pub id: String,
    /// The id of `owner`, a user or an organization.
    pub owner_id: String,
    pub owner: String,
    /// The owning organization's public identity (its avatar); absent when a user owns the repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_identity: Option<PublicIdentity>,
    pub name: String,
    /// One line of plain text for the About block; null when unset.
    #[serde(default)]
    #[schema(required = true)]
    pub description: Option<String>,
    /// An http or https link for the About block; null when unset.
    #[serde(default)]
    #[schema(required = true)]
    pub website: Option<String>,
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
    /// Whether exclusive files are read-only in a work tree until their lock is held: the policy file's
    /// `meta.exclusive_read_only`, else the owning organization's setting, else on.
    #[serde(default = "default_true")]
    pub exclusive_read_only: bool,
    /// Which of the three decided `exclusive_read_only`.
    #[serde(default)]
    pub exclusive_read_only_source: ReadOnlySource,
    pub created_at: DateTime<Utc>,
    /// How many accounts starred the repository. Anyone who can read the repository sees it, private ones included.
    #[serde(default)]
    #[schema(required = true)]
    pub star_count: u64,
    /// Whether the caller starred it; false for a signed-out caller.
    #[serde(default)]
    #[schema(required = true)]
    pub starred: bool,
    /// The caller's role in the repository; absent when they have none.
    #[schema(value_type = Option<Role>)]
    pub role: Option<String>,
    /// The caller's watch level; null when they chose none, and for a signed-out caller.
    #[serde(default)]
    #[schema(required = true)]
    pub watching: Option<WatchLevel>,
    /// How many accounts watch the repository at `all` or `locks`. Anyone who can read the repository sees it.
    #[serde(default)]
    #[schema(required = true)]
    pub watcher_count: u64,
}

/// What a watcher hears about: every event of the repository, only its lock events, or nothing at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum WatchLevel {
    All,
    Locks,
    Ignore,
}

/// The caller's watch level on a repository and how many accounts watch it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct WatchState {
    /// Null when the caller chose no level.
    #[schema(required = true)]
    pub level: Option<WatchLevel>,
    pub watchers: u64,
}

/// Body of `PUT /v1/repos/{owner}/{name}/watch`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SetWatchRequest {
    pub level: WatchLevel,
}

/// One repository in the caller's list of watched repositories.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct WatchedRepo {
    pub level: WatchLevel,
    /// When the level was last set.
    pub updated_at: DateTime<Utc>,
    pub repo: RepoInfo,
}

/// A repository's star count and whether the caller starred it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct StarState {
    pub starred: bool,
    pub count: u64,
}

/// One repository in an account's list of stars.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct StarredRepo {
    pub starred_at: DateTime<Utc>,
    pub repo: RepoInfo,
}

/// A repository in the administrator's listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AdminRepoEntry {
    pub id: String,
    pub owner: String,
    pub name: String,
    pub visibility: Visibility,
    pub created_at: DateTime<Utc>,
}

/// One page of every repository on the server, of any visibility.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AdminRepoPage {
    /// Ordered by owner, then name.
    pub repos: Vec<AdminRepoEntry>,
    /// Pass as `after` to fetch the next page; null on the last page.
    #[schema(required = true)]
    pub next_after: Option<String>,
}

/// An organization in the administrator's listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AdminOrgEntry {
    pub id: String,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub member_count: u64,
}

/// One page of every organization on the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AdminOrgPage {
    /// Ordered by name.
    pub organizations: Vec<AdminOrgEntry>,
    /// Pass as `after` to fetch the next page; null on the last page.
    #[schema(required = true)]
    pub next_after: Option<String>,
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
    /// Normalised to a canonical name (trimmed, spaces become `-`, lowercased): 1 to 100 letters, digits, `-`, `_` or `.`, starting with a letter or digit and not ending in `.`.
    pub name: String,
    /// Defaults to `private`.
    pub visibility: Option<Visibility>,
    /// Defaults to 8.
    pub lease_hours: Option<u32>,
    /// Locks one user may hold; defaults to the server's. A limit in the repository's policy file overrides it.
    pub max_locks_per_user: Option<u32>,
    /// Defaults to `true`: the first commit holds `.pyn/pyn.toml` with the starter policy (shared by default, exclusive
    /// rules for object and binary files). `false` leaves the repository empty, under the server's initial policy.
    pub starter_policy: Option<bool>,
    /// The starter policy's `[meta] default`: whether paths no entry covers are `shared` or `exclusive`. Defaults to
    /// `shared`; ignored when `starter_policy` is `false`.
    pub starter_default: Option<Mode>,
    /// One line, at most 350 characters; blank means none.
    #[serde(default)]
    pub description: Option<String>,
    /// An http or https link; blank means none.
    #[serde(default)]
    pub website: Option<String>,
}

/// Fields left out stay as they are.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct UpdateRepoRequest {
    /// The new name, normalised like a new repository's.
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
    /// One line, at most 350 characters; `null` or blank clears it.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(value_type = Option<String>)]
    pub description: Option<Option<String>>,
    /// An http or https link; `null` or blank clears it.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    #[schema(value_type = Option<String>)]
    pub website: Option<Option<String>>,
}

/// Tells an explicit `null` (`Some(None)`) from an absent field (`None`).
fn present<'de, T: Deserialize<'de>, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<Option<T>>, D::Error> {
    Option::deserialize(d).map(Some)
}

/// One permission of a fine-grained token: a resource at a level.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TokenPermission {
    /// Repository permissions: `metadata`, `contents`, `locks`, `policy`, `members`, `administration` or `audit`.
    /// Organization permissions, for an organization owner: `org_members`, `org_teams`, `org_administration` or
    /// `org_audit`. Account permissions, for the caller's own account as owner: `account_profile`,
    /// `account_ssh_keys` or `account_locks`.
    pub resource: String,
    /// `read` or `write` (write includes read); `admin` for `locks` only (includes write). `org_audit` and
    /// `account_locks` take `read` only.
    pub level: String,
}

/// Which repositories of the token's owner it reaches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RepositoryAccess {
    /// `public` (none beyond the public repositories every token can read), `all` (every repository the owner
    /// has now and later) or `selected` (only `repos`).
    pub mode: String,
    /// `owner/name` of each selected repository, all owned by the token's owner; empty unless `selected`.
    #[serde(default)]
    pub repos: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateTokenRequest {
    pub name: String,
    pub description: Option<String>,
    /// The resource owner: the caller's own account or an organization they belong to. Defaults to the caller.
    pub owner: Option<String>,
    /// When the token expires; without it and without `never` the token lasts 30 days.
    pub expires_at: Option<DateTime<Utc>>,
    /// Never expires; not combinable with `expires_at`.
    #[serde(default)]
    pub never: bool,
    pub repository_access: RepositoryAccess,
    /// Metadata read is implied for `all` and `selected`, which need at least one repository permission; `public`
    /// takes no repository permissions. Organization and account permissions go with any repository access.
    pub permissions: Vec<TokenPermission>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TokenInfo {
    pub id: String,
    /// The account that created the token and whose role it is capped by.
    pub user: String,
    pub name: String,
    pub description: Option<String>,
    /// The resource owner.
    pub owner: String,
    pub repository_access: RepositoryAccess,
    pub permissions: Vec<TokenPermission>,
    /// Issued by `pyn login`: a full-scope credential rather than a fine-grained token.
    pub sign_in: bool,
    pub created_at: DateTime<Utc>,
    /// Absent means the token never expires.
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
    #[schema(value_type = Role)]
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
    /// The member's public identity; absent when the account is not active.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<PublicIdentity>,
    /// The effective role: the highest of the direct grant, team grants and organization ownership.
    #[schema(value_type = Role)]
    pub role: String,
    #[schema(value_type = MemberSource)]
    pub source: String,
}

/// A team of the owning organization and its role in a repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RepoTeam {
    /// Stable id of the team; never reused after a delete.
    pub team_id: String,
    pub slug: String,
    pub name: String,
    pub description: String,
    #[schema(value_type = Role)]
    pub role: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SetTeamRoleRequest {
    #[schema(value_type = Role)]
    pub role: String,
}

/// A repository where a team holds a role.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TeamRepo {
    /// `owner/name`.
    pub repo: String,
    #[schema(value_type = Role)]
    pub role: String,
}

/// A team of an organization with its members and repository roles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TeamInfo {
    /// Stable id: never changes and is never issued again after a delete, so key data on it, not the slug.
    pub id: String,
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
    #[schema(value_type = Role)]
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

/// The collaboration mode of a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum PolicyMode {
    Shared,
    Exclusive,
}

/// One path entry of the policy, in the order the file lists them (exclusive entries, then shared).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PolicyRule {
    /// The entry as written: an exact path, a glob, or a folder with a trailing `/`.
    pub pattern: String,
    pub mode: PolicyMode,
}

/// Where the active policy came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum PolicySource {
    /// `.pyn/pyn.toml` at the default branch head.
    File,
    /// The server's initial policy: the repository has no policy file yet, or the file is invalid.
    Initial,
}

/// The mode of one requested path and the rule that decided it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PolicyPath {
    pub path: String,
    pub mode: PolicyMode,
    /// The deciding entry; absent when the default mode (or the policy file's own rule) applied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<PolicyPathRule>,
}

/// The entry of `rules` that decided a path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PolicyPathRule {
    /// Position in `rules`.
    pub index: u32,
    pub pattern: String,
}

/// The policy in force at the default branch head.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ActivePolicy {
    /// The mode of a path no rule covers.
    pub default_mode: PolicyMode,
    pub rules: Vec<PolicyRule>,
    pub source: PolicySource,
    /// The default branch head the policy was read at; absent before the first commit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// Set when `.pyn/pyn.toml` is invalid at the head and the initial policy is shown instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
    /// One entry per `path` query parameter, in request order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<PolicyPath>,
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
    /// Paths where `lockable` in `.gitattributes` (at the default branch tip) and the proposed policy's mode disagree. Advisory: the policy stays the authority.
    #[serde(default)]
    pub disagreements: Vec<LockableDisagreement>,
}

/// A path whose `lockable` attribute and policy mode disagree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LockableDisagreement {
    pub path: String,
    pub kind: LockableDisagreementKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LockableDisagreementKind {
    /// Marked `lockable`, but the policy makes it shared.
    LockableShared,
    /// Exclusive in the policy, but not marked `lockable`.
    ExclusiveNotLockable,
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
    /// The name the event was recorded with; the log never rewrites it.
    pub actor: String,
    /// The actor's name now, when it differs from `actor`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_current: Option<String>,
    /// The actor's public identity (the current name's account when renamed); absent when no active account holds it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_public: Option<PublicIdentity>,
    /// The administrator who acted as `actor` in an access session; absent for an ordinary request. Covered by `hash`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acting_admin: Option<String>,
    pub action: String,
    pub path: Option<String>,
    pub detail: String,
    /// SHA-256 hex over the event's canonical encoding, which includes `previous`.
    pub hash: String,
    /// The hash of the preceding event in the same log; absent for the first.
    pub previous: Option<String>,
}

/// The accounts that wrote revisions on a branch, for a history author filter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct HistoryAuthors {
    /// Account names as written in the revisions (what `author` filters on), sorted; a renamed account's older revisions keep its old name.
    pub authors: Vec<String>,
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
    /// The server can register and sign in with passkeys: it has a public address WebAuthn accepts (a host name over
    /// https, or localhost). Without it, only authenticator apps are offered.
    #[serde(default)]
    pub passkeys: bool,
    /// The server's release version; empty when an older server did not report one.
    #[serde(default)]
    #[schema(required = true)]
    pub version: String,
    /// The commit the server binary was built from; null when the build did not record one.
    #[serde(default)]
    pub revision: Option<String>,
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
    /// What setup recorded or an administrator set later; before setup, the configured default.
    #[schema(value_type = AccessMode)]
    pub access_mode: String,
}

/// The server's access mode, as read and as changed by an administrator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ServerAccess {
    #[schema(value_type = AccessMode)]
    pub access_mode: String,
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
    /// Omit it to take the server's configured default (`PYN_ACCESS`, else `private`).
    #[serde(default)]
    #[schema(value_type = Option<AccessMode>)]
    pub access_mode: Option<String>,
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
    /// Stable, never reused.
    pub id: String,
    pub user: String,
    pub email: Option<String>,
    pub email_verified: bool,
    #[schema(value_type = AccountStatus)]
    pub status: String,
    pub disabled_at: Option<DateTime<Utc>>,
    pub disabled_reason: Option<String>,
    /// When the account's scheduled deletion runs, as in `GET /v1/account/deletion`; null when none is scheduled
    /// (an account disabled with no schedule is deleted by the server's retention setting instead). Filled on
    /// `GET /v1/admin/users`.
    #[serde(default)]
    pub purge_at: Option<DateTime<Utc>>,
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
    /// The authenticator-app code (or a recovery code) of an account with a second factor. Without it such an
    /// account gets `401 second_factor_required` after a right password.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// `POST /v1/session` only: stay signed in for `PYN_SESSION_REMEMBER_DAYS` instead of `PYN_SESSION_DAYS`.
    /// Absent or false keeps the usual lifetime.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub remember: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ChangePasswordRequest {
    /// Required when the account already has a password.
    pub current: Option<String>,
    pub new: String,
}

/// A second factor the account has turned on.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TwoFactorFactor {
    pub id: String,
    /// `totp` (an authenticator app).
    pub kind: String,
    pub label: String,
    pub created_at: DateTime<Utc>,
}

/// The caller's second-factor state.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TwoFactorStatus {
    /// Whether the account has a confirmed factor or a passkey, so sign-in asks for a second step.
    pub enabled: bool,
    pub factors: Vec<TwoFactorFactor>,
    /// Passkeys and security keys; each one also counts as a second factor, so `enabled` is true when there is one.
    #[serde(default)]
    pub passkeys: Vec<Passkey>,
    /// Whether this server can register passkeys (see `ServerCapabilities.passkeys`).
    #[serde(default)]
    pub passkeys_available: bool,
    pub recovery_codes_left: usize,
    /// What still requires the account to enrol (see `Account.two_factor_required`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<TwoFactorRequired>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct BeginTotpRequest {
    /// A name for the app, such as `phone`. Defaults to `Authenticator app`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The current password. Required to turn two-factor authentication on for an account that has a password; an
    /// account that already has a factor needs a fresh check instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
}

/// A pending authenticator-app enrolment. The seed is shown once; the factor counts only after `confirm`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TotpEnrolment {
    pub id: String,
    /// The seed in base32, for typing into an app.
    pub secret: String,
    /// The `otpauth://` URI to show as a QR code.
    pub uri: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CodeRequest {
    /// Six digits from the authenticator app, or a recovery code where the route says so.
    pub code: String,
}

/// A factor that is now on.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TotpConfirmed {
    pub factor: TwoFactorFactor,
    /// Ten single-use recovery codes, shown only now, and only with the account's first factor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_codes: Option<Vec<String>>,
    pub fresh_until: DateTime<Utc>,
}

/// New recovery codes, shown once. The old ones no longer work.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RecoveryCodes {
    pub recovery_codes: Vec<String>,
}

/// Until when the calling credential may do sensitive actions without another second-factor check.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FreshCheck {
    pub fresh_until: DateTime<Utc>,
}

/// A passkey or security key the account has registered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Passkey {
    pub id: String,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    /// How the authenticator reaches the browser (`usb`, `nfc`, `ble`, `internal`, `hybrid`), as the browser reported.
    pub transports: Vec<String>,
    /// The signature counter last seen; 0 for authenticators that keep none.
    pub sign_count: u32,
    /// The passkey can be synced to other devices.
    pub backup_eligible: bool,
    /// The passkey is currently synced or backed up.
    pub backup_state: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct BeginPasskeyRequest {
    /// The current password. Required to add the account's first second factor when it has a password; an account that
    /// already has a factor needs a fresh check instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
}

/// A WebAuthn credential descriptor (`PublicKeyCredentialDescriptorJSON`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PasskeyDescriptor {
    /// Always `public-key`.
    #[serde(rename = "type")]
    pub kind: String,
    /// The credential id, base64url.
    pub id: String,
    pub transports: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PasskeyRp {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PasskeyUser {
    /// base64url
    pub id: String,
    pub name: String,
    pub display_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PasskeyParam {
    /// Always `public-key`.
    #[serde(rename = "type")]
    pub kind: String,
    /// COSE algorithm: -7 (ES256) or -8 (EdDSA).
    pub alg: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PasskeySelection {
    /// `required`: a discoverable credential, so a passkey can sign in without a user name.
    pub resident_key: String,
    /// `required`: the authenticator checks the person (PIN, biometrics).
    pub user_verification: String,
}

/// `PublicKeyCredentialCreationOptionsJSON`: pass it to `PublicKeyCredential.parseCreationOptionsFromJSON` or
/// decode the base64url fields for `navigator.credentials.create`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PasskeyCreationOptions {
    pub rp: PasskeyRp,
    pub user: PasskeyUser,
    /// base64url
    pub challenge: String,
    pub pub_key_cred_params: Vec<PasskeyParam>,
    /// Milliseconds.
    pub timeout: u64,
    pub exclude_credentials: Vec<PasskeyDescriptor>,
    pub authenticator_selection: PasskeySelection,
    /// Always `none`.
    pub attestation: String,
}

/// A registration ceremony to run in the browser; answer it with `RegisterPasskeyRequest` before `expires_at`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PasskeyRegistrationBegun {
    /// Send back with the answer. Single use.
    pub ceremony: String,
    pub public_key: PasskeyCreationOptions,
    pub expires_at: DateTime<Utc>,
}

/// `PublicKeyCredentialRequestOptionsJSON`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PasskeyRequestOptions {
    /// base64url
    pub challenge: String,
    pub timeout: u64,
    pub rp_id: String,
    /// Empty lets the browser offer any discoverable passkey for this server.
    pub allow_credentials: Vec<PasskeyDescriptor>,
    /// Always `required`.
    pub user_verification: String,
}

/// An authentication ceremony to run in the browser (`navigator.credentials.get`); answer it with
/// `PasskeyAssertionRequest` before `expires_at`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PasskeyAuthenticationBegun {
    /// Send back with the answer. Single use.
    pub ceremony: String,
    pub public_key: PasskeyRequestOptions,
    pub expires_at: DateTime<Utc>,
}

/// The authenticator's registration response (`AuthenticatorAttestationResponseJSON`), as `credential.toJSON()` yields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PasskeyAttestationResponse {
    /// base64url
    #[serde(rename = "clientDataJSON")]
    pub client_data_json: String,
    /// base64url
    pub attestation_object: String,
    #[serde(default)]
    pub transports: Vec<String>,
}

/// `RegistrationResponseJSON`; fields beyond these are accepted and ignored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PasskeyAttestationCredential {
    pub id: String,
    pub response: PasskeyAttestationResponse,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RegisterPasskeyRequest {
    pub ceremony: String,
    /// A name for the passkey, such as `laptop`. Defaults to `Passkey`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub credential: PasskeyAttestationCredential,
}

/// The authenticator's authentication response (`AuthenticatorAssertionResponseJSON`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PasskeyAssertionResponse {
    /// base64url
    #[serde(rename = "clientDataJSON")]
    pub client_data_json: String,
    /// base64url
    pub authenticator_data: String,
    /// base64url
    pub signature: String,
    /// base64url; absent for a credential that is not discoverable.
    #[serde(default)]
    pub user_handle: Option<String>,
}

/// `AuthenticationResponseJSON`; fields beyond these are accepted and ignored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PasskeyAssertionCredential {
    /// The credential id, base64url.
    pub id: String,
    pub response: PasskeyAssertionResponse,
}

/// An authentication answer, for a passkey sign-in or a fresh check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PasskeyAssertionRequest {
    pub ceremony: String,
    pub credential: PasskeyAssertionCredential,
    /// Passkey sign-in only: stay signed in for `PYN_SESSION_REMEMBER_DAYS`. Ignored by the fresh check.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub remember: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct BeginPasskeySignInRequest {
    /// Offer this account's passkeys to the browser. Without it the browser picks a discoverable passkey.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RenamePasskeyRequest {
    pub name: String,
}

/// A passkey that is now on.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PasskeyRegistered {
    pub passkey: Passkey,
    /// Ten single-use recovery codes, shown only now, and only with the account's first second factor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_codes: Option<Vec<String>>,
    pub fresh_until: DateTime<Utc>,
}

/// One address an account holds. Only the account's owner ever sees these.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AccountEmail {
    /// Lowercase.
    pub address: String,
    /// False until the link mailed to the address is used; a pending address cannot be the primary.
    pub verified: bool,
    pub verified_at: Option<DateTime<Utc>>,
    /// The address the server uses for the account; always verified.
    pub primary: bool,
    pub added_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AddEmailRequest {
    pub address: String,
}

/// A username change; the old name is free for anyone to take afterwards.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RenameRequest {
    /// The new name, normalised like a registered one: 2 to 39 letters, digits, `-` or `_` (spaces become `-`, case is ignored).
    pub username: String,
    /// The account's current password; omitted only by an account with none, from a web session.
    pub password: Option<String>,
}

/// The answer to a username change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Renamed {
    pub old_name: String,
    pub new_name: String,
    /// The account's repositories; their old addresses now redirect (`X-Pyn-Moved-To`).
    pub repositories: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AddUserRequest {
    pub username: String,
    pub password: String,
    #[schema(value_type = Role)]
    pub role: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateInviteRequest {
    #[schema(value_type = Role)]
    pub role: String,
    /// How long the invitation can be used, in hours.
    pub hours: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct InviteInfo {
    pub id: String,
    #[schema(value_type = Role)]
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
    /// What blocks the account's deletion; present only with `account_owns_resources`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blockers: Option<DeletionBlockers>,
}

/// Response header carrying the revision number of returned content.
pub const REVISION_HEADER: &str = "x-pyn-revision";
/// Carries the content's git blob id on `GET .../content`, so a client can verify what it downloaded.
pub const CONTENT_HASH_HEADER: &str = "x-pyn-content";

/// Set on a successful response for a repository address that only a rename keeps alive; its value is the
/// repository's current `owner/name`, so a client can update its remote.
pub const MOVED_TO_HEADER: &str = "x-pyn-moved-to";

/// Dev-only identity header.
pub const DEV_USER_HEADER: &str = "x-pyn-user";

/// Cookie holding the web session.
pub const SESSION_COOKIE: &str = "pyn_session";

/// Header carrying the CSRF token on state-changing requests made with a session cookie.
pub const CSRF_HEADER: &str = "x-pyn-csrf";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SessionInfo {
    pub user: String,
    /// True when an administrator set the password and the person must replace it (`PUT /v1/me/password`).
    /// Advisory: the server does not refuse other requests until it is changed.
    #[serde(default)]
    pub password_change_required: bool,
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
    /// Whether a passkey can sign in (`POST /v1/session/passkey/begin`); false when the server has no public address
    /// WebAuthn accepts.
    #[serde(default)]
    pub passkeys: bool,
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OwnerLimits {
    /// The id of the user or organization these limits belong to.
    pub owner_id: String,
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

/// Whether an owner is frozen: every write under it is refused with `owner_frozen` while reads continue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OwnerFreeze {
    pub owner: String,
    /// The stable id of the user or organization.
    pub owner_id: String,
    pub kind: OwnerKind,
    pub frozen: bool,
    /// When it was frozen; null while it is not.
    #[schema(required = true)]
    pub frozen_at: Option<DateTime<Utc>>,
    /// Why it was frozen; null while it is not.
    #[schema(required = true)]
    pub reason: Option<String>,
}

/// Freezes an owner, or replaces the reason and time of a freeze already in force.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FreezeRequest {
    /// Shown to the owner's members: 1 to 512 bytes.
    pub reason: String,
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
    /// Bytes of distinct revision content, counted once per repository.
    pub stored_bytes: u64,
    /// Bytes of LFS objects. The storage limit applies to `stored_bytes` plus this.
    pub lfs_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RepoUsage {
    /// `owner/name`.
    pub repository: String,
    /// The repository's id.
    pub repo_id: String,
    /// Bytes of distinct revision content.
    pub stored_bytes: u64,
    /// Bytes of LFS objects. The storage cap applies to `stored_bytes` plus this.
    pub lfs_bytes: u64,
    /// Paths with a head revision.
    pub files: u64,
    pub revisions: u64,
    /// The repository's own storage cap in bytes; null when it has none.
    #[serde(default)]
    #[schema(required = true)]
    pub storage_cap: Option<u64>,
}

/// Distinct stored bytes of one top-level folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct FolderUsage {
    /// The top-level folder; empty for the files at the root.
    pub folder: String,
    pub stored_bytes: u64,
    /// Paths with a revision under the folder.
    pub files: u64,
}

/// Distinct stored bytes of one path across its revisions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PathUsage {
    pub path: String,
    pub stored_bytes: u64,
    pub revisions: u64,
}

/// Where a repository's stored bytes sit. Content shared by several folders or paths counts in each, so their sums
/// can pass `stored_bytes`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RepoUsageBreakdown {
    /// `owner/name`.
    pub repository: String,
    pub repo_id: String,
    /// Distinct bytes in the whole repository, each content once.
    pub stored_bytes: u64,
    /// Bytes of the repository's LFS objects, not part of the folders and paths.
    pub lfs_bytes: u64,
    #[schema(required = true)]
    pub storage_cap: Option<u64>,
    /// Largest first.
    pub folders: Vec<FolderUsage>,
    /// The 20 largest, largest first.
    pub largest_paths: Vec<PathUsage>,
}

/// The new storage cap of a repository; `null` removes it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SetRepoStorageCapRequest {
    #[serde(default)]
    #[schema(required = true)]
    pub storage_bytes: Option<u64>,
}

/// A repository's own storage cap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RepoStorageCap {
    /// `owner/name`.
    pub repository: String,
    pub repo_id: String,
    #[schema(required = true)]
    pub storage_bytes: Option<u64>,
}

/// What an owner uses against its limits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OwnerUsage {
    pub owner: String,
    /// The id of `owner`.
    pub owner_id: String,
    pub limits: OwnerLimits,
    pub usage: Usage,
    /// Largest first, then by address.
    pub repositories: Vec<RepoUsage>,
}

/// One break found by verification: `hash_mismatch` (a record's fields do not give its hash), `version_mismatch` (a revision's `hash_version` disagrees with its stored fields), `broken_link` (it does
/// not follow the record before it) `base_mismatch` (a revision's base is not an older revision of its path) or `pointer_mismatch` (a commit's pointer cache differs from the revisions it made and adopted). Revision breaks carry `path` and `revision`; audit breaks carry `event`.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_revision_is_null_without_one() {
        let c = ServerCapabilities {
            local_commits: true,
            passkeys: false,
            version: "0.1.2".into(),
            revision: None,
        };
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!(v["version"], "0.1.2");
        assert!(v["revision"].is_null());
    }

    #[test]
    fn capabilities_from_an_older_server_still_parse() {
        let c: ServerCapabilities = serde_json::from_str(r#"{"local_commits":true}"#).unwrap();
        assert!(c.local_commits);
        assert_eq!(c.version, "");
        assert_eq!(c.revision, None);
    }
}
