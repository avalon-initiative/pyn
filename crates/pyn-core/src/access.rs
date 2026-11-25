use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::access_session::AccessSessionRecord;
use crate::error::{PynError, Result};
use crate::types::{AccountId, RepoId, TeamId, UserId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    Read,
    Lock,
    Checkin,
    Restore,
    ForceUnlock,
    EditPolicy,
    ManageUsers,
    ManageRoles,
    ViewAudit,
}

impl Permission {
    pub const ALL: [Permission; 9] = [
        Permission::Read,
        Permission::Lock,
        Permission::Checkin,
        Permission::Restore,
        Permission::ForceUnlock,
        Permission::EditPolicy,
        Permission::ManageUsers,
        Permission::ManageRoles,
        Permission::ViewAudit,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Lock => "lock",
            Self::Checkin => "checkin",
            Self::Restore => "restore",
            Self::ForceUnlock => "force_unlock",
            Self::EditPolicy => "edit_policy",
            Self::ManageUsers => "manage_users",
            Self::ManageRoles => "manage_roles",
            Self::ViewAudit => "view_audit",
        }
    }
}

impl fmt::Display for Permission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Permission {
    type Err = PynError;
    fn from_str(s: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|p| p.as_str() == s)
            .ok_or_else(|| PynError::InvalidRequest(format!("unknown permission {s:?}")))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Reader,
    Writer,
    Maintainer,
    Admin,
}

impl Role {
    pub const ALL: [Role; 4] = [Role::Reader, Role::Writer, Role::Maintainer, Role::Admin];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reader => "reader",
            Self::Writer => "writer",
            Self::Maintainer => "maintainer",
            Self::Admin => "admin",
        }
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Role {
    type Err = PynError;
    fn from_str(s: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|r| r.as_str() == s)
            .ok_or_else(|| PynError::InvalidRequest(format!("unknown role {s:?}")))
    }
}

/// What each role grants. Starts from the defaults; a repository owner can override any role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleDefinitions(BTreeMap<Role, BTreeSet<Permission>>);

impl RoleDefinitions {
    pub fn defaults() -> Self {
        use Permission::*;
        let reader: BTreeSet<_> = [Read].into();
        let writer: BTreeSet<_> = [Read, Lock, Checkin].into();
        let maintainer: BTreeSet<_> = [
            Read,
            Lock,
            Checkin,
            Restore,
            ForceUnlock,
            EditPolicy,
            ViewAudit,
        ]
        .into();
        let admin: BTreeSet<_> = Permission::ALL.into();
        Self(BTreeMap::from([
            (Role::Reader, reader),
            (Role::Writer, writer),
            (Role::Maintainer, maintainer),
            (Role::Admin, admin),
        ]))
    }

    pub fn get(&self, role: Role) -> &BTreeSet<Permission> {
        &self.0[&role]
    }

    pub fn set(&mut self, role: Role, permissions: BTreeSet<Permission>) {
        self.0.insert(role, permissions);
    }
}

/// How a request proved who it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Credential {
    /// A bearer token: it limits what its owner's role grants, and to which repositories.
    Token(TokenRecord),
    /// A web session: the owner's roles apply as they are.
    Session,
    /// A trusted external service: administrative scopes only, no account and no repository roles.
    Service(ServiceCredentialRecord),
    /// An administrator acting as the account for an hour (decision #1024): the account's roles apply to repository
    /// work, and everything that manages the account itself, or access to it, is refused.
    AccessSession(AccessSessionRecord),
    /// Development identity with every permission everywhere.
    Unrestricted,
}

/// An authenticated account, before any repository is chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub user: UserId,
    pub credential: Credential,
}

impl Identity {
    /// The administrator behind an access-session credential; `None` for every other credential.
    pub fn acting_admin(&self) -> Option<&UserId> {
        match &self.credential {
            Credential::AccessSession(record) => Some(&record.admin),
            _ => None,
        }
    }

    /// A session, an unrestricted credential, or a sign-in token. A fine-grained token never is, whatever it holds.
    pub fn is_full_scope(&self) -> bool {
        match &self.credential {
            Credential::Session | Credential::Unrestricted => true,
            Credential::Service(_) | Credential::AccessSession(_) => false,
            Credential::Token(t) => t.sign_in,
        }
    }

    /// Whether the credential may act in `repo`, owned by `owner`: a token only where its owner and
    /// repository access reach, a service in none.
    pub fn covers_repo(&self, repo: &RepoId, owner: &UserId) -> bool {
        match &self.credential {
            Credential::Session | Credential::Unrestricted | Credential::AccessSession(_) => true,
            Credential::Service(_) => false,
            Credential::Token(t) => t.covers(repo, owner),
        }
    }

    /// A token must hold the grant; a session, the development identity and a sign-in token do not narrow by grants.
    pub fn require_grant(&self, need: GrantNeed) -> Result<()> {
        match &self.credential {
            Credential::Token(t) if !t.sign_in => match t.grants.get(&need.resource) {
                Some(level) if *level >= need.level => Ok(()),
                _ => Err(PynError::GrantRequired(need)),
            },
            Credential::AccessSession(_)
                if need.level >= GrantLevel::Write && need.resource.shapes_access() =>
            {
                Err(PynError::AccessSessionNotAllowed)
            }
            _ => Ok(()),
        }
    }

    /// An organization route: a full-scope credential, or a token owned by `org` that holds the need. The creator's
    /// organization role still caps what the token does there.
    pub fn require_org_grant(&self, org: &UserId, need: GrantNeed) -> Result<()> {
        self.require_owner_grant(org, need)
    }

    /// An account route: a full-scope credential, or a token owned by the account itself that holds the need.
    pub fn require_account_grant(&self, need: GrantNeed) -> Result<()> {
        self.require_owner_grant(&self.user, need)
    }

    fn require_owner_grant(&self, owner: &UserId, need: GrantNeed) -> Result<()> {
        match &self.credential {
            Credential::Token(t)
                if !t.sign_in && (&t.owner != owner || self.require_grant(need).is_err()) =>
            {
                self.require_full_scope()
            }
            Credential::Service(_) | Credential::AccessSession(_) => self.require_full_scope(),
            _ => Ok(()),
        }
    }

    /// Whether a token owned by the account holds the account grant `need`.
    pub fn holds_account_grant(&self, need: GrantNeed) -> bool {
        matches!(&self.credential, Credential::Token(t) if !t.sign_in && t.owner == self.user)
            && self.require_grant(need).is_ok()
    }

    /// Account-management operations need a full-scope credential.
    pub fn require_full_scope(&self) -> Result<()> {
        if self.is_full_scope() {
            return Ok(());
        }
        match &self.credential {
            Credential::Service(_) => Err(PynError::ServiceCredentialNotAllowed),
            Credential::AccessSession(_) => Err(PynError::AccessSessionNotAllowed),
            Credential::Token(t) => {
                let held = t.permissions();
                Err(PynError::Forbidden(
                    Permission::ALL
                        .into_iter()
                        .find(|p| !held.contains(p))
                        .unwrap_or(Permission::ManageRoles),
                ))
            }
            _ => Ok(()),
        }
    }

