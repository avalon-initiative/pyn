use std::fmt;
use std::str::FromStr;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::access::{Identity, Principal};
use crate::access_service::SERVER_AUDIT_ID;
use crate::error::{PynError, Result};
use crate::types::{AccountId, RepoId, RepoPath, UserId};

/// The most message bytes an audit detail carries.
pub const AUDIT_MESSAGE_BYTES: usize = 256;

/// `text` cut to `AUDIT_MESSAGE_BYTES` on a character boundary, marked when cut.
pub fn brief(text: &str) -> String {
    if text.len() <= AUDIT_MESSAGE_BYTES {
        return text.to_string();
    }
    let mut end = AUDIT_MESSAGE_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &text[..end])
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditAction {
    Checkout,
    Release,
    Checkin,
    Restore,
    ForceUnlock,
    MemberAdded,
    RoleChanged,
    RolePermissionsChanged,
    TokenCreated,
    TokenRevoked,
    RepoCreated,
    RepoUpdated,
    RepoDeleted,
    PolicyChanged,
    ModeConverted,
    AccountApproved,
    AccountDisabled,
    AccountEnabled,
    AccountDeletionScheduled,
    AccountDeletionRestored,
    AccountDeleted,
    OrgCreated,
    OrgDeleted,
    OrgMemberAdded,
    OrgMemberRemoved,
    OrgMemberRoleChanged,
    OrgProfileUpdated,
    OrgAvatarSet,
    OrgAvatarRemoved,
    OrgMemberInvited,
    OrgInvitationAccepted,
    OrgInvitationDeclined,
    OrgInvitationCancelled,
    OrgInvitationExpired,
    TeamCreated,
    TeamUpdated,
    TeamDeleted,
    TeamMemberAdded,
    TeamMemberRemoved,
    TeamAccessSet,
    TeamAccessRemoved,
    RepoCreationPolicyChanged,
    RepoCreationRuleSet,
    RepoCreationRuleRemoved,
    OrgSettingChanged,
    ServiceCredentialCreated,
    ServiceCredentialRevoked,
    ServiceCredentialRefused,
    ServerSetupCompleted,
    ServerAccessModeChanged,
    AdminGranted,
    AdminRevoked,
    AccountRenamed,
    EmailAdded,
    EmailVerified,
    EmailPrimaryChanged,
    EmailRemoved,
    TwoFactorAdded,
    TwoFactorRemoved,
    RecoveryCodesRegenerated,
    RecoveryCodeUsed,
    TwoFactorReset,
    PasskeyAdded,
    PasskeyRemoved,
    PasskeyCloneSuspected,
    ServerTwoFactorRequirementChanged,
    OrgTwoFactorRequirementChanged,
    OwnerLimitsChanged,
    RepoStorageCapChanged,
    OwnerFrozen,
    OwnerUnfrozen,
    ExternalAccountCreated,
    ExternalIdentityLinked,
    ExternalIdentityUnlinked,
    InviteCreated,
    InviteRevoked,
    BranchCreated,
    BranchDeleted,
    BranchRenamed,
    BranchMerged,
    VersionStranded,
    WebhookCreated,
    WebhookUpdated,
    WebhookDeleted,
    WebhookSecretRotated,
    WebhookSecretRetired,
    WebhookRedelivered,
    AccessSessionStarted,
    AccessSessionReached,
    AccessSessionEnded,
    AccountAddedByAdmin,
    AccountConfirmationResent,
    ServerInvitationCreated,
    ServerInvitationRevoked,
    ServerInvitationUsed,
    Delete,
}

