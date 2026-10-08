use std::collections::BTreeSet;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use chrono::Duration;

use crate::access::{
    InviteId, InviteRecord, Permission, Principal, RegistrationMode, Role, RoleDefinitions,
    SessionRecord, SshKeyRecord, TokenId, TokenRecord, account, invite, session, ssh, token,
};
use crate::clock::Clock;
use crate::error::{PynError, Result};
use crate::types::{RepoId, UserId};

/// Persistence for users, roles and tokens.
#[async_trait]
pub trait AccessStore: Send + Sync {
    /// Creates the user if it does not exist.
    async fn ensure_user(&self, user: &UserId, now: DateTime<Utc>) -> Result<()>;

    async fn set_role(&self, repo: &RepoId, user: &UserId, role: Role) -> Result<()>;

    async fn role_of(&self, repo: &RepoId, user: &UserId) -> Result<Option<Role>>;

    async fn members(&self, repo: &RepoId) -> Result<Vec<(UserId, Role)>>;

    /// The defaults with the repository's overrides applied.
    async fn role_definitions(&self, repo: &RepoId) -> Result<RoleDefinitions>;

    async fn set_role_permissions(
        &self,
        repo: &RepoId,
        role: Role,
        permissions: BTreeSet<Permission>,
    ) -> Result<()>;

    /// Creates the user; false if the name is already taken.
    async fn create_user(&self, user: &UserId, now: DateTime<Utc>) -> Result<bool>;

    async fn user_exists(&self, user: &UserId) -> Result<bool>;

    async fn set_password_hash(&self, user: &UserId, hash: &str) -> Result<()>;

    async fn password_hash(&self, user: &UserId) -> Result<Option<String>>;

    async fn create_invite(&self, invite: InviteRecord) -> Result<()>;

    async fn get_invite(&self, id: &InviteId) -> Result<Option<InviteRecord>>;

    /// A repository's invitations, newest first.
    async fn list_invites(&self, repo: &RepoId) -> Result<Vec<InviteRecord>>;

    /// Marks the invitation revoked; false if there is none. Revoking twice is harmless.
    async fn revoke_invite(&self, id: &InviteId, now: DateTime<Utc>) -> Result<bool>;

    /// Marks the invitation used by `user` if it is still unused, unrevoked and unexpired; false otherwise.
    async fn use_invite(&self, id: &InviteId, user: &UserId, now: DateTime<Utc>) -> Result<bool>;

    /// Links a key to its account; false if another account (or this one) already has the same key.
    async fn add_ssh_key(&self, key: SshKeyRecord) -> Result<bool>;

    /// An account's keys, newest first.
    async fn list_ssh_keys(&self, user: &UserId) -> Result<Vec<SshKeyRecord>>;

    /// Removes one of the account's keys; false if it has no such key.
    async fn delete_ssh_key(&self, user: &UserId, id: &str) -> Result<bool>;

    async fn find_ssh_key(&self, fingerprint: &str) -> Result<Option<SshKeyRecord>>;

    async fn touch_ssh_key(&self, fingerprint: &str, now: DateTime<Utc>) -> Result<()>;

    async fn create_session(&self, session: SessionRecord) -> Result<()>;

    async fn get_session(&self, id_hash: &str) -> Result<Option<SessionRecord>>;

    /// Removes the session; false if there is none.
    async fn delete_session(&self, id_hash: &str) -> Result<bool>;

    /// Removes every session that expired at or before `now`.
    async fn delete_expired_sessions(&self, now: DateTime<Utc>) -> Result<()>;

    async fn create_token(&self, token: TokenRecord) -> Result<()>;

    async fn get_token(&self, id: &TokenId) -> Result<Option<TokenRecord>>;

    /// A user's tokens, newest first.
    async fn list_tokens(&self, user: &UserId) -> Result<Vec<TokenRecord>>;

    /// Marks the token revoked; false if there is no such token. Revoking twice is harmless.
    async fn revoke_token(&self, id: &TokenId, now: DateTime<Utc>) -> Result<bool>;