    /// A token must be a sign-in token, or its own account's token for every repository with `manage_roles`,
    /// to create or delete repositories and organizations.
    pub fn require_namespace_management(&self) -> Result<()> {
        match &self.credential {
            Credential::Token(t)
                if !t.sign_in
                    && (t.repo_access != RepoAccess::All
                        || t.owner != self.user
                        || !t.permissions().contains(&Permission::ManageRoles)) =>
            {
                Err(PynError::Forbidden(Permission::ManageRoles))
            }
            Credential::Service(_) => Err(PynError::ServiceCredentialNotAllowed),
            Credential::AccessSession(_) => Err(PynError::AccessSessionNotAllowed),
            _ => Ok(()),
        }
    }
}

/// An authenticated caller and what they may do in one repository right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    pub user: UserId,
    pub permissions: BTreeSet<Permission>,
    /// The account's email, recorded as the author's in the commits the caller makes.
    pub email: Option<String>,
    /// The administrator acting as `user` in an access session; audit and webhook events name it beside `user`.
    pub acting_admin: Option<UserId>,
}

impl Principal {
    pub fn has(&self, permission: Permission) -> bool {
        self.permissions.contains(&permission)
    }

    pub fn require(&self, permission: Permission) -> Result<()> {
        if self.has(permission) {
            Ok(())
        } else {
            Err(PynError::Forbidden(permission))
        }
    }

    /// Whether the principal holds the permission the need maps to, or the need maps to none.
    pub fn require_need(&self, need: GrantNeed) -> Result<()> {
        need.permission().map_or(Ok(()), |p| self.require(p))
    }

    /// Every permission, for the development identity.
    pub fn unrestricted(user: UserId) -> Self {
        Self {
            user,
            permissions: Permission::ALL.into(),
            email: None,
            acting_admin: None,
        }
    }

    /// The git identity commits made by this caller carry.
    pub fn person(&self) -> crate::types::Person {
        crate::types::Person::for_user(&self.user, self.email.as_deref())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TokenId(pub String);

impl fmt::Display for TokenId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A stored token. Only a hash of the secret is kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenRecord {
    pub id: TokenId,
    pub user: UserId,
    pub name: String,
    pub description: Option<String>,
    pub secret_hash: String,
    /// The account the token acts for: the creator's own, or one organization they may create tokens for.
    pub owner: UserId,
    pub repo_access: RepoAccess,
    /// Used only with `RepoAccess::Selected`.
    pub repos: Vec<RepoId>,
    /// The source of truth for what the token may do; `permissions()` derives the internal set.
    pub grants: BTreeMap<TokenResource, GrantLevel>,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub last_used_at: Option<DateTime<Utc>>,
    /// Issued by a password sign-in rather than created on request.
    pub sign_in: bool,
}

impl TokenRecord {
    pub fn permissions(&self) -> BTreeSet<Permission> {
        permissions_of(&self.grants)
    }

    /// Whether the token may act in `repo`, owned by `repo_owner`. Sign-in tokens reach every repository.
    pub fn covers(&self, repo: &RepoId, repo_owner: &UserId) -> bool {
        if self.sign_in {
            return true;
        }
        if &self.owner != repo_owner {
            return false;
        }
        match self.repo_access {
            RepoAccess::Public => false,
            RepoAccess::All => true,
            RepoAccess::Selected => self.repos.contains(repo),
        }
    }

    /// The grants of a sign-in token: the most each resource allows.
    pub fn full_grants() -> BTreeMap<TokenResource, GrantLevel> {
        TokenResource::ALL
            .into_iter()
            .map(|r| (r, r.max_level()))
            .collect()
    }
}

/// What a fine-grained token may touch, per the permission catalog. The organization and account resources carry
/// their category in the wire name so they never clash with the repository ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenResource {
    Metadata,
    Contents,
    Locks,
    Policy,
    Members,
    Administration,
    Audit,
    OrgMembers,
    OrgTeams,
    OrgAdministration,
    OrgAudit,
    AccountProfile,
    AccountSshKeys,
    AccountLocks,
}

/// What a resource's grant applies to: the repositories of the token's owner, the owner organization, or the account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantScope {
    Repository,
    Organization,
    Account,
}

impl TokenResource {
    /// The repository resources; a sign-in token's grants are these at their top level.
    pub const ALL: [TokenResource; 7] = [
        Self::Metadata,
        Self::Contents,
        Self::Locks,
        Self::Policy,
        Self::Members,
        Self::Administration,
        Self::Audit,
    ];

    pub const ORGANIZATION: [TokenResource; 4] = [
        Self::OrgMembers,
        Self::OrgTeams,
        Self::OrgAdministration,
        Self::OrgAudit,
    ];

    pub const ACCOUNT: [TokenResource; 3] = [
        Self::AccountProfile,
        Self::AccountSshKeys,
        Self::AccountLocks,
    ];

    /// Every resource of every category.
    pub fn every() -> impl Iterator<Item = TokenResource> {
        Self::ALL
            .into_iter()
            .chain(Self::ORGANIZATION)
            .chain(Self::ACCOUNT)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Metadata => "metadata",
            Self::Contents => "contents",
            Self::Locks => "locks",
            Self::Policy => "policy",
            Self::Members => "members",
            Self::Administration => "administration",
            Self::Audit => "audit",
            Self::OrgMembers => "org_members",
            Self::OrgTeams => "org_teams",
            Self::OrgAdministration => "org_administration",
            Self::OrgAudit => "org_audit",
            Self::AccountProfile => "account_profile",
            Self::AccountSshKeys => "account_ssh_keys",
            Self::AccountLocks => "account_locks",
        }
    }

    /// Whether writing this resource changes who can reach a repository or whether it exists.
    pub fn shapes_access(self) -> bool {
        matches!(self, Self::Members | Self::Administration)
    }

    pub fn scope(self) -> GrantScope {
        match self {
            Self::OrgMembers | Self::OrgTeams | Self::OrgAdministration | Self::OrgAudit => {
                GrantScope::Organization
            }
            Self::AccountProfile | Self::AccountSshKeys | Self::AccountLocks => GrantScope::Account,
            _ => GrantScope::Repository,
        }
    }

    /// The lowest level the catalog offers: policy and the account profile have no read level.
    pub fn min_level(self) -> GrantLevel {
        match self {
            Self::Policy | Self::AccountProfile => GrantLevel::Write,
            _ => GrantLevel::Read,
        }
    }

    /// The highest level the catalog offers: `admin` for locks, read for the logs and lists that have no write
    /// (metadata, audit, organization audit, account locks), `write` for the rest.
    pub fn max_level(self) -> GrantLevel {
        match self {
            Self::Metadata | Self::Audit | Self::OrgAudit | Self::AccountLocks => GrantLevel::Read,
            Self::Locks => GrantLevel::Admin,
            _ => GrantLevel::Write,
        }
    }

    /// The organization role the operations of an organization grant need today: members and teams may be listed
    /// by any member, everything else (writes, invitations, the audit log, the creation policy) is for owners.
    pub fn org_role_needed(self, level: GrantLevel) -> OrgRole {
        match (self, level) {
            (Self::OrgMembers | Self::OrgTeams, GrantLevel::Read) => OrgRole::Member,
            _ => OrgRole::Owner,
        }
    }
}

impl FromStr for TokenResource {
    type Err = PynError;
    fn from_str(s: &str) -> Result<Self> {
        Self::every()
            .find(|r| r.as_str() == s)
            .ok_or_else(|| PynError::InvalidRequest(format!("unknown token resource {s:?}")))
    }
}