impl AuditAction {
    pub const ALL: [AuditAction; 96] = [
        AuditAction::Checkout,
        AuditAction::Release,
        AuditAction::Checkin,
        AuditAction::Restore,
        AuditAction::ForceUnlock,
        AuditAction::MemberAdded,
        AuditAction::RoleChanged,
        AuditAction::RolePermissionsChanged,
        AuditAction::TokenCreated,
        AuditAction::TokenRevoked,
        AuditAction::RepoCreated,
        AuditAction::RepoUpdated,
        AuditAction::RepoDeleted,
        AuditAction::PolicyChanged,
        AuditAction::ModeConverted,
        AuditAction::AccountApproved,
        AuditAction::AccountDisabled,
        AuditAction::AccountEnabled,
        AuditAction::AccountDeletionScheduled,
        AuditAction::AccountDeletionRestored,
        AuditAction::AccountDeleted,
        AuditAction::OrgCreated,
        AuditAction::OrgDeleted,
        AuditAction::OrgMemberAdded,
        AuditAction::OrgMemberRemoved,
        AuditAction::OrgMemberRoleChanged,
        AuditAction::OrgProfileUpdated,
        AuditAction::OrgAvatarSet,
        AuditAction::OrgAvatarRemoved,
        AuditAction::OrgMemberInvited,
        AuditAction::OrgInvitationAccepted,
        AuditAction::OrgInvitationDeclined,
        AuditAction::OrgInvitationCancelled,
        AuditAction::OrgInvitationExpired,
        AuditAction::TeamCreated,
        AuditAction::TeamUpdated,
        AuditAction::TeamDeleted,
        AuditAction::TeamMemberAdded,
        AuditAction::TeamMemberRemoved,
        AuditAction::TeamAccessSet,
        AuditAction::TeamAccessRemoved,
        AuditAction::RepoCreationPolicyChanged,
        AuditAction::RepoCreationRuleSet,
        AuditAction::RepoCreationRuleRemoved,
        AuditAction::OrgSettingChanged,
        AuditAction::ServiceCredentialCreated,
        AuditAction::ServiceCredentialRevoked,
        AuditAction::ServiceCredentialRefused,
        AuditAction::ServerSetupCompleted,
        AuditAction::ServerAccessModeChanged,
        AuditAction::AdminGranted,
        AuditAction::AdminRevoked,
        AuditAction::AccountRenamed,
        AuditAction::EmailAdded,
        AuditAction::EmailVerified,
        AuditAction::EmailPrimaryChanged,
        AuditAction::EmailRemoved,
        AuditAction::TwoFactorAdded,
        AuditAction::TwoFactorRemoved,
        AuditAction::RecoveryCodesRegenerated,
        AuditAction::RecoveryCodeUsed,
        AuditAction::TwoFactorReset,
        AuditAction::PasskeyAdded,
        AuditAction::PasskeyRemoved,
        AuditAction::PasskeyCloneSuspected,
        AuditAction::ServerTwoFactorRequirementChanged,
        AuditAction::OrgTwoFactorRequirementChanged,
        AuditAction::OwnerLimitsChanged,
        AuditAction::RepoStorageCapChanged,
        AuditAction::OwnerFrozen,
        AuditAction::OwnerUnfrozen,
        AuditAction::ExternalAccountCreated,
        AuditAction::ExternalIdentityLinked,
        AuditAction::ExternalIdentityUnlinked,
        AuditAction::InviteCreated,
        AuditAction::InviteRevoked,
        AuditAction::BranchCreated,
        AuditAction::BranchDeleted,
        AuditAction::BranchRenamed,
        AuditAction::BranchMerged,
        AuditAction::VersionStranded,
        AuditAction::WebhookCreated,
        AuditAction::WebhookUpdated,
        AuditAction::WebhookDeleted,
        AuditAction::WebhookSecretRotated,
        AuditAction::WebhookSecretRetired,
        AuditAction::WebhookRedelivered,
        AuditAction::AccessSessionStarted,
        AuditAction::AccessSessionReached,
        AuditAction::AccessSessionEnded,
        AuditAction::AccountAddedByAdmin,
        AuditAction::AccountConfirmationResent,
        AuditAction::ServerInvitationCreated,
        AuditAction::ServerInvitationRevoked,
        AuditAction::ServerInvitationUsed,
        AuditAction::Delete,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Checkout => "checkout",
            Self::Release => "release",
            Self::Checkin => "checkin",
            Self::Restore => "restore",
            Self::ForceUnlock => "force_unlock",
            Self::MemberAdded => "member_added",
            Self::RoleChanged => "role_changed",
            Self::RolePermissionsChanged => "role_permissions_changed",
            Self::TokenCreated => "token_created",
            Self::TokenRevoked => "token_revoked",
            Self::RepoCreated => "repo_created",
            Self::RepoUpdated => "repo_updated",
            Self::RepoDeleted => "repo_deleted",
            Self::PolicyChanged => "policy_changed",
            Self::ModeConverted => "mode_converted",
            Self::AccountApproved => "account_approved",
            Self::AccountDisabled => "account_disabled",
            Self::AccountEnabled => "account_enabled",
            Self::AccountDeletionScheduled => "account_deletion_scheduled",
            Self::AccountDeletionRestored => "account_deletion_restored",
            Self::AccountDeleted => "account_deleted",
            Self::OrgCreated => "org_created",
            Self::OrgDeleted => "org_deleted",
            Self::OrgMemberAdded => "org_member_added",
            Self::OrgMemberRemoved => "org_member_removed",
            Self::OrgMemberRoleChanged => "org_member_role_changed",
            Self::OrgProfileUpdated => "org_profile_updated",
            Self::OrgAvatarSet => "org_avatar_set",
            Self::OrgAvatarRemoved => "org_avatar_removed",
            Self::OrgMemberInvited => "org_member_invited",
            Self::OrgInvitationAccepted => "org_invitation_accepted",
            Self::OrgInvitationDeclined => "org_invitation_declined",
            Self::OrgInvitationCancelled => "org_invitation_cancelled",
            Self::OrgInvitationExpired => "org_invitation_expired",
            Self::TeamCreated => "team_created",
            Self::TeamUpdated => "team_updated",
            Self::TeamDeleted => "team_deleted",
            Self::TeamMemberAdded => "team_member_added",
            Self::TeamMemberRemoved => "team_member_removed",
            Self::TeamAccessSet => "team_access_set",
            Self::TeamAccessRemoved => "team_access_removed",
            Self::RepoCreationPolicyChanged => "repo_creation_policy_changed",
            Self::RepoCreationRuleSet => "repo_creation_rule_set",
            Self::RepoCreationRuleRemoved => "repo_creation_rule_removed",
            Self::OrgSettingChanged => "org_setting_changed",
            Self::ServiceCredentialCreated => "service_credential_created",
            Self::ServiceCredentialRevoked => "service_credential_revoked",
            Self::ServiceCredentialRefused => "service_credential_refused",
            Self::ServerSetupCompleted => "server_setup_completed",
            Self::ServerAccessModeChanged => "server_access_mode_changed",
            Self::AdminGranted => "admin_granted",
            Self::AdminRevoked => "admin_revoked",
            Self::AccountRenamed => "account_renamed",
            Self::EmailAdded => "email_added",
            Self::EmailVerified => "email_verified",
            Self::EmailPrimaryChanged => "email_primary_changed",
            Self::EmailRemoved => "email_removed",
            Self::TwoFactorAdded => "two_factor_added",
            Self::TwoFactorRemoved => "two_factor_removed",
            Self::RecoveryCodesRegenerated => "recovery_codes_regenerated",
            Self::RecoveryCodeUsed => "recovery_code_used",
            Self::TwoFactorReset => "two_factor_reset",
            Self::PasskeyAdded => "passkey_added",
            Self::PasskeyRemoved => "passkey_removed",
            Self::PasskeyCloneSuspected => "passkey_clone_suspected",
            Self::ServerTwoFactorRequirementChanged => "server_two_factor_requirement_changed",
            Self::OrgTwoFactorRequirementChanged => "org_two_factor_requirement_changed",
            Self::OwnerLimitsChanged => "owner_limits_changed",
            Self::RepoStorageCapChanged => "repo_storage_cap_changed",
            Self::OwnerFrozen => "owner_frozen",
            Self::OwnerUnfrozen => "owner_unfrozen",
            Self::ExternalAccountCreated => "external_account_created",
            Self::ExternalIdentityLinked => "external_identity_linked",
            Self::ExternalIdentityUnlinked => "external_identity_unlinked",
            Self::InviteCreated => "invite_created",
            Self::InviteRevoked => "invite_revoked",
            Self::BranchCreated => "branch_created",
            Self::BranchDeleted => "branch_deleted",
            Self::BranchRenamed => "branch_renamed",
            Self::BranchMerged => "branch_merged",
            Self::VersionStranded => "version_stranded",
            Self::WebhookCreated => "webhook_created",
            Self::WebhookUpdated => "webhook_updated",
            Self::WebhookDeleted => "webhook_deleted",
            Self::WebhookSecretRotated => "webhook_secret_rotated",
            Self::WebhookSecretRetired => "webhook_secret_retired",
            Self::WebhookRedelivered => "webhook_redelivered",
            Self::AccessSessionStarted => "access_session_started",
            Self::AccessSessionReached => "access_session_reached",
            Self::AccessSessionEnded => "access_session_ended",
            Self::AccountAddedByAdmin => "account_added_by_admin",
            Self::AccountConfirmationResent => "account_confirmation_resent",
            Self::ServerInvitationCreated => "server_invitation_created",
            Self::ServerInvitationRevoked => "server_invitation_revoked",
            Self::ServerInvitationUsed => "server_invitation_used",
            Self::Delete => "delete",
        }
    }
}