    async fn touch_token(&self, id: &TokenId, now: DateTime<Utc>) -> Result<()>;
}

/// Server settings that shape who can join and how long a sign-in lasts.
#[derive(Debug, Clone)]
pub struct AccessConfig {
    pub registration: RegistrationMode,
    /// The role given to people who register on an open server.
    pub default_role: Role,
    pub session_days: i64,
}

impl Default for AccessConfig {
    fn default() -> Self {
        Self {
            registration: RegistrationMode::InviteOnly,
            default_role: Role::Reader,
            session_days: 30,
        }
    }
}

const MAX_FAILED_SIGN_INS: u32 = 5;
const SIGN_IN_WINDOW: Duration = Duration::minutes(15);

/// Authentication of tokens and passwords, and every rule about who may join and manage users, roles and tokens.
pub struct AccessService {
    store: Arc<dyn AccessStore>,
    clock: Arc<dyn Clock>,
    config: AccessConfig,
    /// Failed sign-ins per user name: how many, and when the window started.
    failures: Mutex<HashMap<String, (u32, DateTime<Utc>)>>,
}

fn unauthenticated(why: &str) -> PynError {
    PynError::Unauthenticated(why.to_string())
}

impl AccessService {
    pub fn new(store: Arc<dyn AccessStore>, clock: Arc<dyn Clock>) -> Self {
        Self {
            store,
            clock,
            config: AccessConfig::default(),
            failures: Mutex::new(HashMap::new()),
        }
    }

    pub fn with_config(mut self, config: AccessConfig) -> Self {
        self.config = config;
        self
    }

    pub fn registration_mode(&self) -> RegistrationMode {
        self.config.registration
    }

    /// What `user`'s role grants in `repo`; nothing for a non-member.
    pub async fn role_permissions(
        &self,
        repo: &RepoId,
        user: &UserId,
    ) -> Result<BTreeSet<Permission>> {
        let Some(role) = self.store.role_of(repo, user).await? else {
            return Ok(BTreeSet::new());
        };
        Ok(self.store.role_definitions(repo).await?.get(role).clone())
    }

    pub async fn principal(&self, repo: &RepoId, user: &UserId) -> Result<Principal> {
        Ok(Principal {
            user: user.clone(),
            permissions: self.role_permissions(repo, user).await?,
        })
    }

    /// Resolves a bearer token to a principal limited to what both the token and the user's role allow.
    pub async fn authenticate(&self, repo: &RepoId, raw: &str) -> Result<Principal> {
        let (id, secret) = token::parse(raw).ok_or_else(|| unauthenticated("malformed token"))?;
        let record = self
            .store
            .get_token(&id)
            .await?
            .ok_or_else(|| unauthenticated("unknown token"))?;
        if !token::hashes_match(&record.secret_hash, &token::hash_secret(secret)) {
            return Err(unauthenticated("unknown token"));
        }
        let now = self.clock.now();
        if record.revoked_at.is_some() {
            return Err(unauthenticated("token revoked"));
        }
        if record.expires_at.is_some_and(|t| t <= now) {
            return Err(unauthenticated("token expired"));
        }
        if !record.repos.is_empty() && !record.repos.contains(repo) {
            return Err(unauthenticated("token is not valid for this repository"));
        }
        self.store.touch_token(&id, now).await?;
        let role = self.role_permissions(repo, &record.user).await?;
        let permissions = record.permissions.intersection(&role).copied().collect();
        Ok(Principal {
            user: record.user,
            permissions,
        })
    }