/// A permission level; each includes the ones below it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantLevel {
    Read,
    Write,
    Admin,
}

impl GrantLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Admin => "admin",
        }
    }
}

impl FromStr for GrantLevel {
    type Err = PynError;
    fn from_str(s: &str) -> Result<Self> {
        [Self::Read, Self::Write, Self::Admin]
            .into_iter()
            .find(|l| l.as_str() == s)
            .ok_or_else(|| PynError::InvalidRequest(format!("unknown permission level {s:?}")))
    }
}

/// The resource and level a route needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GrantNeed {
    pub resource: TokenResource,
    pub level: GrantLevel,
    via: Option<Permission>,
}

impl GrantNeed {
    pub const fn new(resource: TokenResource, level: GrantLevel) -> Self {
        Self {
            resource,
            level,
            via: None,
        }
    }

    /// Names the permission a role must hold where the level adds several (contents write: `checkin` or `restore`).
    pub const fn via(mut self, permission: Permission) -> Self {
        self.via = Some(permission);
        self
    }

    /// The `resource:level` spelling published in `x-pyn-reach`.
    pub fn label(self) -> String {
        format!("{}:{}", self.resource.as_str(), self.level.as_str())
    }

    /// The permission a credential without grants must hold: what the level adds over the one below, as
    /// `permissions_of` derives it. `None` where it adds none (metadata, policy read, administration read), so
    /// seeing the repository is enough.
    pub fn permission(self) -> Option<Permission> {
        self.via
            .or_else(|| grant_delta(self.resource, self.level).first().copied())
    }
}

/// The needs of the organization and account operations; routes declare the same ones and core re-checks them.
pub mod needs {
    use super::{GrantLevel::*, GrantNeed, TokenResource::*};

    pub const ORG_MEMBERS_READ: GrantNeed = GrantNeed::new(OrgMembers, Read);
    pub const ORG_MEMBERS_WRITE: GrantNeed = GrantNeed::new(OrgMembers, Write);
    pub const ORG_TEAMS_READ: GrantNeed = GrantNeed::new(OrgTeams, Read);
    pub const ORG_TEAMS_WRITE: GrantNeed = GrantNeed::new(OrgTeams, Write);
    pub const ORG_ADMINISTRATION_READ: GrantNeed = GrantNeed::new(OrgAdministration, Read);
    pub const ORG_ADMINISTRATION_WRITE: GrantNeed = GrantNeed::new(OrgAdministration, Write);
    pub const ORG_AUDIT_READ: GrantNeed = GrantNeed::new(OrgAudit, Read);
    pub const ACCOUNT_PROFILE_WRITE: GrantNeed = GrantNeed::new(AccountProfile, Write);
    pub const ACCOUNT_SSH_KEYS_READ: GrantNeed = GrantNeed::new(AccountSshKeys, Read);
    pub const ACCOUNT_SSH_KEYS_WRITE: GrantNeed = GrantNeed::new(AccountSshKeys, Write);
    pub const ACCOUNT_LOCKS_READ: GrantNeed = GrantNeed::new(AccountLocks, Read);
}

/// Which repositories of its owner a token reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepoAccess {
    /// None beyond the public repositories every token can read.
    Public,
    /// Every repository the owner has now and later.
    All,
    /// The repositories listed on the token.
    Selected,
}

impl RepoAccess {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::All => "all",
            Self::Selected => "selected",
        }
    }
}

impl FromStr for RepoAccess {
    type Err = PynError;
    fn from_str(s: &str) -> Result<Self> {
        [Self::Public, Self::All, Self::Selected]
            .into_iter()
            .find(|a| a.as_str() == s)
            .ok_or_else(|| PynError::InvalidRequest(format!("unknown repository access {s:?}")))
    }
}

/// The internal permissions a set of grants amounts to; the only place grants turn into permissions.
pub fn permissions_of(grants: &BTreeMap<TokenResource, GrantLevel>) -> BTreeSet<Permission> {
    use Permission::*;
    let mut out = BTreeSet::new();
    for (resource, level) in grants {
        let write = *level >= GrantLevel::Write;
        match resource {
            TokenResource::Metadata => {}
            TokenResource::Contents => {
                out.insert(Read);
                if write {
                    out.extend([Checkin, Restore]);
                }
            }
            TokenResource::Locks => {
                out.insert(Read);
                if write {
                    out.insert(Lock);
                }
                if *level >= GrantLevel::Admin {
                    out.insert(ForceUnlock);
                }
            }
            TokenResource::Members => {
                out.insert(Read);
                if write {
                    out.insert(ManageUsers);
                }
            }
            TokenResource::Policy => {
                if write {
                    out.insert(EditPolicy);
                }
            }
            TokenResource::Administration if write => {
                out.insert(ManageRoles);
            }
            TokenResource::Administration => {}
            TokenResource::Audit => {
                out.insert(ViewAudit);
            }
            // Organization and account grants add no repository permission.
            TokenResource::OrgMembers
            | TokenResource::OrgTeams
            | TokenResource::OrgAdministration
            | TokenResource::OrgAudit
            | TokenResource::AccountProfile
            | TokenResource::AccountSshKeys
            | TokenResource::AccountLocks => {}
        }
    }
    out
}

/// What one grant adds over the next lower level of the same resource.
pub fn grant_delta(resource: TokenResource, level: GrantLevel) -> BTreeSet<Permission> {
    let at = |l| permissions_of(&BTreeMap::from([(resource, l)]));
    let lower = match level {
        GrantLevel::Read => BTreeSet::new(),
        GrantLevel::Write => at(GrantLevel::Read),
        GrantLevel::Admin => at(GrantLevel::Write),
    };
    at(level).difference(&lower).copied().collect()
}

/// Token strings look like `pyn_<12 hex id>_<64 hex secret>`.
pub mod token {
    use super::*;

    const PREFIX: &str = "pyn_";

    pub(crate) fn random_hex(bytes: usize) -> Result<String> {
        let mut buf = vec![0u8; bytes];
        getrandom::fill(&mut buf).map_err(|e| PynError::Storage(format!("no randomness: {e}")))?;
        Ok(hex::encode(buf))
    }

    /// A random id, secret and the hash of the secret.
    pub(crate) fn random_parts() -> Result<(String, String, String)> {
        let id = random_hex(6)?;
        let secret = random_hex(32)?;
        let hash = hash_secret(&secret);
        Ok((id, secret, hash))
    }

    pub(crate) fn is_id_and_secret(id: &str, secret: &str) -> bool {
        let hex = |s: &str, len: usize| s.len() == len && s.bytes().all(|b| b.is_ascii_hexdigit());
        hex(id, 12) && hex(secret, 64)
    }

    /// A new id, the full token to hand to the user once, and the hash to store.
    pub fn generate() -> Result<(TokenId, String, String)> {
        let (id, secret, hash) = random_parts()?;
        Ok((TokenId(id.clone()), format!("{PREFIX}{id}_{secret}"), hash))
    }

    pub fn parse(raw: &str) -> Option<(TokenId, &str)> {
        let rest = raw.strip_prefix(PREFIX)?;
        let (id, secret) = rest.split_once('_')?;
        is_id_and_secret(id, secret).then(|| (TokenId(id.to_string()), secret))
    }

