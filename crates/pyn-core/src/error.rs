use chrono::{DateTime, Utc};

use crate::access::{AccountStatus, Permission, ServiceScope};
use crate::types::{RepoPath, RevisionId, UserId};

pub type Result<T, E = PynError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum PynError {
    #[error("invalid path: {0:?}")]
    InvalidPath(String),
    #[error("invalid rules: {0}")]
    InvalidRules(String),
    #[error(
        "{path} is locked by {owner} until {expires_at}. Ask {owner} to release it, or wait for the lease to end."
    )]
    LockHeld {
        path: RepoPath,
        owner: UserId,
        expires_at: DateTime<Utc>,
    },
    #[error(
        "lock limit reached: you already hold {limit} lock(s) in this repository; release one before checking out another"
    )]
    LockLimitReached { limit: u32 },
    #[error("{0} is not locked by you")]
    NotLockHolder(RepoPath),
    #[error("{0} is exclusive: check it out before checking in")]
    LockRequired(RepoPath),
    #[error("{0} is not an exclusive path; there is nothing to check out")]
    NotExclusive(RepoPath),
    #[error("stale base for {path}: you have {expected:?}, head is {actual:?}")]
    StaleBase {
        path: RepoPath,
        expected: Option<RevisionId>,
        actual: Option<RevisionId>,
    },
    #[error("content {0} is not in the object store")]
    ObjectMissing(String),
    #[error("{0} is not locked")]
    NotLocked(RepoPath),
    #[error("{path} has no revision {revision}")]
    RevisionNotFound { path: RepoPath, revision: String },
    #[error("restoring replaces the head; to confirm, pass {expected}")]
    ConfirmationRequired { expected: String },
    #[error("this server has not been set up yet; complete the first-run setup")]
    NotInitialised,
    #[error("this server has already been set up")]
    AlreadyInitialised,
    #[error("the setup token is not correct")]
    InvalidSetupToken,
    #[error("registration is closed on this server")]
    RegistrationClosed,
    #[error("{0} already exists")]
    UserExists(UserId),
    #[error("invalid invitation: {0}")]
    InvalidInvite(String),
    #[error("too many attempts; try again in {retry_after_secs} seconds")]
    TooManyAttempts { retry_after_secs: i64 },
    #[error("{}", inactive_message(*.0))]
    AccountInactive(AccountStatus),
    #[error("invalid verification: {0}")]
    InvalidVerification(String),
    #[error("no account {0}")]
    UserNotFound(String),
    #[error("only a server administrator can do that")]
    ServerAdminRequired,
    #[error("the server must keep at least one active administrator")]
    LastServerAdmin,
    #[error("the service credential needs the {0} scope")]
    ServiceScopeRequired(ServiceScope),
    #[error("a service credential cannot be used for this")]
    ServiceCredentialNotAllowed,
    #[error("no service credential {0}")]
    ServiceCredentialNotFound(String),
    #[error("there is already a service credential named {0}")]
    ServiceCredentialExists(String),
    #[error("that key is already linked to an account")]
    KeyInUse,
    #[error("no key {0}")]
    KeyNotFound(String),
    #[error("authentication failed: {0}")]
    Unauthenticated(String),
    #[error("the request is missing a valid CSRF token")]
    CsrfFailed,
    #[error("permission {0} is required")]
    Forbidden(Permission),
    #[error("no token {0}")]
    TokenNotFound(String),
    #[error("{0}")]
    InvalidRequest(String),
    #[error("no file or folder {0}")]
    PathNotFound(String),
    #[error("{0} already exists")]
    RepoExists(String),
    #[error("no repository {0}")]
    RepoNotFound(String),
    #[error(
        "invalid repository name {0:?}: use 1 to 100 lowercase letters, digits, '-', '_' or '.', starting with a letter or digit"
    )]
    InvalidRepoName(String),
    #[error("only {0} can do that in their namespace")]
    NotNamespaceOwner(String),
    #[error("{0:?} is reserved and cannot be used as a name")]
    ReservedName(String),
    #[error("no organization {0}")]
    OrgNotFound(String),
    #[error("only an owner of {0} can do that")]
    NotOrgOwner(String),
    #[error("{0} still owns repositories; delete or transfer them first")]
    OrgNotEmpty(String),
    #[error("{0} is being deleted")]
    OrgDeleting(String),
    #[error("you are not a member of {0}")]
    NotOrgMember(String),
    #[error("{user} is not a member of {org}; add them to the organization first")]
    UserNotOrgMember { org: String, user: String },
    #[error("{user} is already a member of {org}")]
    AlreadyOrgMember { org: String, user: String },
    #[error("{user} is not a member of {org}")]
    OrgMemberNotFound { org: String, user: String },
    #[error("{0} must keep at least one owner")]
    LastOrgOwner(String),
    #[error("{org} has no team {team}")]
    TeamNotFound { org: String, team: String },
    #[error("{org} already has a team {team}")]
    TeamExists { org: String, team: String },
    #[error("{user} is not in team {team}")]
    TeamMemberNotFound { team: String, user: String },
    #[error("{0} is visible to you, but you hold no role in it")]
    NotRepoMember(String),
    #[error("{org} does not let you create {scope} repositories")]
    RepoCreateForbidden { org: String, scope: String },
    #[error("{org} has no repository-creation rule for {rule}")]
    CreationRuleNotFound { org: String, rule: String },
    #[error("{0} is not owned by an organization, so it has no teams")]
    NotOrgRepo(String),
    #[error("storage error: {0}")]
    Storage(String),
}

