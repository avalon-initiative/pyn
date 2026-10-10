//! Domain model and policy. No database, HTTP or filesystem dependency; infrastructure sits behind traits.

pub mod access;
pub mod access_service;
pub mod audit;
pub mod auth;
pub mod clock;
#[cfg(feature = "contract")]
pub mod contract;
pub mod email;
pub mod error;
pub mod history;
pub mod memory;
pub mod object;
pub mod passwords;
pub mod ratelimit;
pub mod repo;
pub mod repo_policy;
pub mod repositories;
pub mod rules;
pub mod service;
pub mod store;
pub mod tree;
pub mod types;

pub use access::{
    AccountKind, AccountRecord, AccountStatus, Credential, Identity, InviteId, InviteRecord,
    NewAccount, ORG_DELETE_MARK_SECONDS, OrgCreation, OrgRole, Permission, Principal,
    RegistrationMode, Role, RoleDefinitions, RoleSource, ServerSettings, ServiceCredentialRecord,
    ServiceScope, SessionRecord, SignupStage, SshKeyRecord, TeamRecord, TokenId, TokenRecord,
    VerificationRecord, account, invite, service_credential, session, ssh, team, token,
    verification,
};
pub use access_service::{
    AccessConfig, AccessService, AccessStore, AccountDisable, AdminRevoke, OrgDeleteMark,
    OrgMemberChange, RateLimits, Registration, RepoMember, SERVER_AUDIT_ID, SetupRequest,
    SetupStatus, SignUp, TeamDetail, generate_setup_token,
};
pub use audit::{AuditAction, AuditEvent, AuditQuery, AuditScope, AuditStore, NewAuditEvent};
pub use auth::AuthProvider;
pub use clock::{Clock, ManualClock, SystemClock};
pub use email::{EmailMessage, EmailSender, MemoryEmailSender, NullEmailSender};
pub use error::{PynError, Result};
pub use history::{HISTORY_DEFAULT_LIMIT, HISTORY_MAX_LIMIT, HistoryCursor, HistoryPage};
pub use object::ObjectStore;
pub use passwords::{InlinePasswords, PasswordWorker};
pub use ratelimit::{RateLimitStore, RateState};
pub use repo::{
    DEFAULT_MAX_LOCKS_PER_USER, MAX_LOCKS_PER_USER_CEILING, RepoRecord, RepoSettings, RepoUpdate,
    Visibility, validate_max_locks,
};
pub use repo_policy::{
    CreationEffect, CreationRule, CreationScope, CreationSubject, MemberCreation, RepoPolicy,
    Standing, SubjectKind,
};
pub use repositories::Repositories;
pub use rules::{Mode, PathFilter, Rules};
pub use service::{FileEntry, LockLimit, RepoService, ServiceConfig};
pub use store::MetadataStore;
pub use tree::{DEFAULT_BRANCH, EntryKind, EntryMode, RepoSummary, TreeEntry};
pub use types::{ContentHash, Lock, NewRevision, RepoId, RepoPath, Revision, RevisionId, UserId};
