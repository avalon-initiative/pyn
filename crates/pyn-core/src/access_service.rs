use std::collections::BTreeSet;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::access::{Permission, Principal, Role, RoleDefinitions, TokenId, TokenRecord, token};
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

    async fn create_token(&self, token: TokenRecord) -> Result<()>;

    async fn get_token(&self, id: &TokenId) -> Result<Option<TokenRecord>>;

    /// A user's tokens, newest first.
    async fn list_tokens(&self, user: &UserId) -> Result<Vec<TokenRecord>>;

    /// Marks the token revoked; false if there is no such token. Revoking twice is harmless.
    async fn revoke_token(&self, id: &TokenId, now: DateTime<Utc>) -> Result<bool>;

    async fn touch_token(&self, id: &TokenId, now: DateTime<Utc>) -> Result<()>;
}

/// Authentication of tokens and every rule about who may manage users, roles and tokens.
pub struct AccessService {
    store: Arc<dyn AccessStore>,
    clock: Arc<dyn Clock>,
}

fn unauthenticated(why: &str) -> PynError {
    PynError::Unauthenticated(why.to_string())
}

impl AccessService {
    pub fn new(store: Arc<dyn AccessStore>, clock: Arc<dyn Clock>) -> Self {
        Self { store, clock }
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
