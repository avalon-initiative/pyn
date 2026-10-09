//! Domain model and policy. No database, HTTP or filesystem dependency; infrastructure sits behind traits.

pub mod access;
pub mod access_service;
pub mod audit;
pub mod auth;
pub mod clock;
#[cfg(feature = "contract")]
pub mod contract;
pub mod error;
pub mod history;
pub mod memory;
pub mod object;
pub mod repo;
pub mod repositories;
pub mod rules;
pub mod service;
pub mod store;
pub mod tree;
pub mod types;

pub use access::{
    Credential, Identity, InviteId, InviteRecord, Permission, Principal, RegistrationMode, Role,
    RoleDefinitions, SessionRecord, SshKeyRecord, TokenId, TokenRecord, account, invite, session,
    ssh, token,
};
pub use access_service::{AccessConfig, AccessService, AccessStore};
pub use audit::{AuditAction, AuditEvent, AuditQuery, AuditStore, NewAuditEvent};
pub use auth::AuthProvider;
pub use clock::{Clock, ManualClock, SystemClock};
pub use error::{PynError, Result};
pub use history::{HISTORY_DEFAULT_LIMIT, HISTORY_MAX_LIMIT, HistoryCursor, HistoryPage};
pub use object::ObjectStore;
pub use repo::{
    DEFAULT_MAX_LOCKS_PER_USER, MAX_LOCKS_PER_USER_CEILING, RepoRecord, RepoSettings, RepoUpdate,
    Visibility, validate_max_locks,
};
pub use repositories::Repositories;
pub use rules::{Mode, PathFilter, Rules};
pub use service::{FileEntry, LockLimit, RepoService, ServiceConfig};
pub use store::MetadataStore;
pub use tree::{DEFAULT_BRANCH, EntryKind, EntryMode, RepoSummary, TreeEntry};
pub use types::{ContentHash, Lock, NewRevision, RepoId, RepoPath, Revision, RevisionId, UserId};