    pub fn hash_secret(secret: &str) -> String {
        hex::encode(Sha256::digest(secret.as_bytes()))
    }

    /// Compares without stopping at the first difference.
    pub fn hashes_match(a: &str, b: &str) -> bool {
        a.len() == b.len()
            && a.bytes()
                .zip(b.bytes())
                .fold(0u8, |acc, (x, y)| acc | (x ^ y))
                == 0
    }
}

/// What a service credential may do on the server. Scopes only ever get added.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceScope {
    /// List, approve, disable and enable accounts.
    ManageAccounts,
    /// Create and delete organizations on behalf of an owner.
    ManageOrganizations,
    /// Set per-owner limits and read any owner's usage.
    ManageLimits,
    /// Create, change and delete webhook subscriptions and rotate their secrets.
    ManageWebhooks,
    /// Freeze and unfreeze owners and read any owner's frozen state.
    ManageFreeze,
}

impl ServiceScope {
    pub const ALL: [ServiceScope; 5] = [
        ServiceScope::ManageAccounts,
        ServiceScope::ManageOrganizations,
        ServiceScope::ManageLimits,
        ServiceScope::ManageWebhooks,
        ServiceScope::ManageFreeze,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::ManageAccounts => "manage_accounts",
            Self::ManageOrganizations => "manage_organizations",
            Self::ManageLimits => "manage_limits",
            Self::ManageWebhooks => "manage_webhooks",
            Self::ManageFreeze => "manage_freeze",
        }
    }
}

impl fmt::Display for ServiceScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ServiceScope {
    type Err = PynError;
    fn from_str(s: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|p| p.as_str() == s)
            .ok_or_else(|| PynError::InvalidRequest(format!("unknown service scope {s:?}")))
    }
}

/// A stored service credential. Only a hash of the secret is kept; names are never reused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceCredentialRecord {
    pub id: TokenId,
    pub name: String,
    pub secret_hash: String,
    pub scopes: BTreeSet<ServiceScope>,
    pub created_by: UserId,
    pub created_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub last_used_at: Option<DateTime<Utc>>,
    /// The address of the last accepted request, when the server could tell.
    pub last_used_from: Option<String>,
    /// Refused from this instant on; none means it never expires.
    pub expires_at: Option<DateTime<Utc>>,
    /// Canonical addresses or CIDR blocks the credential may be used from; empty means anywhere.
    pub allowed_sources: Vec<String>,
    /// Requests per minute; none means the server default.
    pub rate_limit_per_minute: Option<u32>,
}

impl ServiceCredentialRecord {
    /// The audit actor. The `@` keeps it apart from every account name.
    pub fn actor(&self) -> UserId {
        UserId::new(format!("@service:{}", self.name))
    }

    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        self.expires_at.is_some_and(|t| t <= now)
    }

    /// Whether a request from `source` is allowed. A restricted credential refuses an unknown address.
    pub fn allows_source(&self, source: Option<std::net::IpAddr>) -> bool {
        self.allowed_sources.is_empty()
            || source.is_some_and(|ip| crate::source_range::any_contains(&self.allowed_sources, ip))
    }
}

/// Service credential strings look like `pyns_<12 hex id>_<64 hex secret>`.
pub mod service_credential {
    use super::*;

    const PREFIX: &str = "pyns_";
    const MAX_NAME_LENGTH: usize = 64;

    /// Lowercase letters, digits, `-` and `_`, 1 to 64 characters, starting with a letter or digit.
    pub fn validate_name(name: &str) -> Result<String> {
        let ok = !name.is_empty()
            && name.len() <= MAX_NAME_LENGTH
            && name
                .bytes()
                .next()
                .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
            && name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
        if ok {
            Ok(name.to_string())
        } else {
            Err(PynError::InvalidRequest(format!(
                "invalid service credential name {name:?}: use 1 to 64 lowercase letters, digits, '-' or '_', starting with a letter or digit"
            )))
        }
    }

    /// A new id, the full credential to hand over once, and the hash to store.
    pub fn generate() -> Result<(TokenId, String, String)> {
        let (id, secret, hash) = token::random_parts()?;
        Ok((TokenId(id.clone()), format!("{PREFIX}{id}_{secret}"), hash))
    }

    pub fn is_service(raw: &str) -> bool {
        raw.starts_with(PREFIX)
    }

    pub fn parse(raw: &str) -> Option<(TokenId, &str)> {
        let rest = raw.strip_prefix(PREFIX)?;
        let (id, secret) = rest.split_once('_')?;
        token::is_id_and_secret(id, secret).then(|| (TokenId(id.to_string()), secret))
    }
}

/// Who may create an account without an administrator adding them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegistrationMode {
    Open,
    InviteOnly,
    Closed,
}

impl RegistrationMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::InviteOnly => "invite",
            Self::Closed => "closed",
        }
    }
}

impl FromStr for RegistrationMode {
    type Err = PynError;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "open" => Ok(Self::Open),
            "invite" => Ok(Self::InviteOnly),
            "closed" => Ok(Self::Closed),
            other => Err(PynError::InvalidRequest(format!(
                "unknown registration mode {other:?}; use open, invite or closed"
            ))),
        }
    }
}

/// Whether a signed-out caller can read anything: nothing on a `private` server, public things on a `public` one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessMode {
    #[default]
    Private,
    Public,
}

impl AccessMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Private => "private",
            Self::Public => "public",
        }
    }
}

impl FromStr for AccessMode {
    type Err = PynError;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "private" => Ok(Self::Private),
            "public" => Ok(Self::Public),
            other => Err(PynError::InvalidRequest(format!(
                "unknown access mode {other:?}; use private or public"
            ))),
        }
    }
}

/// Who may create an organization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrgCreation {
    Anyone,
    AdminsOnly,
}

impl OrgCreation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Anyone => "anyone",
            Self::AdminsOnly => "admins",
        }
    }
}

impl FromStr for OrgCreation {
    type Err = PynError;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "anyone" => Ok(Self::Anyone),
            "admins" => Ok(Self::AdminsOnly),
            other => Err(PynError::InvalidRequest(format!(
                "unknown organization creation setting {other:?}; use anyone or admins"
            ))),
        }
    }
}

/// How an organization owner adds a member.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrgMemberAdd {
    /// Adding creates a pending invitation the user accepts or declines.
    Invite,
    /// A server administrator adds without consent; everyone else still invites.
    Direct,
}

impl OrgMemberAdd {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Invite => "invite",
            Self::Direct => "direct",
        }
    }
}

impl FromStr for OrgMemberAdd {
    type Err = PynError;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "invite" => Ok(Self::Invite),
            "direct" => Ok(Self::Direct),
            other => Err(PynError::InvalidRequest(format!(
                "unknown organization member add setting {other:?}; use invite or direct"
            ))),
        }
    }
}

/// A pending offer of organization membership; at most one per organization and user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgInvitation {
    pub org: UserId,
    pub user: UserId,
    pub role: OrgRole,
    pub invited_by: UserId,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

/// The outcome of recording an invitation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvitationCreate {
    Created,
    AlreadyMember,
    AlreadyInvited,
}

