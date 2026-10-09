use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{PynError, Result};
use crate::types::{RepoId, UserId};

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
    /// A token must be unscoped and carry `manage_roles` to create or delete repositories and organizations.
    pub fn require_namespace_management(&self) -> Result<()> {
        match &self.credential {
            Credential::Token(t)
                if !t.repos.is_empty() || !t.permissions.contains(&Permission::ManageRoles) =>
            {
                Err(PynError::Forbidden(Permission::ManageRoles))
            }
            _ => Ok(()),
        }
    }
}

/// An authenticated caller and what they may do in one repository right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    pub user: UserId,
    pub permissions: BTreeSet<Permission>,
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

    /// Every permission, for the development identity.
    pub fn unrestricted(user: UserId) -> Self {
        Self {
            user,
            permissions: Permission::ALL.into(),
        }
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
    pub secret_hash: String,
    pub permissions: BTreeSet<Permission>,
    /// Empty means every repository the user can access.
    pub repos: Vec<RepoId>,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub last_used_at: Option<DateTime<Utc>>,
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

    pub(super) fn is_id_and_secret(id: &str, secret: &str) -> bool {
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
    pub const RESERVED_NAMES: [&str; 24] = [
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

    /// Lowercase letters, digits, `-` and `_`, 2 to 39 characters, starting with a letter or digit, and not
    /// reserved.
    pub fn validate_username(name: &str) -> Result<UserId> {
        validate_namespace(name, "a user name")
    }

    /// An organization name follows the user name rules: both live in one namespace.
    pub fn validate_org_name(name: &str) -> Result<UserId> {
        validate_namespace(name, "an organization name")
    }

    fn validate_namespace(name: &str, noun: &str) -> Result<UserId> {
        if is_reserved_name(name) {
            return Err(PynError::ReservedName(name.to_string()));
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
                "{noun} is 2 to 39 lowercase letters, digits, '-' or '_', starting with a letter or digit"
            )))
        }
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

    pub fn validate_name(name: &str) -> Result<String> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > MAX_NAME {
            return Err(PynError::InvalidRequest(format!(
                "a team name is 1 to {MAX_NAME} characters"
            )));
        }
        Ok(name.to_string())
    }

    pub fn validate_description(description: &str) -> Result<String> {
        let description = description.trim();
        if description.chars().count() > MAX_DESCRIPTION {
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
}

impl AccountRecord {
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
    pub user: UserId,
    pub email: Option<String>,
    pub password_hash: String,
    pub signup: SignupStage,
    pub created_at: DateTime<Utc>,
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