    /// Creates a token for the actor. It can hold only permissions the actor holds. Returns the record and the
    /// full token string, which is not stored and cannot be shown again.
    pub async fn create_token(
        &self,
        actor: &Principal,
        name: &str,
        permissions: BTreeSet<Permission>,
        repos: Vec<RepoId>,
        expires_at: Option<DateTime<Utc>>,
    ) -> Result<(TokenRecord, String)> {
        if name.trim().is_empty() {
            return Err(PynError::InvalidRequest("a token needs a name".into()));
        }
        if permissions.is_empty() {
            return Err(PynError::InvalidRequest(
                "a token needs at least one permission".into(),
            ));
        }
        for p in &permissions {
            actor.require(*p)?;
        }
        let now = self.clock.now();
        if expires_at.is_some_and(|t| t <= now) {
            return Err(PynError::InvalidRequest(
                "expiry must be in the future".into(),
            ));
        }
        let (id, full, secret_hash) = token::generate()?;
        let record = TokenRecord {
            id,
            user: actor.user.clone(),
            name: name.trim().to_string(),
            secret_hash,
            permissions,
            repos,
            created_at: now,
            expires_at,
            revoked_at: None,
            last_used_at: None,
        };
        self.store.create_token(record.clone()).await?;
        Ok((record, full))
    }

    /// The actor's own tokens; another user's need `manage_users`.
    pub async fn list_tokens(&self, actor: &Principal, user: &UserId) -> Result<Vec<TokenRecord>> {
        if &actor.user != user {
            actor.require(Permission::ManageUsers)?;
        }
        self.store.list_tokens(user).await
    }

    /// Revokes the actor's own token; another user's needs `manage_users`.
    pub async fn revoke_token(&self, actor: &Principal, id: &TokenId) -> Result<()> {
        let record = self
            .store
            .get_token(id)
            .await?
            .ok_or_else(|| PynError::TokenNotFound(id.to_string()))?;
        if record.user != actor.user {
            actor.require(Permission::ManageUsers)?;
        }
        self.store.revoke_token(id, self.clock.now()).await?;
        Ok(())
    }

    /// A role can be given only by someone who already holds everything it grants.
    async fn require_can_grant(&self, actor: &Principal, repo: &RepoId, role: Role) -> Result<()> {
        let granted = self.store.role_definitions(repo).await?.get(role).clone();
        for p in granted {
            actor.require(p)?;
        }
        Ok(())
    }

    /// Adds the user if new and gives them `role`.
    pub async fn set_user_role(
        &self,
        actor: &Principal,
        repo: &RepoId,
        user: &UserId,
        role: Role,
    ) -> Result<()> {
        actor.require(Permission::ManageUsers)?;
        self.require_can_grant(actor, repo, role).await?;
        self.store.ensure_user(user, self.clock.now()).await?;
        self.store.set_role(repo, user, role).await
    }

    pub async fn members(&self, actor: &Principal, repo: &RepoId) -> Result<Vec<(UserId, Role)>> {
        actor.require(Permission::ManageUsers)?;
        self.store.members(repo).await
    }

    pub async fn role_definitions(&self, repo: &RepoId) -> Result<RoleDefinitions> {
        self.store.role_definitions(repo).await
    }

    /// Changes what a role grants. The admin role must keep the permissions that manage access.
    pub async fn set_role_permissions(
        &self,
        actor: &Principal,
        repo: &RepoId,
        role: Role,
        permissions: BTreeSet<Permission>,
    ) -> Result<()> {
        actor.require(Permission::ManageRoles)?;
        if role == Role::Admin
            && !(permissions.contains(&Permission::ManageUsers)
                && permissions.contains(&Permission::ManageRoles))
        {
            return Err(PynError::InvalidRequest(
                "admin must keep manage_users and manage_roles".into(),
            ));
        }
        self.store
            .set_role_permissions(repo, role, permissions)
            .await
    }

    fn check_not_throttled(&self, key: &str, now: DateTime<Utc>) -> Result<()> {
        let mut failures = self.failures.lock().unwrap();
        if let Some(&(count, since)) = failures.get(key) {
            if now >= since + SIGN_IN_WINDOW {
                failures.remove(key);
            } else if count >= MAX_FAILED_SIGN_INS {
                let retry_after_secs = (since + SIGN_IN_WINDOW - now).num_seconds().max(1);
                return Err(PynError::TooManyAttempts { retry_after_secs });
            }
        }
        Ok(())
    }

