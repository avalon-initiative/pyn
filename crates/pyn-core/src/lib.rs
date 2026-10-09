//! Domain model and policy. No database, HTTP or filesystem dependency; infrastructure sits behind traits.

pub mod access;
pub mod access_service;
pub mod audit;
pub mod auth;
pub mod clock;
#[cfg(feature = "contract")]
pub mod contract;
pub mod error;
pub mod memory;
pub mod object;
pub mod repo;
pub mod repositories;
pub mod rules;
pub mod service;
pub mod store;
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
pub use object::ObjectStore;
pub use repo::{RepoRecord, RepoSettings, RepoUpdate, Visibility};
pub use repositories::Repositories;
pub use rules::{Mode, Rules};
pub use service::{FileEntry, RepoService, ServiceConfig};
pub use store::MetadataStore;
pub use types::{ContentHash, Lock, NewRevision, RepoId, RepoPath, Revision, RevisionId, UserId};