/// The outcome of accepting an invitation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvitationAccept {
    /// The membership now exists, with the offered role.
    Joined(OrgRole),
    /// No live invitation (never made, cancelled, declined or expired).
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct InviteId(pub String);

impl fmt::Display for InviteId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A one-time invitation. Only a hash of its secret is kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InviteRecord {
    pub id: InviteId,
    pub secret_hash: String,
    pub repo: RepoId,
    pub role: Role,
    pub created_by: UserId,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub used_at: Option<DateTime<Utc>>,
    pub used_by: Option<UserId>,
    pub revoked_at: Option<DateTime<Utc>>,
}

/// What a server invitation gives the new account besides the account itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvitationGrant {
    Repository { repo: RepoId, role: Role },
    Organization { org: UserId, role: OrgRole },
}

/// A one-time invitation to the server itself, created by an administrator. Only a hash of its secret is kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerInvitation {
    pub id: InviteId,
    pub secret_hash: String,
    /// The address it was meant for; the server never reads mail sent there.
    pub email: Option<String>,
    pub grant: Option<InvitationGrant>,
    pub created_by: UserId,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub used_at: Option<DateTime<Utc>>,
    pub used_by: Option<UserId>,
    pub revoked_at: Option<DateTime<Utc>>,
}

/// Server invitation codes look like `pynv_<12 hex id>_<64 hex secret>`.
pub mod server_invite {
    use super::*;

    const PREFIX: &str = "pynv_";

    pub fn generate() -> Result<(InviteId, String, String)> {
        let (id, secret, hash) = token::random_parts()?;
        Ok((InviteId(id.clone()), format!("{PREFIX}{id}_{secret}"), hash))
    }

    pub fn parse(raw: &str) -> Option<(InviteId, &str)> {
        let rest = raw.strip_prefix(PREFIX)?;
        let (id, secret) = rest.split_once('_')?;
        token::is_id_and_secret(id, secret).then(|| (InviteId(id.to_string()), secret))
    }
}

/// Invitation codes look like `pyni_<12 hex id>_<64 hex secret>`.
pub mod invite {
    use super::*;

    const PREFIX: &str = "pyni_";

    /// A new id, the full code to hand to the invitee once, and the hash to store.
    pub fn generate() -> Result<(InviteId, String, String)> {
        let (id, secret, hash) = token::random_parts()?;
        Ok((InviteId(id.clone()), format!("{PREFIX}{id}_{secret}"), hash))
    }

    pub fn parse(raw: &str) -> Option<(InviteId, &str)> {
        let rest = raw.strip_prefix(PREFIX)?;
        let (id, secret) = rest.split_once('_')?;
        token::is_id_and_secret(id, secret).then(|| (InviteId(id.to_string()), secret))
    }
}

/// Rules for new accounts and password hashing.
pub mod account {
    use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};

    use super::*;

    pub const MIN_PASSWORD_LENGTH: usize = 10;
    pub const MAX_PASSWORD_LENGTH: usize = 256;
    const MAX_EMAIL_LENGTH: usize = 254;

    /// Names that routes or pages use, so no user or organization may take them.
    pub const RESERVED_NAMES: [&str; 25] = [
        "_",
        "-",
        "about",
        "admin",
        "api",
        "assets",
        "explore",
        "healthz",
        "help",
        "login",
        "logout",
        "new",
        "notifications",
        "openapi",
        "orgs",
        "pricing",
        "readyz",
        "register",
        "search",
        "settings",
        "signup",
        "static",
        "user",
        "users",
        "v1",
    ];

    pub fn is_reserved_name(name: &str) -> bool {
        RESERVED_NAMES.contains(&name)
    }

    /// The canonical form of a typed owner name: trimmed, each run of whitespace one `-`, ASCII lowercase.
    pub fn normalize_name(name: &str) -> String {
        name.split_whitespace()
            .collect::<Vec<_>>()
            .join("-")
            .to_ascii_lowercase()
    }

    /// A user name in canonical form: lowercase letters, digits, `-` and `_`, 2 to 39 characters, starting with a
    /// letter or digit, and not reserved. Typed forms normalise first (see `normalize_name`).
    pub fn validate_username(name: &str) -> Result<UserId> {
        validate_namespace(name, "a user name")
    }

    /// An organization name follows the user name rules: both live in one namespace.
    pub fn validate_org_name(name: &str) -> Result<UserId> {
        validate_namespace(name, "an organization name")
    }

    fn validate_namespace(typed: &str, noun: &str) -> Result<UserId> {
        let name = normalize_name(typed);
        if is_reserved_name(&name) {
            return Err(PynError::ReservedName(name));
        }
        let ok_chars = name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
        let starts_ok = name
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
        if (2..=39).contains(&name.len()) && ok_chars && starts_ok {
            Ok(UserId::new(name))
        } else {
            Err(PynError::InvalidRequest(format!(
                "{noun} is 2 to 39 letters, digits, '-' or '_' (spaces become '-', case is ignored), starting with a letter or digit"
            )))
        }
    }

    /// A random password of 48 hex characters (192 bits) that passes `validate_password`.
    pub fn generate_password() -> Result<String> {
        super::token::random_hex(24)
    }

    pub fn validate_password(password: &str) -> Result<()> {
        let length = password.chars().count();
        if length < MIN_PASSWORD_LENGTH {
            return Err(PynError::InvalidRequest(format!(
                "a password needs at least {MIN_PASSWORD_LENGTH} characters"
            )));
        }
        if length > MAX_PASSWORD_LENGTH {
            return Err(PynError::InvalidRequest(format!(
                "a password is at most {MAX_PASSWORD_LENGTH} characters"
            )));
        }
        Ok(())
    }

    /// Trims and lowercases an address and checks its shape; delivery is the only real proof it works.
    pub fn normalize_email(email: &str) -> Result<String> {
        let email = email.trim().to_lowercase();
        let bad = || PynError::InvalidRequest("that is not a valid email address".into());
        let (local, domain) = email.split_once('@').ok_or_else(bad)?;
        let plain = |s: &str| {
            s.chars()
                .all(|c| c.is_ascii_graphic() && !matches!(c, '@' | '<' | '>' | ',' | ';' | '"'))
        };
        let domain_ok = domain.contains('.')
            && !domain.starts_with(['.', '-'])
            && !domain.ends_with(['.', '-'])
            && !domain.contains("..");
        if email.len() > MAX_EMAIL_LENGTH
            || local.is_empty()
            || !plain(local)
            || !plain(domain)
            || !domain_ok
        {
            return Err(bad());
        }
        Ok(email)
    }

    /// An argon2id hash with its own random salt, as a PHC string.
    pub fn hash_password(password: &str) -> Result<String> {
        Argon2::default()
            .hash_password(password.as_bytes())
            .map(|h| h.to_string())
            .map_err(|e| PynError::Storage(format!("could not hash the password: {e}")))
    }

    pub fn verify_password(password: &str, hash: &str) -> bool {
        hash.parse::<PasswordHash>().is_ok_and(|parsed| {
            Argon2::default()
                .verify_password(password.as_bytes(), &parsed)
                .is_ok()
        })
    }
}

/// How far a new account has come in the sign-up flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignupStage {
    PendingVerification,
    PendingApproval,
    Complete,
}

impl SignupStage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PendingVerification => "pending_verification",
            Self::PendingApproval => "pending_approval",
            Self::Complete => "active",
        }
    }
}