    fn note_sign_in(&self, key: &str, now: DateTime<Utc>, success: bool) {
        let mut failures = self.failures.lock().unwrap();
        if success {
            failures.remove(key);
        } else {
            failures.entry(key.to_string()).or_insert((0, now)).0 += 1;
        }
    }

    /// Checks a password for a user with access to `repo`. Repeated failures lock the user name out for a while.
    async fn verify_sign_in(
        &self,
        repo: &RepoId,
        username: &str,
        password: &str,
    ) -> Result<Principal> {
        let now = self.clock.now();
        let key = username.to_lowercase();
        self.check_not_throttled(&key, now)?;

        let user = UserId::new(username);
        let verified = match self.store.password_hash(&user).await? {
            Some(hash) => account::verify_password(password, &hash),
            None => {
                static DUMMY: OnceLock<String> = OnceLock::new();
                let dummy = DUMMY.get_or_init(|| {
                    account::hash_password("not-a-real-password").unwrap_or_default()
                });
                account::verify_password(password, dummy);
                false
            }
        };
        self.note_sign_in(&key, now, verified);
        if !verified {
            return Err(PynError::Unauthenticated(
                "wrong user name or password".into(),
            ));
        }

        let permissions = self.role_permissions(repo, &user).await?;
        if permissions.is_empty() {
            return Err(PynError::Unauthenticated(
                "this account has no access to this repository".into(),
            ));
        }
        Ok(Principal { user, permissions })
    }

    /// Checks a password and returns a token limited to the user's current role that expires after the
    /// configured number of days.
    pub async fn login(
        &self,
        repo: &RepoId,
        username: &str,
        password: &str,
    ) -> Result<(TokenRecord, String)> {
        let actor = self.verify_sign_in(repo, username, password).await?;
        let expires = self.clock.now() + Duration::days(self.config.session_days);
        let permissions = actor.permissions.clone();
        self.create_token(
            &actor,
            "sign-in",
            permissions,
            vec![repo.clone()],
            Some(expires),
        )
        .await
    }

    /// Checks a password and starts a web session. Returns the record and the cookie value, which is not stored.
    pub async fn start_session(
        &self,
        repo: &RepoId,
        username: &str,
        password: &str,
    ) -> Result<(SessionRecord, String)> {
        let who = self.verify_sign_in(repo, username, password).await?;
        let now = self.clock.now();
        self.store.delete_expired_sessions(now).await?;
        let (cookie, csrf_token, id_hash) = session::generate()?;
        let record = SessionRecord {
            id_hash,
            user: who.user,
            csrf_token,
            created_at: now,
            expires_at: now + Duration::days(self.config.session_days),
        };
        self.store.create_session(record.clone()).await?;
        Ok((record, cookie))
    }

    /// The session behind a cookie value, if it exists and has not expired.
    pub async fn find_session(&self, cookie: &str) -> Result<Option<SessionRecord>> {
        let Some(record) = self.store.get_session(&session::hash(cookie)).await? else {
            return Ok(None);
        };
        Ok((record.expires_at > self.clock.now()).then_some(record))
    }

    /// Resolves a session cookie to the user's current permissions, so a role change applies at once.
    pub async fn authenticate_session(
        &self,
        repo: &RepoId,
        cookie: &str,
    ) -> Result<(Principal, SessionRecord)> {
        let record = self
            .find_session(cookie)
            .await?
            .ok_or_else(|| unauthenticated("not signed in"))?;
        let who = self.principal(repo, &record.user).await?;
        if who.permissions.is_empty() {
            return Err(unauthenticated(
                "this account has no access to this repository",
            ));
        }
        Ok((who, record))
    }

    /// Ends the session; ending one that is already gone is harmless.
    pub async fn end_session(&self, cookie: &str) -> Result<()> {
        self.store.delete_session(&session::hash(cookie)).await?;
        Ok(())
    }

    pub fn session_lifetime(&self) -> Duration {
        Duration::days(self.config.session_days)
    }

