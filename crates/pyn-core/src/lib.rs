//! Domain model and policy. No database, HTTP or filesystem dependency; infrastructure sits behind traits.

pub mod access;
pub mod access_service;
pub mod access_session;
pub mod account_self;
pub mod audit;
pub mod auth;
pub mod branch;
pub mod chunking;
pub mod clock;
pub mod commit_record;
#[cfg(feature = "contract")]
pub mod contract;
pub mod deletion;
pub mod email;
pub mod error;
pub mod freeze;
pub mod gate;
pub mod history;
pub mod integrity;
pub mod limit_events;
pub mod limits;
pub mod memory;
pub mod merge;
pub mod object;
pub mod oidc;
pub mod org_settings;
pub mod passwords;
pub mod profile;
pub mod profiles;
pub mod pull;
pub mod ratelimit;
pub mod relying_party;
pub mod repo;
pub mod repo_policy;
pub mod repositories;
pub mod rules;
pub mod service;
pub mod source_range;
pub mod store;
pub mod stranded;
pub mod tag;
pub mod totp;
pub mod tree;
pub mod two_factor;
pub mod types;
pub mod watch;
pub mod webhook;
pub mod webhook_secret;
pub mod webhook_url;

#[cfg(any(feature = "contract", feature = "test-support"))]
pub use access::fixtures;
pub use access::{
    AccessMode, AccountKind, AccountRecord, AccountStatus, Credential, GrantLevel, GrantNeed,
    GrantScope, Identity, InvitationAccept, InvitationCreate, InvitationGrant, InviteId,
    InviteRecord, NewAccount, ORG_DELETE_MARK_SECONDS, OrgCreation, OrgInvitation, OrgMemberAdd,
    OrgRole, Permission, Principal, RegistrationMode, RepoAccess, Role, RoleDefinitions,
    RoleSource, ServerInvitation, ServerSettings, ServiceCredentialRecord, ServiceScope,
    SessionRecord, SignupStage, SshKeyRecord, TeamRecord, TokenId, TokenRecord, TokenResource,
    VerificationRecord, account, invite, permissions_of, server_invite, service_credential,
    session, ssh, team, token, verification,
};
pub use access_service::{
    ACCOUNT_DELETION_ACTOR, AccessConfig, AccessService, AccessStore, AccountDisable, AdminRevoke,
    CreatedServerInvitation, DEFAULT_SERVER_INVITE_DAYS, DEFAULT_TOKEN_DAYS, EmailStatus,
    ExternalPolicy, ExternalSignIn, MAX_INVITE_HOURS, MAX_SERVER_INVITE_DAYS, MailCheck,
    MailFailure, NewServerInvitation, NewToken, OidcBegin, OidcFinish, OidcStart, OrgAdd,
    OrgDeleteMark, OrgMemberChange, OwnerFreezeState, OwnerLimits, PasskeyAssertion,
    PasskeyAttestation, PasskeyCreation, PasskeyRegistered, PasskeyRequest, RateLimits,
    Registration, RepoMember, SERVER_AUDIT_ID, ServiceCredentialLimits, SetupRequest, SetupStatus,
    SignUp, TeamDetail, TestEmail, TokenExpiry, generate_setup_token,
};
pub use access_service::{
    AdminRecovery, CreateWebhook, DeliveryPage, HOST_RECOVERY_ACTOR, IssuedSecret, TotpConfirmed,
    TotpEnrolment, TwoFactorStatus, WebhookDetail, WebhookSecretInfo, WebhookSettings,
};
pub use access_session::{
    ACCESS_SESSION_MAX_ROUTES, ACCESS_SESSION_MINUTES, ACCESS_SESSION_NOTES_CHARS,
    AccessSessionEnd, AccessSessionReach, AccessSessionReason, AccessSessionRecord,
    AccessSessionView, OVERFLOW_ROUTE, ReachNote, session_credential,
};
pub use account_self::{
    AccountEmail, EmailAdd, EmailRemove, NameSpan, PrimaryChange, RenameOutcome,
};
pub use audit::{
    Actor, AuditAction, AuditEvent, AuditQuery, AuditScope, AuditStore, NewAuditEvent,
};
pub use auth::AuthProvider;
pub use branch::{Branch, DEFAULT_BRANCH, validate_branch_name};
pub use chunking::ChunkedStore;
pub use clock::{Clock, ManualClock, ScaledClock, SystemClock};
pub use deletion::{
    AccountDeletion, AccountPurge, DeletionSchedule, DeletionSweep, NAME_RESERVATION_DAYS,
    NameReservation, PurgeBlockers,
};
pub use email::{
    EmailMessage, EmailSender, MailerKind, MemoryEmailSender, NullEmailSender, invitation_message,
    welcome_message,
};
pub use error::{PasskeyReason, PynError, Result};
pub use freeze::{FREEZE_REASON_BYTES, MemoryOwnerWrites, OwnerWrites};
pub use history::{
    HISTORY_DEFAULT_LIMIT, HISTORY_MAX_LIMIT, HISTORY_SCAN_BUDGET, HistoryBy, HistoryCursor,
    HistoryPage, HistoryScan, PathHistoryPage, decode_path_cursor, encode_path_cursor,
};
pub use integrity::{BreakKind, Problem, VerifyReport};
pub use limit_events::{
    DomainEvent, LimitEvents, LimitKind, LimitReached, LimitsEvent, LockChange, LockEventBuilder,
    MemoryLimitEvents,
};
pub use limits::{
    BREAKDOWN_PATHS, FolderUsage, Limits, LimitsChange, MAX_LIMIT_VALUE, PathUsage, RepoBreakdown,
    RepoUsage, StorageCap, StorageCaps, Usage, top_folder,
};
pub use merge::{
    Change, Conflict, ConflictKind, FastForward, MergeOutcome, MergeReport, MergeRequest,
    Resolution,
};
pub use object::{
    KeyHasher, KeyPage, Keying, ObjectRead, ObjectReader, ObjectStore, REPO_ALGORITHM, Stored,
    StoredKey, StoredKind, page_of_keys,
};
pub use oidc::{
    AuthorizationRequest, CodeExchange, ExternalClaims, ExternalCreate, ExternalIdentity,
    IdentityUnlink, NewExternalAccount, OidcFlow, OidcProvider, pkce_challenge,
};
pub use org_settings::{OrgSettings, ReadOnlyDefault, ReadOnlySource};
pub use passwords::{InlinePasswords, PasswordWorker};
pub use profile::{
    Avatar, ImageKind, OrgProfile, OrgProfileFields, Profile, ProfileFields, ProfileVisibility,
};
pub use profiles::{Profiles, PublicProfile};
pub use pyn_gitattr::lockable::Disagreement;
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
pub use repositories::{DEFAULT_BRANCH_STALE_DAYS, Repositories, StarState};
pub use rules::{Mode, PathFilter, PathMatch, RuleEntry, Rules};
pub use service::{
    ActivePolicy, BranchStatus, COMPARE_MAX_COMMITS, COMPARE_MAX_PATHS, ChangeEntry, ChangeKind,
    CommitPage, Comparison, DIFF_DEFAULT_CONTEXT, DIFF_MAX_CONTEXT, DIFF_MAX_FILE_BYTES, Deletion,
    Diff, DiffHunk, DiffLine, DiffLineKind, DiffOptions, DiffStatus, FETCH_DEFAULT_LIMIT,
    FETCH_MAX_EXAMINED, FETCH_MAX_HAVES, FETCH_MAX_LIMIT, FetchPage, FetchQuery, FetchedCommit,
    FileDiff, FileEntry, FileLines, LINES_MAX, LockLimit, PathChange, PolicyPreview, PushObjects,
    Pushed, Recorded, RepoService, RepoServiceContext, ServiceConfig, Snapshot, SnapshotEntry,
    TUS_APPEND_LEASE_MINUTES, TUS_MAX_SESSIONS_PER_USER, TUS_SESSION_TTL_HOURS, TusAppended,
    TusError, TusStatus, VERIFY_BUDGET, is_lfs_oid, sweep_tus_sessions,
};
pub use store::{MetadataStore, UploadReservation};
pub use stranded::{ConversionNote, StrandedVersion};
pub use tag::{Tag, validate_tag_name};
pub use tree::{EntryKind, EntryMode, HeadCounts, RepoSummary, TREE_MAX_PATHS, TreeEntry};
pub use two_factor::{
    ChallengePurpose, ChallengeRecord, FactorKind, FactorRecord, FactorRemoval, MAX_GRACE_DAYS,
    MAX_PASSKEYS, NOTICE_PASSKEY_CLONE_SUSPECTED, NOTICE_TWO_FACTOR_RESET, NoticeRecord,
    PasskeyRecord, RequiredBy, Requirement, TwoFactorPolicy, rotate_seed_key,
};
pub use types::{
    AccountId, ChangesetItem, Commit, CommitFilter, LfsObject, Lock, MAX_CHANGESET_ITEMS,
    MAX_COMMIT_CHANGES, MAX_LABEL_BYTES, MAX_MESSAGE_BYTES, MAX_PATH_BYTES,
    MAX_PATH_COMPONENT_BYTES, MAX_PUSH_CHANGES, MAX_PUSH_COMMITS, MAX_TREE_ENTRIES, NewRevision,
    Person, PushedCommit, RepoId, RepoPath, Revision, RevisionHash, RevisionId, TeamId, TusCancel,
    TusClaim, TusSession, UserId,
};
pub use watch::{Notification, WatchLevel, WatchState};
pub use webhook::{
    Delivery, DeliveryQuery, DeliveryStatus, DueDelivery, EventDraft, FixedJitter, Jitter,
    MAX_ACTIVE_SECRETS, MAX_ATTEMPTS, NewSecret, NewSubscription, OutboxEvent, RETENTION_DAYS,
    RandomJitter, Subscription, SubscriptionUpdate, WebhookSecret, WebhookStore, retry_delay,
};
pub use webhook_secret::{
    SECRET_PREFIX, SecretKey, generate_secret, reveal_secret, rotate_secret_key,
};