fn inactive_message(status: AccountStatus) -> &'static str {
    match status {
        AccountStatus::PendingVerification => {
            "this account's email address is not verified yet; follow the link in the verification email"
        }
        AccountStatus::PendingApproval => {
            "this account is waiting for an administrator to approve it"
        }
        AccountStatus::Disabled => "this account is disabled",
        AccountStatus::Active => "this account is active",
    }
}

impl PynError {
    /// Stable machine-readable code sent to API clients.
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidPath(_) => "invalid_path",
            Self::InvalidRules(_) => "invalid_rules",
            Self::LockHeld { .. } => "lock_held",
            Self::LockLimitReached { .. } => "lock_limit_reached",
            Self::NotLockHolder(_) => "not_lock_holder",
            Self::LockRequired(_) => "lock_required",
            Self::NotExclusive(_) => "not_exclusive",
            Self::StaleBase { .. } => "stale_base",
            Self::ObjectMissing(_) => "object_missing",
            Self::RevisionNotFound { .. } => "revision_not_found",
            Self::ConfirmationRequired { .. } => "confirmation_required",
            Self::NotInitialised => "not_initialised",
            Self::AlreadyInitialised => "already_initialised",
            Self::InvalidSetupToken => "invalid_setup_token",
            Self::RegistrationClosed => "registration_closed",
            Self::UserExists(_) => "user_exists",
            Self::InvalidInvite(_) => "invalid_invite",
            Self::TooManyAttempts { .. } => "too_many_attempts",
            Self::AccountInactive(status) => match status {
                AccountStatus::PendingVerification => "email_not_verified",
                AccountStatus::PendingApproval => "approval_pending",
                AccountStatus::Disabled => "account_disabled",
                AccountStatus::Active => "account_active",
            },
            Self::InvalidVerification(_) => "invalid_verification",
            Self::UserNotFound(_) => "user_not_found",
            Self::ServerAdminRequired => "server_admin_required",
            Self::LastServerAdmin => "last_server_admin",
            Self::ServiceScopeRequired(_) => "service_scope_required",
            Self::ServiceCredentialNotAllowed => "service_credential_not_allowed",
            Self::ServiceCredentialNotFound(_) => "service_credential_not_found",
            Self::ServiceCredentialExists(_) => "service_credential_exists",
            Self::KeyInUse => "key_in_use",
            Self::KeyNotFound(_) => "key_not_found",
            Self::Unauthenticated(_) => "unauthenticated",
            Self::CsrfFailed => "csrf_failed",
            Self::Forbidden(_) => "forbidden",
            Self::TokenNotFound(_) => "token_not_found",
            Self::InvalidRequest(_) => "invalid_request",
            Self::NotLocked(_) => "not_locked",
            Self::PathNotFound(_) => "path_not_found",
            Self::RepoExists(_) => "repo_exists",
            Self::RepoNotFound(_) => "repo_not_found",
            Self::InvalidRepoName(_) => "invalid_repo_name",
            Self::NotNamespaceOwner(_) => "not_namespace_owner",
            Self::ReservedName(_) => "reserved_name",
            Self::OrgNotFound(_) => "org_not_found",
            Self::NotOrgOwner(_) => "not_org_owner",
            Self::OrgNotEmpty(_) => "org_not_empty",
            Self::OrgDeleting(_) => "org_deleting",
            Self::NotOrgMember(_) => "not_org_member",
            Self::UserNotOrgMember { .. } => "user_not_org_member",
            Self::AlreadyOrgMember { .. } => "already_org_member",
            Self::OrgMemberNotFound { .. } => "org_member_not_found",
            Self::LastOrgOwner(_) => "last_org_owner",
            Self::TeamNotFound { .. } => "team_not_found",
            Self::TeamExists { .. } => "team_exists",
            Self::TeamMemberNotFound { .. } => "team_member_not_found",
            Self::NotRepoMember(_) => "not_repo_member",
            Self::RepoCreateForbidden { .. } => "repo_create_forbidden",
            Self::CreationRuleNotFound { .. } => "creation_rule_not_found",
            Self::NotOrgRepo(_) => "not_org_repo",
            Self::Storage(_) => "storage",
        }
    }
}