impl fmt::Display for AuditAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for AuditAction {
    type Err = PynError;
    fn from_str(s: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|a| a.as_str() == s)
            .ok_or_else(|| PynError::InvalidRequest(format!("unknown audit action {s:?}")))
    }
}

/// Who an audited action is attributed to: the account, and the administrator acting as it in an access session.
/// Every audit helper takes this, so a caller that holds an identity cannot record the account alone by omission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Actor<'a> {
    pub user: &'a UserId,
    pub acting_admin: Option<&'a UserId>,
}

/// An actor outside any access session.
impl<'a> From<&'a UserId> for Actor<'a> {
    fn from(user: &'a UserId) -> Self {
        Self {
            user,
            acting_admin: None,
        }
    }
}

impl<'a> From<&'a Identity> for Actor<'a> {
    fn from(who: &'a Identity) -> Self {
        Self {
            user: &who.user,
            acting_admin: who.acting_admin(),
        }
    }
}

impl<'a> From<&'a Principal> for Actor<'a> {
    fn from(who: &'a Principal) -> Self {
        Self {
            user: &who.user,
            acting_admin: who.acting_admin.as_ref(),
        }
    }
}

impl<'a> From<&'a Actor<'a>> for Actor<'a> {
    fn from(who: &'a Actor<'a>) -> Self {
        *who
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEvent {
    pub id: i64,
    pub at: DateTime<Utc>,
    pub actor: UserId,
    /// The administrator who acted as `actor` in an access session; covered by the hash.
    pub acting_admin: Option<UserId>,
    pub action: AuditAction,
    pub path: Option<RepoPath>,
    pub detail: String,
    /// Hash of the preceding event in the same log; `None` for the first.
    pub previous: Option<String>,
    pub hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAuditEvent {
    pub at: DateTime<Utc>,
    pub actor: UserId,
    pub acting_admin: Option<UserId>,
    pub action: AuditAction,
    pub path: Option<RepoPath>,
    pub detail: String,
}

/// Which events to list: all matching filters, newest first, starting below `before`.
#[derive(Debug, Clone, Default)]
pub struct AuditQuery {
    pub path: Option<RepoPath>,
    pub actor: Option<UserId>,
    pub action: Option<AuditAction>,
    /// Any of these actions; no restriction when empty.
    pub actions: Vec<AuditAction>,
    pub before: Option<i64>,
    pub limit: usize,
}

/// What a log belongs to: a repository, an organization, or the server itself.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AuditScope {
    Repo(RepoId),
    Org(UserId),
    /// An account's security log, keyed by its stable id so a rename keeps it.
    Account(AccountId),
    Server,
}

impl AuditScope {
    /// The stable storage key. Neither `@` form can collide with a repository id or a user name.
    pub fn key(&self) -> String {
        match self {
            Self::Repo(id) => id.to_string(),
            Self::Org(name) => format!("@org:{name}"),
            Self::Account(id) => format!("@account:{id}"),
            Self::Server => SERVER_AUDIT_ID.to_string(),
        }
    }
}

impl AuditScope {
    /// The scope a storage key from `key` names.
    pub fn from_key(key: &str) -> Self {
        if let Some(id) = key.strip_prefix("@account:") {
            return Self::Account(AccountId::new(id));
        }
        match key.strip_prefix("@org:") {
            Some(name) => Self::Org(UserId::new(name)),
            None if key == SERVER_AUDIT_ID => Self::Server,
            None => Self::Repo(RepoId::new(key)),
        }
    }
}

impl From<&RepoId> for AuditScope {
    fn from(id: &RepoId) -> Self {
        Self::Repo(id.clone())
    }
}

/// Append-only record of administrative and locking events, hash-chained per scope. Events are never changed or removed.
#[async_trait]
pub trait AuditStore: Send + Sync {
    /// Appends the event, chained to the scope's newest one in the same atomic step.
    async fn record(&self, scope: &AuditScope, event: NewAuditEvent) -> Result<()>;

    async fn list(&self, scope: &AuditScope, query: &AuditQuery) -> Result<Vec<AuditEvent>>;

    /// Whether any log holds an event whose actor is `actor`; the comparison ignores case.
    async fn actor_appears(&self, actor: &UserId) -> Result<bool>;
}
