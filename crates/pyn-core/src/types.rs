use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{PynError, Result};
use crate::rules::Mode;
use pyn_object::{FileMode, ObjectId, is_git_dir_name};

macro_rules! string_id {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            pub fn new(s: impl Into<String>) -> Self {
                Self(s.into())
            }
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

string_id!(RepoId);
string_id!(UserId);
string_id!(
    /// SHA-256 hex over a revision's canonical encoding (docs/concepts/integrity.md).
    RevisionHash
);

/// Longest accepted path, in bytes.
pub const MAX_PATH_BYTES: usize = 4096;
/// Longest accepted path component, in bytes.
pub const MAX_PATH_COMPONENT_BYTES: usize = 255;
/// Reserved workspace prefix for machine-local state; never a repository path.
pub const LOCAL_ONLY_PREFIX: &str = ".pyn/local_only";

/// Repo-relative `/`-separated path; rejects absolute, `.`/`..` and backslash forms.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RepoPath(String);

fn structurally_valid(s: &str) -> bool {
    !(s.is_empty()
        || s.starts_with('/')
        || s.ends_with('/')
        || s.contains('\\')
        || s.contains('\0')
        || s.split('/')
            .any(|seg| seg.is_empty() || seg == "." || seg == ".."))
}

impl RepoPath {
    /// A path for a new write: structural rules plus length, control character and reserved-name limits.
    pub fn new(s: impl Into<String>) -> Result<Self> {
        let s = s.into();
        let local_only = s.len() >= LOCAL_ONLY_PREFIX.len()
            && s.is_char_boundary(LOCAL_ONLY_PREFIX.len())
            && s[..LOCAL_ONLY_PREFIX.len()].eq_ignore_ascii_case(LOCAL_ONLY_PREFIX)
            && matches!(s.as_bytes().get(LOCAL_ONLY_PREFIX.len()), None | Some(b'/'));
        if structurally_valid(&s)
            && let Some(seg) = s.split('/').find(|seg| is_git_dir_name(seg.as_bytes()))
        {
            return Err(PynError::ReservedPathComponent {
                component: seg.to_string(),
                path: s,
            });
        }
        let bad = !structurally_valid(&s)
            || s.len() > MAX_PATH_BYTES
            || s.chars().any(char::is_control)
            || s.split('/').any(|seg| seg.len() > MAX_PATH_COMPONENT_BYTES)
            || local_only;
        if bad {
            Err(PynError::InvalidPath(s))
        } else {
            Ok(Self(s))
        }
    }

    /// A path read back from existing data or used to look one up: only the structural rules apply.
    pub fn from_stored(s: impl Into<String>) -> Result<Self> {
        let s = s.into();
        if structurally_valid(&s) {
            Ok(Self(s))
        } else {
            Err(PynError::InvalidPath(s))
        }
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for RepoPath {
    type Error = PynError;
    fn try_from(s: String) -> Result<Self> {
        Self::from_stored(s)
    }
}

impl From<RepoPath> for String {
    fn from(p: RepoPath) -> String {
        p.0
    }
}

impl std::fmt::Display for RepoPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Longest name or title (token, SSH key), in bytes.
pub const MAX_LABEL_BYTES: usize = 256;
/// Longest commit or revision message, in bytes.
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;
/// Most paths in one changeset.
pub const MAX_CHANGESET_ITEMS: usize = 10_000;
/// Most commits in one push.
pub const MAX_PUSH_COMMITS: usize = 10_000;
/// Most file changes (added, changed or removed paths) in one pushed commit.
pub const MAX_COMMIT_CHANGES: usize = MAX_CHANGESET_ITEMS;
/// Most file changes across all the commits of one push.
pub const MAX_PUSH_CHANGES: usize = 50_000;
/// Most entries in one tree a push brings or a check-in builds.
pub const MAX_TREE_ENTRIES: usize = 100_000;

/// A name or title: 1 to `MAX_LABEL_BYTES` bytes, no control characters. Returns it trimmed.
pub fn validate_label(what: &str, text: &str) -> Result<String> {
    let text = text.trim();
    if text.is_empty() || text.len() > MAX_LABEL_BYTES || text.chars().any(char::is_control) {
        return Err(PynError::InvalidRequest(format!(
            "a {what} is 1 to {MAX_LABEL_BYTES} bytes without control characters"
        )));
    }
    Ok(text.to_string())
}

/// A message: at most `MAX_MESSAGE_BYTES` bytes, control characters only newline, tab and carriage return.
pub fn validate_message(text: &str) -> Result<()> {
    if text.len() > MAX_MESSAGE_BYTES
        || text
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\t' | '\r'))
    {
        return Err(PynError::InvalidRequest(format!(
            "a message is at most {MAX_MESSAGE_BYTES} bytes without control characters other than newline and tab"
        )));
    }
    Ok(())
}

/// Per-path revision number, starting at 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RevisionId(pub u64);

impl std::fmt::Display for RevisionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A commit's author or committer as git records it, apart from the time: the commit's own time applies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Person {
    pub name: String,
    pub email: String,
    /// Minutes east of UTC.
    pub offset_minutes: i32,
}

impl Person {
    /// Strips the characters git's identity line cannot hold; an empty name becomes `unknown`.
    pub fn new(name: &str, email: &str, offset_minutes: i32) -> Self {
        let clean = |s: &str| {
            s.chars()
                .filter(|c| !matches!(c, '<' | '>' | '\n' | '\0'))
                .collect::<String>()
                .trim()
                .to_string()
        };
        let name = clean(name);
        Self {
            name: if name.is_empty() {
                "unknown".to_string()
            } else {
                name
            },
            email: clean(email),
            offset_minutes,
        }
    }

    /// The account's name and email, UTC; the `.invalid` placeholder stands in for an account without an email.
    pub fn for_user(user: &UserId, email: Option<&str>) -> Self {
        match email {
            Some(e) => Self::new(user.as_str(), e, 0),
            None => Self::new(user.as_str(), &format!("{user}@pyn.invalid"), 0),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Revision {
    pub id: RevisionId,
    pub path: RepoPath,
    /// The git blob id of the content.
    pub content: ObjectId,
    /// The content string the v1 hash covers, when it is not the blob id: revisions migrated from SHA-256 content hashes.
    pub legacy_content: Option<String>,
    pub author: UserId,
    /// The structured author the v2 hash covers; `None` for a v1 revision.
    pub person: Option<Person>,
    /// The encoding `hash` uses (`pyn_hash::REVISION_V1`, `REVISION_V2` or `REVISION_V3`); it must agree with
    /// `person` and `deleted`.
    pub hash_version: u32,
    pub message: String,
    pub created_at: DateTime<Utc>,
    /// Set when this revision restores the content of an earlier one.
    pub restored_from: Option<RevisionId>,
    /// The path's mode when the revision was made; `None` for revisions that predate recording.
    pub mode: Option<Mode>,
    /// Git's file mode of the content: regular, executable or symlink.
    pub file_mode: FileMode,
    /// The revision removes the path; `content` and `file_mode` are those of the version it removed.
    pub deleted: bool,
    /// Shared by the revisions a changeset checked in together; `None` for a single-path checkin.
    pub changeset: Option<String>,
    /// The id of the commit this revision belongs to.
    pub commit: ObjectId,
    pub hash: RevisionHash,
    /// Hash of the path's previous revision by number; `None` for the first.
    pub previous: Option<RevisionHash>,
    /// The revision this one was based on: the committing branch's pointer for the path. Revisions of an exclusive
    /// path form a tree through it; `None` when the branch had no revision of the path. Not covered by `hash`.
    pub base: Option<RevisionId>,
}

/// One atomic write: a single checkin or a whole changeset, recorded as a git commit on the single mainline. Its id is
/// the git commit id over `tree`, `parents`, the author and committer and the message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    pub id: ObjectId,
    pub parents: Vec<ObjectId>,
    /// The root git tree of the repository as of this commit.
    pub tree: ObjectId,
    pub author: Person,
    pub committer: Person,
    /// Whole seconds; the author time. Its timezone is `author.offset_minutes`.
    pub authored_at: DateTime<Utc>,
    /// Whole seconds; the committer time.
    pub created_at: DateTime<Utc>,
    /// Display text: the message bytes decoded lossily; `raw` holds them exactly.
    pub message: String,
    /// The commit's `gpgsig` header, if it is signed.
    pub signature: Option<String>,
    pub changeset: Option<String>,
    /// The exact git commit bytes; `id` is their hash and the fields above are derived from them.
    pub raw: Vec<u8>,
    /// The revisions it made, ordered by path.
    pub revisions: Vec<Revision>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewRevision {
    pub path: RepoPath,
    pub content: ObjectId,
    pub file_mode: FileMode,
    pub author: UserId,
    pub person: Person,
    pub message: String,
    pub created_at: DateTime<Utc>,
    pub restored_from: Option<RevisionId>,
    pub mode: Mode,
    /// Removes the path; `content` and `file_mode` are those of the version removed.
    pub deleted: bool,
    /// Length of the content in bytes.
    pub size: u64,
    pub changeset: Option<String>,
}

/// One path of a changeset: its revision, the head it must be based on and, for an exclusive path, the lock holder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangesetItem {
    pub revision: NewRevision,
    pub expected_head: Option<RevisionId>,
    pub lock_holder: Option<UserId>,
}

/// Which commits a listing keeps. Every field narrows the result; `Default` keeps everything.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommitFilter {
    /// Case-insensitive substring of the author's name or email.
    pub author: Option<String>,
    /// Author date at or after this instant.
    pub since: Option<DateTime<Utc>>,
    /// Author date at or before this instant.
    pub until: Option<DateTime<Utc>>,
    /// Case-insensitive substring of the message.
    pub grep: Option<String>,
    /// Commits that made a revision of this path, or of a path under it.
    pub path: Option<RepoPath>,
    /// Drops this commit and its ancestors.
    pub exclude: Option<ObjectId>,
}

impl CommitFilter {
    /// Every criterion except `path` and `exclude`, which need the store.
    pub fn matches_record(&self, c: &Commit) -> bool {
        let has = |hay: &str, needle: &str| hay.to_lowercase().contains(&needle.to_lowercase());
        self.author
            .as_deref()
            .is_none_or(|a| has(&c.author.name, a) || has(&c.author.email, a))
            && self.since.is_none_or(|t| c.authored_at >= t)
            && self.until.is_none_or(|t| c.authored_at <= t)
            && self.grep.as_deref().is_none_or(|g| has(&c.message, g))
    }

    /// Whether the commit made a revision of `path` or of a path under it.
    pub fn matches_path(&self, c: &Commit) -> bool {
        self.path.as_ref().is_none_or(|p| {
            c.revisions.iter().any(|r| {
                r.path == *p
                    || r.path
                        .as_str()
                        .strip_prefix(p.as_str())
                        .is_some_and(|rest| rest.starts_with('/'))
            })
        })
    }
}

/// A client-built commit the service has verified, for `MetadataStore::push_commits_on`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushedCommit {
    /// The commit as its raw bytes describe it, without revisions.
    pub record: Commit,
    /// The trees the commit adds, as `(id, body)`.
    pub trees: Vec<(ObjectId, Vec<u8>)>,
    /// One revision per changed path, ordered by path.
    pub items: Vec<NewRevision>,
    /// Paths of `items` that need no lock although they are exclusive: the policy file's bootstrap.
    pub unlocked: Vec<RepoPath>,
    /// For a merge commit: existing revisions the merge points paths at, taken from the second parent.
    pub adopted: Vec<Revision>,
    /// The repository already holds this commit (from another branch): the push only moves the branch onto it.
    pub known: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lock {
    pub path: RepoPath,
    pub owner: UserId,
    pub acquired_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_cannot_escape_or_alias() {
        for bad in ["", "/etc/passwd", "a/../b", "a//b", "a/", "./a", "a\\b"] {
            assert!(RepoPath::new(bad).is_err(), "{bad:?} should be rejected");
        }
        assert!(RepoPath::new("Content/World/Main.umap").is_ok());
    }

    #[test]
    fn new_writes_reject_reserved_and_oversized_paths() {
        let long_component = "a".repeat(256);
        let long_path = vec!["a".repeat(200); 21].join("/");
        for bad in [
            ".git",
            ".GIT/config",
            "a/.Git/b",
            "a/.git",
            "GIT~1",
            "a/git~12/b",
            "dir/.git. ",
            ".pyn/local_only",
            ".pyn/LOCAL_ONLY/state",
            "a\u{1}b",
            "a\u{7f}b",
            "a\u{85}b",
            "a\nb",
            long_component.as_str(),
            long_path.as_str(),
        ] {
            assert!(RepoPath::new(bad).is_err(), "{bad:?} should be rejected");
        }
        for ok in [
            ".pyn/pyn.toml",
            ".pyn/ignore",
            ".pyn/local_only_notes",
            ".gitignore",
            ".github/ci.yml",
            "a.git/b",
            "git~x",
            "git~",
            "digit~1",
        ] {
            assert!(RepoPath::new(ok).is_ok(), "{ok:?} should be accepted");
        }
        assert!(RepoPath::new("a".repeat(255)).is_ok());
    }

    #[test]
    fn a_short_name_error_names_the_component() {
        let err = RepoPath::new("docs/GIT~1/x").unwrap_err();
        assert_eq!(err.code(), "invalid_path");
        assert!(err.to_string().contains("\"GIT~1\""), "{err}");
    }

    #[test]
    fn labels_and_messages_are_bounded() {
        assert_eq!(validate_label("name", "  ci  ").unwrap(), "ci");
        for bad in ["", "a\u{1}b", "a\nb", &"x".repeat(257)] {
            assert!(validate_label("name", bad).is_err(), "{bad:?}");
        }
        assert!(validate_message("line\n\tindented").is_ok());
        assert!(validate_message("a\u{0}b").is_err());
        assert!(validate_message(&"x".repeat(MAX_MESSAGE_BYTES + 1)).is_err());
    }

    #[test]
    fn stored_paths_keep_reading() {
        assert!(RepoPath::from_stored(".git/config").is_ok());
        assert!(RepoPath::from_stored("a/../b").is_err());
        assert_eq!(
            RepoPath::try_from(".git/x".to_string()).unwrap().as_str(),
            ".git/x"
        );
    }
}