impl FromStr for SignupStage {
    type Err = PynError;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "pending_verification" => Ok(Self::PendingVerification),
            "pending_approval" => Ok(Self::PendingApproval),
            "active" => Ok(Self::Complete),
            other => Err(PynError::Storage(format!(
                "unknown sign-up stage {other:?}"
            ))),
        }
    }
}

/// What an account may do right now; an administrator's disable overrides the sign-up stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountStatus {
    PendingVerification,
    PendingApproval,
    Active,
    Disabled,
}

impl AccountStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PendingVerification => "pending_verification",
            Self::PendingApproval => "pending_approval",
            Self::Active => "active",
            Self::Disabled => "disabled",
        }
    }
}

impl fmt::Display for AccountStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for AccountStatus {
    type Err = PynError;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "pending_verification" => Ok(Self::PendingVerification),
            "pending_approval" => Ok(Self::PendingApproval),
            "active" => Ok(Self::Active),
            "disabled" => Ok(Self::Disabled),
            other => Err(PynError::InvalidRequest(format!(
                "unknown account status {other:?}; use pending_verification, pending_approval, active or disabled"
            ))),
        }
    }
}

/// What a namespace name belongs to. Organizations cannot sign in, hold keys or tokens, or sign up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountKind {
    User,
    Org,
}

impl AccountKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Org => "org",
        }
    }
}

impl AccountId {
    /// A fresh random id: `u` or `o` for the kind, then 96 random bits in hex.
    pub fn generate(kind: AccountKind) -> Result<Self> {
        let prefix = match kind {
            AccountKind::User => 'u',
            AccountKind::Org => 'o',
        };
        Ok(Self::new(format!("{prefix}{}", token::random_hex(12)?)))
    }
}

impl TeamId {
    /// A fresh random id: `t`, then 96 random bits in hex.
    pub fn generate() -> Result<Self> {
        Ok(Self::new(format!("t{}", token::random_hex(12)?)))
    }
}

impl fmt::Display for AccountKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for AccountKind {
    type Err = PynError;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "user" => Ok(Self::User),
            "org" => Ok(Self::Org),
            other => Err(PynError::Storage(format!("unknown account kind {other:?}"))),
        }
    }
}

/// A person's standing in an organization. A member gets no repository access from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrgRole {
    Owner,
    Member,
}

impl OrgRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Member => "member",
        }
    }
}

impl fmt::Display for OrgRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for OrgRole {
    type Err = PynError;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "owner" => Ok(Self::Owner),
            "member" => Ok(Self::Member),
            other => Err(PynError::Storage(format!("unknown org role {other:?}"))),
        }
    }
}

/// A flat group of an organization's members that holds one role per repository the organization owns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamRecord {
    pub id: TeamId,
    pub org: UserId,
    pub slug: String,
    pub name: String,
    pub description: String,
    pub created_at: DateTime<Utc>,
}

/// Where an effective role comes from. When sources tie on the role, the first listed wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoleSource {
    Direct,
    Team,
    OrgOwner,
}

impl RoleSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Team => "team",
            Self::OrgOwner => "org_owner",
        }
    }
}

impl fmt::Display for RoleSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Rules for team names.
pub mod team {
    use super::*;

    const MAX_NAME: usize = 100;
    const MAX_DESCRIPTION: usize = 500;

    /// 1 to 39 lowercase letters, digits, `-` or `_`, starting with a letter or digit.
    pub fn validate_slug(slug: &str) -> Result<String> {
        let chars_ok = slug
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
        let starts_ok = slug
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
        if (1..=39).contains(&slug.len()) && chars_ok && starts_ok {
            Ok(slug.to_string())
        } else {
            Err(PynError::InvalidRequest(
                "a team slug is 1 to 39 lowercase letters, digits, '-' or '_', starting with a letter or digit"
                    .into(),
            ))
        }
    }

    /// The slug of a typed team name: the owner-name canonical form (see `account::normalize_name`) with characters a
    /// slug cannot hold dropped, runs of `-` collapsed and leading or trailing `-` and `_` trimmed.
    pub fn derive_slug(name: &str) -> Result<String> {
        let kept: String = super::account::normalize_name(name)
            .chars()
            .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_'))
            .collect();
        let slug = kept
            .split('-')
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("-");
        let slug = slug.trim_matches(|c| c == '-' || c == '_');
        validate_slug(slug).map_err(|_| {
            PynError::InvalidRequest(format!(
                "no team slug can be derived from the name {:?} (it gives {slug:?}); pass a slug of 1 to 39 lowercase \
                 letters, digits, '-' or '_', starting with a letter or digit",
                name.trim()
            ))
        })
    }

    pub fn validate_name(name: &str) -> Result<String> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > MAX_NAME || name.chars().any(char::is_control)
        {
            return Err(PynError::InvalidRequest(format!(
                "a team name is 1 to {MAX_NAME} characters without control characters"
            )));
        }
        Ok(name.to_string())
    }

    pub fn validate_description(description: &str) -> Result<String> {
        let description = description.trim();
        if description.chars().count() > MAX_DESCRIPTION
            || description
                .chars()
                .any(|c| c.is_control() && !matches!(c, '\n' | '\t'))
        {
            return Err(PynError::InvalidRequest(format!(
                "a team description is at most {MAX_DESCRIPTION} characters"
            )));
        }
        Ok(description.to_string())
    }
}

/// An account as the sign-up and administration rules see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountRecord {
    pub id: AccountId,
    pub user: UserId,
    pub kind: AccountKind,
    /// Lowercase.
    pub email: Option<String>,
    pub email_verified_at: Option<DateTime<Utc>>,
    pub signup: SignupStage,
    pub disabled_at: Option<DateTime<Utc>>,
    pub disabled_reason: Option<String>,
    /// Runs the server: approves and disables accounts.
    pub is_admin: bool,
    pub created_at: DateTime<Utc>,
    /// Set on an organization while a delete is in flight.
    pub deleting_since: Option<DateTime<Utc>>,
    /// Set while the owner is frozen: every write under it is refused.
    pub frozen_at: Option<DateTime<Utc>>,
    pub frozen_reason: Option<String>,
}

/// How long an organization's deleting mark blocks repository creation before it counts as left by a crash.
pub const ORG_DELETE_MARK_SECONDS: i64 = 60;

impl AccountRecord {
    /// Whether a fresh deleting mark is set at `now`.
    pub fn is_deleting(&self, now: DateTime<Utc>) -> bool {
        self.deleting_since
            .is_some_and(|since| since > now - chrono::Duration::seconds(ORG_DELETE_MARK_SECONDS))
    }

    pub fn is_frozen(&self) -> bool {
        self.frozen_at.is_some()
    }

    pub fn status(&self) -> AccountStatus {
        if self.disabled_at.is_some() {
            return AccountStatus::Disabled;
        }
        match self.signup {
            SignupStage::PendingVerification => AccountStatus::PendingVerification,
            SignupStage::PendingApproval => AccountStatus::PendingApproval,
            SignupStage::Complete => AccountStatus::Active,
        }
    }
}

/// A self-registered account to store, with its password already hashed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAccount {
    pub id: AccountId,
    pub user: UserId,
    pub email: Option<String>,
    pub password_hash: String,
    pub signup: SignupStage,
    pub created_at: DateTime<Utc>,
}

