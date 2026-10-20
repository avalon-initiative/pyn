//! Domain model and policy. No database, HTTP or filesystem dependency; infrastructure sits behind traits.

pub mod access;
pub mod access_service;
pub mod audit;
pub mod auth;
pub mod branch;
pub mod chunking;
pub mod clock;
pub mod commit_record;
#[cfg(feature = "contract")]
pub mod contract;
pub mod email;
pub mod error;
pub mod gate;
pub mod history;
pub mod integrity;
pub mod limits;
pub mod memory;
pub mod merge;
pub mod object;
pub mod oidc;
pub mod passwords;
pub mod ratelimit;
pub mod repo;
pub mod repo_policy;
pub mod repositories;
pub mod rules;
pub mod service;
pub mod store;
pub mod stranded;
pub mod tree;
pub mod types;
pub mod webhook;

pub use access::{
    AccountKind, AccountRecord, AccountStatus, Credential, Identity, InviteId, InviteRecord,
    NewAccount, ORG_DELETE_MARK_SECONDS, OrgCreation, OrgRole, Permission, Principal,
    RegistrationMode, Role, RoleDefinitions, RoleSource, ServerSettings, ServiceCredentialRecord,
    ServiceScope, SessionRecord, SignupStage, SshKeyRecord, TeamRecord, TokenId, TokenRecord,
    VerificationRecord, account, invite, service_credential, session, ssh, team, token,
    verification,
};
pub use access_service::{
    AccessConfig, AccessService, AccessStore, AccountDisable, AdminRevoke, ExternalPolicy,
    ExternalSignIn, MAX_INVITE_HOURS, OidcBegin, OidcFinish, OidcStart, OrgDeleteMark,
    OrgMemberChange, OwnerLimits, RateLimits, Registration, RepoMember, SERVER_AUDIT_ID,
    SetupRequest, SetupStatus, SignUp, TeamDetail, generate_setup_token,
};
pub use audit::{AuditAction, AuditEvent, AuditQuery, AuditScope, AuditStore, NewAuditEvent};
pub use auth::AuthProvider;
pub use branch::{Branch, DEFAULT_BRANCH, validate_branch_name};
pub use chunking::ChunkedStore;
pub use clock::{Clock, ManualClock, SystemClock};
pub use email::{EmailMessage, EmailSender, MemoryEmailSender, NullEmailSender};
pub use error::{PynError, Result};
pub use history::{
    HISTORY_DEFAULT_LIMIT, HISTORY_MAX_LIMIT, HISTORY_SCAN_BUDGET, HistoryCursor, HistoryPage,
    HistoryScan, PathHistoryPage, decode_path_cursor, encode_path_cursor,
};
pub use integrity::{BreakKind, Problem, VerifyReport};
pub use limits::{
    Limits, LimitsChange, MAX_LIMIT_VALUE, RepoUsage, StorageCap, StorageCaps, Usage,
};
pub use merge::{
    Change, Conflict, ConflictKind, FastForward, MergeOutcome, MergeReport, MergeRequest,
    Resolution,
};
pub use object::{
    KeyHasher, Keying, ObjectRead, ObjectReader, ObjectStore, REPO_ALGORITHM, Stored,
};
pub use oidc::{
    AuthorizationRequest, CodeExchange, ExternalClaims, ExternalCreate, ExternalIdentity,
    IdentityUnlink, NewExternalAccount, OidcFlow, OidcProvider, pkce_challenge,
};
pub use passwords::{InlinePasswords, PasswordWorker};
pub use pyn_object::{FileMode, ObjectId};
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
pub use service::{
    ChangeEntry, CommitPage, Deletion, FETCH_DEFAULT_LIMIT, FETCH_MAX_EXAMINED, FETCH_MAX_HAVES,
    FETCH_MAX_LIMIT, FetchPage, FetchQuery, FetchedCommit, FileEntry, LockLimit, PolicyPreview,
    PushObjects, Pushed, Recorded, RepoService, ServiceConfig, Snapshot, SnapshotEntry,
    VERIFY_BUDGET,
};
pub use store::{MetadataStore, UploadReservation};
pub use stranded::{ConversionNote, StrandedVersion};
pub use tree::{EntryKind, EntryMode, HeadCounts, RepoSummary, TREE_MAX_PATHS, TreeEntry};
pub use types::{
    ChangesetItem, Commit, Lock, MAX_CHANGESET_ITEMS, MAX_COMMIT_CHANGES, MAX_LABEL_BYTES,
    MAX_MESSAGE_BYTES, MAX_PATH_BYTES, MAX_PATH_COMPONENT_BYTES, MAX_PUSH_CHANGES,
    MAX_PUSH_COMMITS, MAX_TREE_ENTRIES, NewRevision, Person, PushedCommit, RepoId, RepoPath,
    Revision, RevisionHash, RevisionId, UserId,
};
pub use webhook::{
    Delivery, DeliveryQuery, DeliveryStatus, DueDelivery, FixedJitter, Jitter, MAX_ACTIVE_SECRETS,
    MAX_ATTEMPTS, NewSecret, NewSubscription, OutboxEvent, RETENTION_DAYS, RandomJitter,
    Subscription, SubscriptionUpdate, WebhookSecret, WebhookStore, retry_delay,
};