    /// Creates an account on the server's own terms: freely when registration is open, with a valid invitation
    /// when it is invite only, never when it is closed.
    pub async fn register(
        &self,
        repo: &RepoId,
        username: &str,
        password: &str,
        invitation: Option<&str>,
    ) -> Result<UserId> {
        let user = account::validate_username(username)?;
        account::validate_password(password)?;
        let (role, claimed) = match self.config.registration {
            RegistrationMode::Closed => return Err(PynError::RegistrationClosed),
            RegistrationMode::Open => (self.config.default_role, None),
            RegistrationMode::InviteOnly => {
                let record = self.check_invitation(repo, invitation).await?;
                (record.role, Some(record.id))
            }
        };
        if self.store.user_exists(&user).await? {
            return Err(PynError::UserExists(user));
        }
        let now = self.clock.now();
        if let Some(id) = &claimed
            && !self.store.use_invite(id, &user, now).await?
        {
            return Err(PynError::InvalidInvite("it has already been used".into()));
        }
        if !self.store.create_user(&user, now).await? {
            return Err(PynError::UserExists(user));
        }
        self.store
            .set_password_hash(&user, &account::hash_password(password)?)
            .await?;
        self.store.set_role(repo, &user, role).await?;
        Ok(user)
    }

    async fn check_invitation(&self, repo: &RepoId, code: Option<&str>) -> Result<InviteRecord> {
        let bad = |why: &str| PynError::InvalidInvite(why.to_string());
        let (id, secret) =
            invite::parse(code.ok_or_else(|| bad("this server needs an invitation"))?)
                .ok_or_else(|| bad("the code is not valid"))?;
        let record = self
            .store
            .get_invite(&id)
            .await?
            .ok_or_else(|| bad("the code is not valid"))?;
        if !token::hashes_match(&record.secret_hash, &token::hash_secret(secret)) {
            return Err(bad("the code is not valid"));
        }
        let now = self.clock.now();
        if &record.repo != repo {
            return Err(bad("the code is for another repository"));
        }
        if record.revoked_at.is_some() {
            return Err(bad("it has been revoked"));
        }
        if record.used_at.is_some() {
            return Err(bad("it has already been used"));
        }
        if record.expires_at <= now {
            return Err(bad("it has expired"));
        }
        Ok(record)
    }

    /// Adds an account with an initial password, in any registration mode.
    pub async fn add_user(
        &self,
        actor: &Principal,
        repo: &RepoId,
        username: &str,
        password: &str,
        role: Role,
    ) -> Result<UserId> {
        actor.require(Permission::ManageUsers)?;
        self.require_can_grant(actor, repo, role).await?;
        let user = account::validate_username(username)?;
        account::validate_password(password)?;
        if !self.store.create_user(&user, self.clock.now()).await? {
            return Err(PynError::UserExists(user));
        }
        self.store
            .set_password_hash(&user, &account::hash_password(password)?)
            .await?;
        self.store.set_role(repo, &user, role).await?;
        Ok(user)
    }

    /// Changes the actor's own password. If they already have one, the current password must be given.
    pub async fn change_password(
        &self,
        actor: &Principal,
        current: Option<&str>,
        new: &str,
    ) -> Result<()> {
        account::validate_password(new)?;
        if let Some(hash) = self.store.password_hash(&actor.user).await?
            && !current.is_some_and(|c| account::verify_password(c, &hash))
        {
            return Err(PynError::Unauthenticated(
                "the current password is wrong".into(),
            ));
        }
        self.store
            .set_password_hash(&actor.user, &account::hash_password(new)?)
            .await
    }

    /// Creates a one-time invitation for `role`. The code is returned once and not stored.
    pub async fn create_invite(
        &self,
        actor: &Principal,
        repo: &RepoId,
        role: Role,
        valid_for: Duration,
    ) -> Result<(InviteRecord, String)> {
        actor.require(Permission::ManageUsers)?;
        self.require_can_grant(actor, repo, role).await?;
        if valid_for <= Duration::zero() {
            return Err(PynError::InvalidRequest(
                "an invitation must be valid for some time".into(),
            ));
        }
        let (id, code, secret_hash) = invite::generate()?;
        let now = self.clock.now();
        let record = InviteRecord {
            id,
            secret_hash,
            repo: repo.clone(),
            role,
            created_by: actor.user.clone(),
            created_at: now,
            expires_at: now + valid_for,
            used_at: None,
            used_by: None,
            revoked_at: None,
        };
        self.store.create_invite(record.clone()).await?;
        Ok((record, code))
    }