/// What first-run setup recorded about the server; its presence means the server is initialised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerSettings {
    pub initialised_at: DateTime<Utc>,
    pub server_name: Option<String>,
    pub public_url: Option<String>,
    /// Overrides the configured registration mode.
    pub registration: Option<RegistrationMode>,
    pub access_mode: AccessMode,
    /// Set while every account must hold a second factor.
    pub two_factor: Option<crate::two_factor::TwoFactorPolicy>,
}

/// A pending email check. Only a hash of its token is kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationRecord {
    pub token_hash: String,
    pub user: UserId,
    pub email: String,
    pub expires_at: DateTime<Utc>,
}

/// Verification tokens are 64 random hex characters.
pub mod verification {
    use super::*;

    /// The token to put in the email and the hash to store.
    pub(crate) fn generate() -> Result<(String, String)> {
        let secret = token::random_hex(32)?;
        let hash = token::hash_secret(&secret);
        Ok((secret, hash))
    }

    pub fn hash(raw: &str) -> String {
        token::hash_secret(raw.trim())
    }
}

/// A web sign-in. Only a hash of the cookie value is kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecord {
    pub id_hash: String,
    pub user: UserId,
    /// Sent back in a header on state-changing requests.
    pub csrf_token: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

/// Session cookie values are 64 random hex characters.
pub mod session {
    use super::*;

    /// The cookie value to hand to the browser, the CSRF token and the hash to store.
    pub(crate) fn generate() -> Result<(String, String, String)> {
        let secret = token::random_hex(32)?;
        let csrf = token::random_hex(32)?;
        let hash = token::hash_secret(&secret);
        Ok((secret, csrf, hash))
    }

    pub fn hash(cookie_value: &str) -> String {
        token::hash_secret(cookie_value)
    }
}

/// An SSH public key linked to an account. The fingerprint is unique across the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshKeyRecord {
    pub id: String,
    pub user: UserId,
    pub title: String,
    /// `ssh-ed25519`, `ecdsa-sha2-nistp256`, `ssh-rsa` and so on.
    pub algorithm: String,
    /// The key as an OpenSSH line without its comment.
    pub public_key: String,
    /// `SHA256:...`, as `ssh-keygen -l` prints it.
    pub fingerprint: String,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

/// Parsing and vetting of public keys people paste or upload.
pub mod ssh {
    use ssh_key::{Algorithm, HashAlg, PublicKey};

    use super::*;

    const MIN_RSA_BITS: usize = 2048;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct ParsedKey {
        pub algorithm: String,
        pub public_key: String,
        pub fingerprint: String,
        /// The comment at the end of the line, often `user@host`.
        pub comment: String,
    }

    /// Accepts Ed25519, ECDSA, security keys, and RSA of at least 2048 bits; refuses everything else.
    pub fn parse(text: &str) -> Result<ParsedKey> {
        let bad = |why: String| PynError::InvalidRequest(why);
        let mut key = PublicKey::from_openssh(text.trim())
            .map_err(|_| bad("that is not an OpenSSH public key (it should look like `ssh-ed25519 AAAA... name`)".into()))?;
        match key.algorithm() {
            Algorithm::Ed25519
            | Algorithm::Ecdsa { .. }
            | Algorithm::SkEd25519
            | Algorithm::SkEcdsaSha2NistP256 => {}
            Algorithm::Rsa { .. } => {
                let bits = key
                    .key_data()
                    .rsa()
                    .and_then(|r| r.n.as_positive_bytes())
                    .map_or(0, |n| n.len() * 8);
                if bits < MIN_RSA_BITS {
                    return Err(bad(format!(
                        "RSA keys must be at least {MIN_RSA_BITS} bits; this one is {bits}"
                    )));
                }
            }
            other => return Err(bad(format!("{} keys are not accepted", other.as_str()))),
        }
        let comment = key.comment().to_string();
        let fingerprint = key.fingerprint(HashAlg::Sha256).to_string();
        let algorithm = key.algorithm().as_str().to_string();
        key.set_comment("");
        let public_key = key
            .to_openssh()
            .map_err(|_| bad("could not read the key".into()))?;
        Ok(ParsedKey {
            algorithm,
            public_key,
            fingerprint,
            comment,
        })
    }
}

/// Fixtures for tests that think in the nine internal permissions.
#[cfg(any(feature = "contract", feature = "test-support"))]
pub mod fixtures {
    use super::*;
    use crate::access_service::{NewToken, TokenExpiry};

    /// The closest grants for a permission list (the same mapping the token migration applies), with metadata read.
    pub fn grants_for(permissions: &[Permission]) -> BTreeMap<TokenResource, GrantLevel> {
        use GrantLevel::*;
        let has = |p| permissions.contains(&p);
        let mut grants = BTreeMap::from([(TokenResource::Metadata, Read)]);
        if has(Permission::Checkin) || has(Permission::Restore) {
            grants.insert(TokenResource::Contents, Write);
        } else if has(Permission::Read) {
            grants.insert(TokenResource::Contents, Read);
        }
        if has(Permission::ForceUnlock) {
            grants.insert(TokenResource::Locks, Admin);
        } else if has(Permission::Lock) {
            grants.insert(TokenResource::Locks, Write);
        } else if grants.contains_key(&TokenResource::Contents) {
            grants.insert(TokenResource::Locks, Read);
        }
        for (permission, resource, level) in [
            (Permission::EditPolicy, TokenResource::Policy, Write),
            (Permission::ManageUsers, TokenResource::Members, Write),
            (
                Permission::ManageRoles,
                TokenResource::Administration,
                Write,
            ),
            (Permission::ViewAudit, TokenResource::Audit, Read),
        ] {
            if has(permission) {
                grants.insert(resource, level);
            }
        }
        grants
    }

    /// A stored token for `user`: `repos` empty means every repository of the owner.
    pub fn token_record(
        id: &str,
        user: &str,
        permissions: &[Permission],
        repos: Vec<RepoId>,
        now: DateTime<Utc>,
    ) -> TokenRecord {
        TokenRecord {
            id: TokenId(id.to_string()),
            user: UserId::new(user),
            name: format!("token {id}"),
            description: None,
            secret_hash: "00".repeat(32),
            owner: UserId::new(user),
            repo_access: if repos.is_empty() {
                RepoAccess::All
            } else {
                RepoAccess::Selected
            },
            repos,
            grants: grants_for(permissions),
            created_at: now,
            expires_at: None,
            revoked_at: None,
            last_used_at: None,
            sign_in: false,
        }
    }

    /// A sign-in token: full scope, every grant, every owner.
    pub fn sign_in_record(id: &str, user: &str, now: DateTime<Utc>) -> TokenRecord {
        TokenRecord {
            sign_in: true,
            repo_access: RepoAccess::All,
            grants: TokenRecord::full_grants(),
            ..token_record(id, user, &[], Vec::new(), now)
        }
    }

    /// A request for a token on the creator's own selected repositories that never expires.
    pub fn new_token(
        name: &str,
        owner: &str,
        permissions: &[Permission],
        repos: Vec<RepoId>,
    ) -> NewToken {
        NewToken {
            name: name.to_string(),
            description: None,
            owner: UserId::new(owner),
            expiry: TokenExpiry::Never,
            repo_access: RepoAccess::Selected,
            repos,
            grants: grants_for(permissions),
        }
    }

