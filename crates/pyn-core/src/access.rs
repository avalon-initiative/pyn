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

    pub(super) fn random_hex(bytes: usize) -> Result<String> {
        let mut buf = vec![0u8; bytes];
        getrandom::fill(&mut buf).map_err(|e| PynError::Storage(format!("no randomness: {e}")))?;
        Ok(hex::encode(buf))
    }

    /// A random id, secret and the hash of the secret.
    pub(super) fn random_parts() -> Result<(String, String, String)> {
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

    /// Lowercase letters, digits, `-` and `_`, 2 to 39 characters, starting with a letter or digit.
    pub fn validate_username(name: &str) -> Result<UserId> {
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
            Err(PynError::InvalidRequest(
                "a user name is 2 to 39 lowercase letters, digits, '-' or '_', starting with a letter or digit".into(),
            ))
        }
    }

    pub fn validate_password(password: &str) -> Result<()> {
        if password.chars().count() < MIN_PASSWORD_LENGTH {
            return Err(PynError::InvalidRequest(format!(
                "a password needs at least {MIN_PASSWORD_LENGTH} characters"
            )));
        }
        Ok(())
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