    pub async fn list_invites(
        &self,
        actor: &Principal,
        repo: &RepoId,
    ) -> Result<Vec<InviteRecord>> {
        actor.require(Permission::ManageUsers)?;
        self.store.list_invites(repo).await
    }

    pub async fn revoke_invite(&self, actor: &Principal, id: &InviteId) -> Result<()> {
        actor.require(Permission::ManageUsers)?;
        if self.store.revoke_invite(id, self.clock.now()).await? {
            Ok(())
        } else {
            Err(PynError::InvalidInvite(
                "there is no such invitation".into(),
            ))
        }
    }

    /// Links a public key to the actor's account. The title defaults to the key's own comment.
    pub async fn add_ssh_key(
        &self,
        actor: &Principal,
        title: Option<&str>,
        key: &str,
    ) -> Result<SshKeyRecord> {
        let parsed = ssh::parse(key)?;
        let title = title
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .or_else(|| (!parsed.comment.is_empty()).then(|| parsed.comment.clone()))
            .unwrap_or_else(|| parsed.algorithm.clone());
        self.store
            .ensure_user(&actor.user, self.clock.now())
            .await?;
        let (id, _, _) = token::random_parts()?;
        let record = SshKeyRecord {
            id,
            user: actor.user.clone(),
            title,
            algorithm: parsed.algorithm,
            public_key: parsed.public_key,
            fingerprint: parsed.fingerprint,
            created_at: self.clock.now(),
            last_used_at: None,
        };
        if !self.store.add_ssh_key(record.clone()).await? {
            return Err(PynError::KeyInUse);
        }
        Ok(record)
    }

    /// The actor's own keys; another user's need `manage_users`.
    pub async fn list_ssh_keys(
        &self,
        actor: &Principal,
        user: &UserId,
    ) -> Result<Vec<SshKeyRecord>> {
        if &actor.user != user {
            actor.require(Permission::ManageUsers)?;
        }
        self.store.list_ssh_keys(user).await
    }

    pub async fn delete_ssh_key(&self, actor: &Principal, user: &UserId, id: &str) -> Result<()> {
        if &actor.user != user {
            actor.require(Permission::ManageUsers)?;
        }
        if self.store.delete_ssh_key(user, id).await? {
            Ok(())
        } else {
            Err(PynError::KeyNotFound(id.to_string()))
        }
    }

    /// Who a presented key belongs to, with what they may do in `repo`. Used when a connection authenticates by key.
    pub async fn authenticate_ssh_key(
        &self,
        repo: &RepoId,
        fingerprint: &str,
    ) -> Result<Principal> {
        let record = self.store.find_ssh_key(fingerprint).await?.ok_or_else(|| {
            PynError::Unauthenticated("this key is not linked to an account".into())
        })?;
        self.store
            .touch_ssh_key(fingerprint, self.clock.now())
            .await?;
        self.principal(repo, &record.user).await
    }

    /// Gives an existing user a password without needing the old one, for the person running the server.
    pub async fn set_password_for_operator(&self, user: &UserId, password: &str) -> Result<()> {
        account::validate_password(password)?;
        self.store
            .set_password_hash(user, &account::hash_password(password)?)
            .await
    }

    /// Creates the first administrator and a full-access token for them, for the person running the server.
    pub async fn bootstrap_admin(&self, repo: &RepoId, user: &UserId) -> Result<String> {
        self.store.ensure_user(user, self.clock.now()).await?;
        self.store.set_role(repo, user, Role::Admin).await?;
        let actor = self.principal(repo, user).await?;
        let (_, full) = self
            .create_token(
                &actor,
                "bootstrap",
                Permission::ALL.into(),
                vec![repo.clone()],
                None,
            )
            .await?;
        Ok(full)
    }
}