    /// Like `new_token`, owned by the actor's account, with an optional fixed expiry (none means never).
    pub fn new_token_for(
        actor: &Identity,
        name: &str,
        permissions: impl IntoIterator<Item = Permission>,
        repos: Vec<RepoId>,
        expires_at: Option<DateTime<Utc>>,
    ) -> NewToken {
        let permissions: Vec<_> = permissions.into_iter().collect();
        NewToken {
            expiry: expires_at.map_or(TokenExpiry::Never, TokenExpiry::At),
            ..new_token(name, actor.user.as_str(), &permissions, repos)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_need_maps_to_a_permission_its_level_adds() {
        for resource in TokenResource::ALL {
            for level in [GrantLevel::Read, GrantLevel::Write, GrantLevel::Admin] {
                if level > resource.max_level() || level < resource.min_level() {
                    continue;
                }
                let need = GrantNeed::new(resource, level);
                assert_eq!(
                    need.permission().is_some(),
                    !grant_delta(resource, level).is_empty(),
                    "{}",
                    need.label()
                );
                if let Some(p) = need.permission() {
                    assert!(
                        grant_delta(resource, level).contains(&p),
                        "{}",
                        need.label()
                    );
                }
            }
        }
    }

    #[test]
    fn a_token_must_hold_the_level_of_the_resource() {
        let token = |grants: &[(TokenResource, GrantLevel)]| Identity {
            user: UserId::new("alice"),
            credential: Credential::Token(TokenRecord {
                id: TokenId("t".into()),
                user: UserId::new("alice"),
                name: "t".into(),
                description: None,
                secret_hash: String::new(),
                owner: UserId::new("alice"),
                repo_access: RepoAccess::All,
                repos: Vec::new(),
                grants: grants.iter().copied().collect(),
                created_at: Utc::now(),
                expires_at: None,
                revoked_at: None,
                last_used_at: None,
                sign_in: false,
            }),
        };
        use GrantLevel::*;
        use TokenResource::*;
        let who = token(&[(Metadata, Read), (Contents, Read), (Locks, Write)]);
        assert!(who.require_grant(GrantNeed::new(Contents, Read)).is_ok());
        assert!(who.require_grant(GrantNeed::new(Locks, Read)).is_ok());
        assert!(who.require_grant(GrantNeed::new(Locks, Write)).is_ok());
        assert!(who.require_grant(GrantNeed::new(Contents, Write)).is_err());
        assert!(who.require_grant(GrantNeed::new(Locks, Admin)).is_err());
        assert!(who.require_grant(GrantNeed::new(Audit, Read)).is_err());
        let session = Identity {
            user: UserId::new("alice"),
            credential: Credential::Session,
        };
        assert!(session.require_grant(GrantNeed::new(Audit, Read)).is_ok());
    }

    #[test]
    fn organization_and_account_resources_have_their_own_names_and_add_no_repository_permission() {
        let names: BTreeSet<_> = TokenResource::every().map(TokenResource::as_str).collect();
        assert_eq!(names.len(), TokenResource::every().count(), "names clash");
        for r in TokenResource::every() {
            assert_eq!(r.as_str().parse::<TokenResource>().unwrap(), r);
            let owner_scoped = r.scope() != GrantScope::Repository;
            assert_eq!(
                r.as_str().starts_with("org_") || r.as_str().starts_with("account_"),
                owner_scoped
            );
            if owner_scoped {
                for level in [GrantLevel::Read, GrantLevel::Write] {
                    assert!(permissions_of(&BTreeMap::from([(r, level)])).is_empty());
                }
            }
        }
        assert_eq!(TokenResource::OrgAudit.max_level(), GrantLevel::Read);
        assert_eq!(TokenResource::AccountLocks.max_level(), GrantLevel::Read);
        assert_eq!(TokenResource::OrgTeams.max_level(), GrantLevel::Write);
        assert_eq!(
            TokenRecord::full_grants().len(),
            TokenResource::ALL.len(),
            "a sign-in token's grants stay the repository ones"
        );
    }

    #[test]
    fn an_owner_grant_needs_the_matching_owner() {
        use GrantLevel::*;
        use TokenResource::*;
        let token = |owner: &str, grants: &[(TokenResource, GrantLevel)]| Identity {
            user: UserId::new("alice"),
            credential: Credential::Token(TokenRecord {
                id: TokenId("t".into()),
                user: UserId::new("alice"),
                name: "t".into(),
                description: None,
                secret_hash: String::new(),
                owner: UserId::new(owner),
                repo_access: RepoAccess::Public,
                repos: Vec::new(),
                grants: grants.iter().copied().collect(),
                created_at: Utc::now(),
                expires_at: None,
                revoked_at: None,
                last_used_at: None,
                sign_in: false,
            }),
        };
        let (acme, beta) = (UserId::new("acme"), UserId::new("beta"));
        let need = GrantNeed::new(OrgMembers, Write);
        let who = token("acme", &[(OrgMembers, Write)]);
        assert!(who.require_org_grant(&acme, need).is_ok());
        assert!(who.require_org_grant(&beta, need).is_err());
        assert!(
            who.require_account_grant(GrantNeed::new(AccountProfile, Write))
                .is_err()
        );
        assert!(
            token("acme", &[(OrgMembers, Read)])
                .require_org_grant(&acme, need)
                .is_err()
        );
        let own = token("alice", &[(AccountSshKeys, Write)]);
        assert!(
            own.require_account_grant(GrantNeed::new(AccountSshKeys, Read))
                .is_ok()
        );
        assert!(
            own.require_account_grant(GrantNeed::new(AccountProfile, Write))
                .is_err()
        );
        assert!(own.require_org_grant(&acme, need).is_err());
    }
    #[test]
    fn owner_names_normalise_to_one_canonical_form() {
        use super::account::{normalize_name, validate_org_name, validate_username};
        for typed in [
            "Test Studio",
            "test-studio",
            "TeSt-StuDiO",
            "  test  studio\t",
            "TEST-STUDIO ",
        ] {
            assert_eq!(normalize_name(typed), "test-studio", "{typed:?}");
            assert_eq!(validate_org_name(typed).unwrap().as_str(), "test-studio");
            assert_eq!(validate_username(typed).unwrap().as_str(), "test-studio");
        }
        assert_eq!(normalize_name("a - b"), "a---b");
        assert_eq!(normalize_name("Under_Score"), "under_score");
    }

    #[test]
    fn owner_names_outside_the_alphabet_are_refused_and_reserved_ones_ignore_case() {
        use super::account::{validate_org_name, validate_username};
        for bad in ["", " ", "a", "-acme", "ac.me", "ac/me", "ac!me", "é", "ünï"] {
            assert!(
                matches!(validate_org_name(bad), Err(PynError::InvalidRequest(_))),
                "{bad:?}"
            );
        }
        assert!(validate_username(&"x".repeat(40)).is_err());
        assert!(validate_username(&"X".repeat(39)).is_ok());
        for reserved in ["Admin", " ORGS ", "_", " - ", "Settings"] {
            assert!(
                matches!(validate_username(reserved), Err(PynError::ReservedName(_))),
                "{reserved:?}"
            );
        }
    }
}
