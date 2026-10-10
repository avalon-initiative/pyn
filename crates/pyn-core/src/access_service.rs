use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};

use crate::access::{
    AccountKind, AccountRecord, AccountStatus, Credential, Identity, InviteId, InviteRecord,
    NewAccount, ORG_DELETE_MARK_SECONDS, OrgCreation, OrgRole, Permission, Principal,
    RegistrationMode, Role, RoleDefinitions, RoleSource, ServerSettings, ServiceCredentialRecord,
    ServiceScope, SessionRecord, SignupStage, SshKeyRecord, TeamRecord, TokenId, TokenRecord,
    VerificationRecord, account, invite, service_credential, session, ssh, token, verification,
};
use crate::audit::{AuditAction, AuditEvent, AuditQuery, AuditScope, AuditStore, NewAuditEvent};
use crate::clock::Clock;
use crate::email::{EmailMessage, EmailSender, NullEmailSender};
use crate::error::{PynError, Result};
use crate::memory::MemoryRateLimitStore;
use crate::passwords::{InlinePasswords, PasswordWorker};
use crate::ratelimit::RateLimitStore;
use crate::repo_policy::{
    CreationEffect, CreationRule, CreationScope, CreationSubject, MemberCreation, RepoPolicy,
};
use crate::store::MetadataStore;
use crate::types::{RepoId, UserId};

/// Server-wide events (account approvals and disables) are recorded in the audit store under this id, which
/// no repository can have.
pub const SERVER_AUDIT_ID: &str = "@server";

mod organizations;
mod repo_policy;
mod setup;
mod teams;

pub use setup::{SetupRequest, SetupStatus, generate_setup_token};
pub use teams::TeamDetail;

/// The outcome of changing or removing an organization member.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrgMemberChange {
    /// Done; carries the role the person held before.
    Done(OrgRole),
    NotMember,
    LastOwner,
}

/// The outcome of removing the administrator flag from an account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminRevoke {
    Revoked,
    NotAdmin,
    /// No other active administrator would remain.
    LastAdmin,
}

/// The outcome of marking an organization as being deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrgDeleteMark {
    /// This call set the mark.
    Set,
    /// A fresh mark from another delete was already set.
    Held,
    /// No such organization.
    NotFound,
}

/// Persistence for users, organizations, roles and tokens.
#[async_trait]
pub trait AccessStore: Send + Sync {
    /// Creates the user if it does not exist.
    async fn ensure_user(&self, user: &UserId, now: DateTime<Utc>) -> Result<()>;

    async fn set_role(&self, repo: &RepoId, user: &UserId, role: Role) -> Result<()>;

    async fn role_of(&self, repo: &RepoId, user: &UserId) -> Result<Option<Role>>;

    async fn members(&self, repo: &RepoId) -> Result<Vec<(UserId, Role)>>;

    /// The repositories the user has a role in, ordered by id.
    async fn repos_of(&self, user: &UserId) -> Result<Vec<RepoId>>;

    /// Removes the repository's memberships, team roles, role overrides and invitations.
    async fn delete_repo_access(&self, repo: &RepoId) -> Result<()>;

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

    /// Creates the organization with `owner` as its first owner in one step, adding the owner's account if
    /// needed; false if the name is already taken by any account.
    async fn create_org(&self, org: &UserId, owner: &UserId, now: DateTime<Utc>) -> Result<bool>;

    async fn org_role(&self, org: &UserId, user: &UserId) -> Result<Option<OrgRole>>;

    /// The organizations the user belongs to with their role, ordered by name.
    async fn orgs_of(&self, user: &UserId) -> Result<Vec<(UserId, OrgRole)>>;

    /// Removes the organization with its member, team and policy records; false if there is no such organization.
    async fn delete_org(&self, org: &UserId) -> Result<bool>;

    /// Sets the organization's deleting mark in one step unless a mark newer than `stale_before` is already set.
    async fn mark_org_deleting(
        &self,
        org: &UserId,
        now: DateTime<Utc>,
        stale_before: DateTime<Utc>,
    ) -> Result<OrgDeleteMark>;

    /// Clears the organization's deleting mark.
    async fn clear_org_deleting(&self, org: &UserId) -> Result<()>;

    /// The organization's members with their role, ordered by user name.
    async fn org_members(&self, org: &UserId) -> Result<Vec<(UserId, OrgRole)>>;

    /// Adds `user` to the organization; false if they already belong to it.
    async fn add_org_member(&self, org: &UserId, user: &UserId, role: OrgRole) -> Result<bool>;

    /// Sets a member's role, refusing to leave the organization without an owner. Returns the previous role.
    async fn set_org_role(
        &self,
        org: &UserId,
        user: &UserId,
        role: OrgRole,
    ) -> Result<OrgMemberChange>;

    /// Removes a member, their team memberships, creation rules and their direct roles in `repos` (the organization's
    /// repositories), refusing to remove the last owner. Returns the role they held.
    async fn remove_org_member(
        &self,
        org: &UserId,
        user: &UserId,
        repos: &[RepoId],
    ) -> Result<OrgMemberChange>;

    /// Creates the team; false if the organization already has one with that slug.
    async fn create_team(&self, team: TeamRecord) -> Result<bool>;

    async fn team(&self, org: &UserId, slug: &str) -> Result<Option<TeamRecord>>;

    /// The organization's teams, ordered by slug.
    async fn teams(&self, org: &UserId) -> Result<Vec<TeamRecord>>;

    /// Replaces the team's name and description; false if there is no such team.
    async fn update_team(&self, team: &TeamRecord) -> Result<bool>;

    /// Removes the team with its members, repository roles and creation rules; false if there is no such team.
    async fn delete_team(&self, org: &UserId, slug: &str) -> Result<bool>;

    /// The team's members, ordered by user name.
    async fn team_members(&self, org: &UserId, slug: &str) -> Result<Vec<UserId>>;

    /// Adds an organization member to the team; false if they are already in it.
    async fn add_team_member(&self, org: &UserId, slug: &str, user: &UserId) -> Result<bool>;

    /// Removes the person from the team; false if they were not in it.
    async fn remove_team_member(&self, org: &UserId, slug: &str, user: &UserId) -> Result<bool>;

    /// The slugs of the organization's teams the user is in, ordered.
    async fn teams_of(&self, org: &UserId, user: &UserId) -> Result<Vec<String>>;

    /// Gives the team a role on the repository, returning the role it held before.
    async fn set_team_role(
        &self,
        repo: &RepoId,
        org: &UserId,
        slug: &str,
        role: Role,
    ) -> Result<Option<Role>>;

    /// Takes the team's role on the repository away, returning the role it held.
    async fn remove_team_role(
        &self,
        repo: &RepoId,
        org: &UserId,
        slug: &str,
    ) -> Result<Option<Role>>;

    /// The repository's team grants as (team slug, role), ordered by slug.
    async fn team_grants(&self, repo: &RepoId) -> Result<Vec<(String, Role)>>;

    /// The team's repository roles, ordered by repository.
    async fn team_repos(&self, org: &UserId, slug: &str) -> Result<Vec<(RepoId, Role)>>;

    /// The highest role the user's teams hold on the repository.
    async fn team_role_of(&self, repo: &RepoId, user: &UserId) -> Result<Option<Role>>;

    /// The repositories where one of the user's teams holds a role, ordered by id.
    async fn team_repos_of(&self, user: &UserId) -> Result<Vec<RepoId>>;

    /// The organization's repository-creation policy; the default when none was set.
    async fn repo_policy(&self, org: &UserId) -> Result<RepoPolicy>;

    /// Sets what members may create with no rule, returning the previous setting.
    async fn set_member_creation(
        &self,
        org: &UserId,
        base: MemberCreation,
    ) -> Result<MemberCreation>;

    /// Adds or replaces the rule for its subject and effect; false if the team or member no longer exists.
    async fn set_creation_rule(&self, org: &UserId, rule: &CreationRule) -> Result<bool>;

    /// Removes the rule for the subject and effect, returning the scope it had.
    async fn remove_creation_rule(
        &self,
        org: &UserId,
        subject: &CreationSubject,
        effect: CreationEffect,
    ) -> Result<Option<CreationScope>>;

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

    /// Stores the credential; false if its name is already taken, revoked or not.
    async fn create_service_credential(&self, record: ServiceCredentialRecord) -> Result<bool>;

    async fn get_service_credential(&self, id: &TokenId)
    -> Result<Option<ServiceCredentialRecord>>;

    /// Every service credential, oldest first.
    async fn list_service_credentials(&self) -> Result<Vec<ServiceCredentialRecord>>;

    /// Marks the named credential revoked; false if there is none. Revoking twice is harmless.
    async fn revoke_service_credential(&self, name: &str, now: DateTime<Utc>) -> Result<bool>;

    async fn touch_service_credential(&self, id: &TokenId, now: DateTime<Utc>) -> Result<()>;

    /// Creates a self-registered account with its password; false if the name is already taken.
    async fn create_account(&self, new: NewAccount) -> Result<bool>;

    async fn account(&self, user: &UserId) -> Result<Option<AccountRecord>>;

    /// The account whose verified address is `email`, if any.
    async fn verified_email_owner(&self, email: &str) -> Result<Option<UserId>>;

    /// Accounts still waiting to verify `email`.
    async fn pending_by_email(&self, email: &str) -> Result<Vec<AccountRecord>>;

    /// Marks the account's address verified and moves it to `signup`; false if another account has already
    /// verified the same address, or there is no such account.
    async fn complete_verification(
        &self,
        user: &UserId,
        signup: SignupStage,
        now: DateTime<Utc>,
    ) -> Result<bool>;

    /// Moves the account to `signup`; false if there is no such account.
    async fn set_signup(&self, user: &UserId, signup: SignupStage) -> Result<bool>;

    /// Disables the account with a reason, or enables it with `None`; false if there is no such account.
    async fn set_disabled(
        &self,
        user: &UserId,
        disabled: Option<(DateTime<Utc>, Option<String>)>,
    ) -> Result<bool>;

    async fn set_admin(&self, user: &UserId, admin: bool) -> Result<()>;

    /// Removes the flag unless that leaves no other active administrator. Atomic against concurrent revokes.
    async fn revoke_admin(&self, user: &UserId) -> Result<AdminRevoke>;

    /// Accounts oldest first, optionally only those in `status`.
    async fn list_accounts(
        &self,
        status: Option<AccountStatus>,
        limit: usize,
    ) -> Result<Vec<AccountRecord>>;

    /// Removes expired verifications, and the unverified accounts created before `created_before` that have
    /// none left to use.
    async fn delete_stale_pending(
        &self,
        now: DateTime<Utc>,
        created_before: DateTime<Utc>,
    ) -> Result<()>;

    /// Stores the verification, replacing any earlier one for the same account.
    async fn put_verification(&self, record: VerificationRecord) -> Result<()>;

    /// Removes and returns the verification; each can be taken once.
    async fn take_verification(&self, token_hash: &str) -> Result<Option<VerificationRecord>>;

    async fn delete_sessions_of(&self, user: &UserId) -> Result<()>;

    /// What setup recorded; `None` while the server is uninitialised.
    async fn server_settings(&self) -> Result<Option<ServerSettings>>;

    /// Creates `admin` as a server administrator and records `settings` in one step. False if setup already ran;
    /// `UserExists` if the name is taken.
    async fn complete_setup(&self, admin: NewAccount, settings: ServerSettings) -> Result<bool>;
}

/// How many events one key may cause in a window before further ones are refused.
#[derive(Debug, Clone)]
pub struct RateLimits {
    /// Failed sign-ins per user name, existing or not, in 15 minutes.
    pub sign_in_per_account: u32,
    /// Failed sign-ins per client address in 15 minutes.
    pub sign_in_per_client: u32,
    /// Sign-up attempts per client address in an hour.
    pub register_per_client: u32,
    /// Verification emails per recipient address in an hour.
    pub mail_per_email: u32,
}

impl Default for RateLimits {
    fn default() -> Self {
        Self {
            sign_in_per_account: 5,
            sign_in_per_client: 30,
            register_per_client: 10,
            mail_per_email: 3,
        }
    }
}

const SIGN_IN_WINDOW: Duration = Duration::minutes(15);
const REGISTER_WINDOW: Duration = Duration::hours(1);
const MAIL_WINDOW: Duration = Duration::hours(1);

/// Server settings that shape who can join and how long a sign-in lasts.
#[derive(Debug, Clone)]
pub struct AccessConfig {
    pub registration: RegistrationMode,
    pub session_days: i64,
    /// Open registration only: an account stays inactive until its email address is confirmed.
    pub require_email_verification: bool,
    /// Open registration only: an administrator approves each new account after it is verified.
    pub require_approval: bool,
    pub limits: RateLimits,
    /// Where the web app lives; verification links point at `{public_url}/verify-email`.
    pub public_url: String,
    /// How long a verification link works, and how long an unverified account keeps its name.
    pub verification_ttl: Duration,
    /// Who may create organizations.
    pub org_creation: OrgCreation,
}

impl Default for AccessConfig {
    fn default() -> Self {
        Self {
            registration: RegistrationMode::InviteOnly,
            session_days: 30,
            require_email_verification: true,
            require_approval: false,
            limits: RateLimits::default(),
            public_url: "http://localhost:5173".into(),
            verification_ttl: Duration::hours(24),
            org_creation: OrgCreation::Anyone,
        }
    }
}

impl AccessConfig {
    /// What to tell the operator about an open server that lacks protection; empty for other modes.
    pub fn open_registration_warnings(&self, email_delivers: bool) -> Vec<String> {
        if self.registration != RegistrationMode::Open {
            return Vec::new();
        }
        let mut out = vec![
            "registration is open: anyone who can reach this server can create an account"
                .to_string(),
        ];
        if !self.require_email_verification {
            out.push("email verification is off: sign-ups are not checked, so bots can create accounts freely".into());
        } else if !email_delivers {
            out.push("email verification is on but no email is delivered (verification links only reach the log): real people cannot finish signing up".into());
        }
        out
    }
}

/// A request to create an account for oneself.
#[derive(Debug, Clone, Copy)]
pub struct Registration<'a> {
    pub username: &'a str,
    pub password: &'a str,
    pub email: Option<&'a str>,
    pub invite: Option<&'a str>,
    /// The caller's network address, for rate limits.
    pub client: Option<&'a str>,
}

impl<'a> Registration<'a> {
    pub fn new(username: &'a str, password: &'a str) -> Self {
        Self {
            username,
            password,
            email: None,
            invite: None,
            client: None,
        }
    }

    pub fn email(mut self, email: &'a str) -> Self {
        self.email = Some(email);
        self
    }

    pub fn invite(mut self, code: &'a str) -> Self {
        self.invite = Some(code);
        self
    }

    pub fn client(mut self, address: &'a str) -> Self {
        self.client = Some(address);
        self
    }
}

/// The outcome of a sign-up: the account asked for, and what it must still do before it is usable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignUp {
    pub user: UserId,
    pub status: AccountStatus,
}

/// A person with access to a repository, the role they effectively hold and where it comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoMember {
    pub user: UserId,
    pub role: Role,
    pub source: RoleSource,
}

/// Authentication of tokens and passwords, and every rule about who may join and manage users, roles and tokens.
pub struct AccessService {
    store: Arc<dyn AccessStore>,
    clock: Arc<dyn Clock>,
    config: AccessConfig,
    audit: Option<Arc<dyn AuditStore>>,
    limits: Arc<dyn RateLimitStore>,
    email: Arc<dyn EmailSender>,
    passwords: Arc<dyn PasswordWorker>,
    registry: Option<Arc<dyn MetadataStore>>,
    setup: setup::SetupState,
}

fn stronger(
    current: Option<(Role, RoleSource)>,
    candidate: (Role, RoleSource),
) -> Option<(Role, RoleSource)> {
    match current {
        Some((role, source))
            if (role, std::cmp::Reverse(source))
                >= (candidate.0, std::cmp::Reverse(candidate.1)) =>
        {
            current
        }
        _ => Some(candidate),
    }
}

fn join<T: ToString>(items: impl Iterator<Item = T>) -> String {
    items.map(|i| i.to_string()).collect::<Vec<_>>().join(", ")
}

fn org_cannot_sign_in() -> PynError {
    unauthenticated("organizations cannot sign in; act as one of their owners")
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
            audit: None,
            limits: Arc::new(MemoryRateLimitStore::new()),
            email: Arc::new(NullEmailSender),
            passwords: Arc::new(InlinePasswords),
            registry: None,
            setup: setup::SetupState::default(),
        }
    }

    /// The repository registry, which tells who owns a repository so organization owners resolve to admin.
    pub fn with_registry(mut self, registry: Arc<dyn MetadataStore>) -> Self {
        self.registry = Some(registry);
        self
    }

    /// Where rate-limit counters live; the default is in memory and resets on restart.
    pub fn with_rate_limits(mut self, store: Arc<dyn RateLimitStore>) -> Self {
        self.limits = store;
        self
    }

    pub fn with_email(mut self, sender: Arc<dyn EmailSender>) -> Self {
        self.email = sender;
        self
    }

    /// Where passwords are hashed and checked; the default runs on the calling task.
    pub fn with_passwords(mut self, worker: Arc<dyn PasswordWorker>) -> Self {
        self.passwords = worker;
        self
    }

    /// Warnings for the operator about how this server is set up.
    pub fn startup_warnings(&self) -> Vec<String> {
        self.config
            .open_registration_warnings(self.email.delivers())
    }

    pub fn with_config(mut self, config: AccessConfig) -> Self {
        self.config = config;
        self
    }

    /// Records member, role and token changes in `store`, each under the repository it concerns.
    pub fn with_audit(mut self, store: Arc<dyn AuditStore>) -> Self {
        self.audit = Some(store);
        self
    }

    async fn record(
        &self,
        scope: impl Into<AuditScope>,
        actor: &UserId,
        action: AuditAction,
        detail: String,
    ) -> Result<()> {
        let Some(store) = &self.audit else {
            return Ok(());
        };
        let event = NewAuditEvent {
            at: self.clock.now(),
            actor: actor.clone(),
            action,
            path: None,
            detail,
        };
        store.record(&scope.into(), event).await
    }

    pub fn registration_mode(&self) -> RegistrationMode {
        self.setup
            .settings()
            .and_then(|s| s.registration)
            .unwrap_or(self.config.registration)
    }

    fn public_url(&self) -> String {
        self.setup
            .settings()
            .and_then(|s| s.public_url)
            .unwrap_or_else(|| self.config.public_url.clone())
    }

    pub fn requires_email_verification(&self) -> bool {
        self.config.require_email_verification
    }

    pub fn requires_approval(&self) -> bool {
        self.config.require_approval
    }

    /// The role `user` holds in `repo`: the highest of a direct grant, the grants of their teams and the implicit
    /// admin of an owner of the owning organization. Every role lookup goes through here.
    pub async fn effective_role(&self, repo: &RepoId, user: &UserId) -> Result<Option<Role>> {
        Ok(self.resolve_role(repo, user).await?.map(|(role, _)| role))
    }

    /// The effective role with the source that decides it; on a tie, direct beats team beats owner.
    async fn resolve_role(
        &self,
        repo: &RepoId,
        user: &UserId,
    ) -> Result<Option<(Role, RoleSource)>> {
        let mut best = None;
        if let Some(role) = self.store.role_of(repo, user).await? {
            best = stronger(best, (role, RoleSource::Direct));
        }
        if best.is_some_and(|(role, _)| role == Role::Admin) {
            return Ok(best);
        }
        if let Some(role) = self.store.team_role_of(repo, user).await? {
            best = stronger(best, (role, RoleSource::Team));
        }
        let Some(registry) = &self.registry else {
            return Ok(best);
        };
        let Some(record) = registry.get_repo(repo).await? else {
            return Ok(best);
        };
        if self.store.org_role(&record.owner, user).await? == Some(OrgRole::Owner) {
            best = stronger(best, (Role::Admin, RoleSource::OrgOwner));
        }
        Ok(best)
    }

    /// What `user`'s role grants in `repo`; nothing for a non-member.
    pub async fn role_permissions(
        &self,
        repo: &RepoId,
        user: &UserId,
    ) -> Result<BTreeSet<Permission>> {
        let Some(role) = self.effective_role(repo, user).await? else {
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

    /// Checks a bearer token and names its owner; the token's limits apply once a repository is chosen.
    pub async fn identify(&self, raw: &str) -> Result<Identity> {
        if service_credential::is_service(raw) {
            return self.identify_service(raw).await;
        }
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
        self.require_active(&record.user).await?;
        self.store.touch_token(&id, now).await?;
        Ok(Identity {
            user: record.user.clone(),
            credential: Credential::Token(record),
        })
    }

    async fn identify_service(&self, raw: &str) -> Result<Identity> {
        let (id, secret) = service_credential::parse(raw)
            .ok_or_else(|| unauthenticated("malformed credential"))?;
        let record = self
            .store
            .get_service_credential(&id)
            .await?
            .ok_or_else(|| unauthenticated("unknown credential"))?;
        if !token::hashes_match(&record.secret_hash, &token::hash_secret(secret)) {
            return Err(unauthenticated("unknown credential"));
        }
        if record.revoked_at.is_some() {
            return Err(unauthenticated("credential revoked"));
        }
        self.store
            .touch_service_credential(&id, self.clock.now())
            .await?;
        Ok(Identity {
            user: record.actor(),
            credential: Credential::Service(record),
        })
    }

    /// What the identity may do in `repo`: its role, further limited by a token's permissions and repositories.
    pub async fn principal_in(&self, repo: &RepoId, who: &Identity) -> Result<Principal> {
        match &who.credential {
            Credential::Unrestricted => Ok(Principal::unrestricted(who.user.clone())),
            Credential::Session => self.principal(repo, &who.user).await,
            Credential::Service(_) => Ok(Principal {
                user: who.user.clone(),
                permissions: BTreeSet::new(),
            }),
            Credential::Token(record) => {
                if !record.repos.is_empty() && !record.repos.contains(repo) {
                    return Err(unauthenticated("token is not valid for this repository"));
                }
                let role = self.role_permissions(repo, &who.user).await?;
                Ok(Principal {
                    user: who.user.clone(),
                    permissions: record.permissions.intersection(&role).copied().collect(),
                })
            }
        }
    }

    /// Resolves a bearer token to a principal limited to what both the token and the user's role allow in `repo`.
    pub async fn authenticate(&self, repo: &RepoId, raw: &str) -> Result<Principal> {
        let who = self.identify(raw).await?;
        self.principal_in(repo, &who).await
    }

    /// Creates a token for the actor, scoped to `repos`. It can hold only permissions the actor holds in each of
    /// them. Returns the record and the full token string, which is not stored and cannot be shown again.
    pub async fn create_token(
        &self,
        actor: &Identity,
        name: &str,
        permissions: BTreeSet<Permission>,
        repos: Vec<RepoId>,
        expires_at: Option<DateTime<Utc>>,
    ) -> Result<(TokenRecord, String)> {
        if repos.is_empty() {
            return Err(PynError::InvalidRequest(
                "a token needs at least one repository".into(),
            ));
        }
        for repo in &repos {
            let held = self.principal_in(repo, actor).await?;
            for p in &permissions {
                held.require(*p)?;
            }
        }
        let (record, full) = self
            .issue_token(&actor.user, name, permissions, repos, expires_at)
            .await?;
        let detail = format!(
            "token {} {:?} for {}: permissions [{}], repos [{}], expires {}",
            record.id,
            record.name,
            record.user,
            join(record.permissions.iter()),
            join(record.repos.iter()),
            record
                .expires_at
                .map_or("never".to_string(), |t| t.to_rfc3339()),
        );
        for repo in &record.repos {
            self.record(repo, &actor.user, AuditAction::TokenCreated, detail.clone())
                .await?;
        }
        Ok((record, full))
    }

    /// Mints a token without an audit event, for sign-in sessions. An empty `repos`
    /// means every repository; the owner's role still caps what it can do in each.
    async fn issue_token(
        &self,
        user: &UserId,
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
        let now = self.clock.now();
        if expires_at.is_some_and(|t| t <= now) {
            return Err(PynError::InvalidRequest(
                "expiry must be in the future".into(),
            ));
        }
        let (id, full, secret_hash) = token::generate()?;
        let record = TokenRecord {
            id,
            user: user.clone(),
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

    /// Allows the actor to act on `target`'s account: always for themselves, otherwise only with `manage_users`
    /// in a repository the target belongs to.
    async fn require_manages(&self, actor: &Identity, target: &UserId) -> Result<()> {
        if &actor.user == target {
            return Ok(());
        }
        for repo in self.repos_of(target).await? {
            if let Ok(held) = self.principal_in(&repo, actor).await
                && held.has(Permission::ManageUsers)
            {
                return Ok(());
            }
        }
        Err(PynError::Forbidden(Permission::ManageUsers))
    }

    /// The actor's own tokens; another user's need `manage_users` in a repository that user belongs to.
    pub async fn list_tokens(&self, actor: &Identity, user: &UserId) -> Result<Vec<TokenRecord>> {
        self.require_manages(actor, user).await?;
        self.store.list_tokens(user).await
    }

    /// Revokes the actor's own token; another user's needs `manage_users` in a repository that user belongs to.
    pub async fn revoke_token(&self, actor: &Identity, id: &TokenId) -> Result<()> {
        let record = self
            .store
            .get_token(id)
            .await?
            .ok_or_else(|| PynError::TokenNotFound(id.to_string()))?;
        self.require_manages(actor, &record.user).await?;
        self.store.revoke_token(id, self.clock.now()).await?;
        let detail = format!("token {} {:?} of {}", record.id, record.name, record.user);
        for repo in &record.repos {
            self.record(repo, &actor.user, AuditAction::TokenRevoked, detail.clone())
                .await?;
        }
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
        self.require_grantable(repo, user).await?;
        let before = self.store.role_of(repo, user).await?;
        self.store.ensure_user(user, self.clock.now()).await?;
        self.store.set_role(repo, user, role).await?;
        self.record_role(repo, &actor.user, user, before, role)
            .await
    }

    async fn record_role(
        &self,
        repo: &RepoId,
        actor: &UserId,
        user: &UserId,
        before: Option<Role>,
        role: Role,
    ) -> Result<()> {
        match before {
            None => {
                let detail = format!("{user} added as {role}");
                self.record(repo, actor, AuditAction::MemberAdded, detail)
                    .await
            }
            Some(old) if old != role => {
                let detail = format!("{user}: {old} -> {role}");
                self.record(repo, actor, AuditAction::RoleChanged, detail)
                    .await
            }
            Some(_) => Ok(()),
        }
    }

    /// Everyone with a role in the repository, ordered by user name: direct grants, members of granted teams and
    /// owners of the owning organization, each with the effective role and its source.
    pub async fn members(&self, actor: &Principal, repo: &RepoId) -> Result<Vec<RepoMember>> {
        actor.require(Permission::ManageUsers)?;
        let mut best: BTreeMap<UserId, (Role, RoleSource)> = BTreeMap::new();
        let mut add = |user: UserId, candidate: (Role, RoleSource)| {
            let current = best.get(&user).copied();
            if let Some(winner) = stronger(current, candidate) {
                best.insert(user, winner);
            }
        };
        for (user, role) in self.store.members(repo).await? {
            add(user, (role, RoleSource::Direct));
        }
        let owner = match &self.registry {
            Some(registry) => registry.get_repo(repo).await?.map(|r| r.owner),
            None => None,
        };
        if let Some(org) = owner {
            for (slug, role) in self.store.team_grants(repo).await? {
                for user in self.store.team_members(&org, &slug).await? {
                    add(user, (role, RoleSource::Team));
                }
            }
            for (user, role) in self.store.org_members(&org).await? {
                if role == OrgRole::Owner {
                    add(user, (Role::Admin, RoleSource::OrgOwner));
                }
            }
        }
        Ok(best
            .into_iter()
            .map(|(user, (role, source))| RepoMember { user, role, source })
            .collect())
    }

    /// What each role grants. Members only: reading a public repository is not membership.
    pub async fn role_definitions(
        &self,
        actor: &Principal,
        repo: &RepoId,
        address: &str,
    ) -> Result<RoleDefinitions> {
        if !actor.has(Permission::ManageRoles) && self.role_in(repo, &actor.user).await?.is_none() {
            return Err(PynError::NotRepoMember(address.to_string()));
        }
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
        let before = self.store.role_definitions(repo).await?.get(role).clone();
        self.store
            .set_role_permissions(repo, role, permissions.clone())
            .await?;
        if before == permissions {
            return Ok(());
        }
        let detail = format!(
            "{role}: added [{}], removed [{}]",
            join(permissions.difference(&before)),
            join(before.difference(&permissions)),
        );
        self.record(
            repo,
            &actor.user,
            AuditAction::RolePermissionsChanged,
            detail,
        )
        .await
    }

    /// Refuses when `key` has already reached `max` events in its live window.
    async fn gate(&self, key: &str, max: u32) -> Result<()> {
        let now = self.clock.now();
        match self.limits.state(key, now).await? {
            Some(state) if state.count >= max => Err(PynError::TooManyAttempts {
                retry_after_secs: (state.resets_at - now).num_seconds().max(1),
            }),
            _ => Ok(()),
        }
    }

    /// Counts an event against `key` and refuses it once the window already holds `max`.
    async fn count_limited(&self, key: &str, max: u32, window: Duration) -> Result<()> {
        let now = self.clock.now();
        let state = self.limits.hit(key, window, now).await?;
        if state.count > max {
            return Err(PynError::TooManyAttempts {
                retry_after_secs: (state.resets_at - now).num_seconds().max(1),
            });
        }
        Ok(())
    }

    /// Refuses unless the account is active; an account with no record is a development identity.
    async fn require_active(&self, user: &UserId) -> Result<()> {
        match self.store.account(user).await? {
            Some(account) if account.kind == AccountKind::Org => Err(org_cannot_sign_in()),
            Some(account) if account.status() != AccountStatus::Active => {
                Err(PynError::AccountInactive(account.status()))
            }
            _ => Ok(()),
        }
    }

    /// Refuses an organization name as a signed-in identity, for credentials that skip the account store.
    pub async fn reject_org(&self, user: &UserId) -> Result<()> {
        match self.store.account(user).await? {
            Some(account) if account.kind == AccountKind::Org => Err(org_cannot_sign_in()),
            _ => Ok(()),
        }
    }

    /// A hash to check against when the user does not exist, so both cases cost the same.
    async fn dummy_hash(&self) -> String {
        static DUMMY: OnceLock<String> = OnceLock::new();
        if let Some(hash) = DUMMY.get() {
            return hash.clone();
        }
        let hash = self
            .passwords
            .hash("not-a-real-password")
            .await
            .unwrap_or_default();
        DUMMY.get_or_init(|| hash).clone()
    }

    /// Checks a password. Repeated failures lock the user name, and the client address, out for a while.
    async fn verify_sign_in(
        &self,
        username: &str,
        password: &str,
        client: Option<&str>,
    ) -> Result<UserId> {
        let limits = &self.config.limits;
        let account_key = format!("sign-in:account:{}", username.to_lowercase());
        let client_key = client.map(|c| format!("sign-in:client:{c}"));
        self.gate(&account_key, limits.sign_in_per_account).await?;
        if let Some(key) = &client_key {
            self.gate(key, limits.sign_in_per_client).await?;
        }

        let user = UserId::new(username);
        let verified = match self.store.password_hash(&user).await? {
            Some(hash) => self.passwords.verify(password, &hash).await,
            None => {
                self.passwords
                    .verify(password, &self.dummy_hash().await)
                    .await;
                false
            }
        };
        let now = self.clock.now();
        if !verified {
            self.limits.hit(&account_key, SIGN_IN_WINDOW, now).await?;
            if let Some(key) = &client_key {
                self.limits.hit(key, SIGN_IN_WINDOW, now).await?;
            }
            return Err(PynError::Unauthenticated(
                "wrong user name or password".into(),
            ));
        }
        self.limits.reset(&account_key).await?;
        self.require_active(&user).await?;
        Ok(user)
    }

    /// Checks a password and returns a token for every repository the user can reach, capped by their roles at
    /// use time, that expires after the configured number of days.
    pub async fn login(
        &self,
        username: &str,
        password: &str,
        client: Option<&str>,
    ) -> Result<(TokenRecord, String)> {
        let user = self.verify_sign_in(username, password, client).await?;
        let expires = self.clock.now() + Duration::days(self.config.session_days);
        self.issue_token(
            &user,
            "sign-in",
            Permission::ALL.into(),
            Vec::new(),
            Some(expires),
        )
        .await
    }

    /// Checks a password and starts a web session. Returns the record and the cookie value, which is not stored.
    pub async fn start_session(
        &self,
        username: &str,
        password: &str,
        client: Option<&str>,
    ) -> Result<(SessionRecord, String)> {
        let user = self.verify_sign_in(username, password, client).await?;
        let now = self.clock.now();
        self.store.delete_expired_sessions(now).await?;
        let (cookie, csrf_token, id_hash) = session::generate()?;
        let record = SessionRecord {
            id_hash,
            user,
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

    /// Resolves a session cookie to its account; roles are read per repository on each request, so a change applies at once.
    pub async fn authenticate_session(&self, cookie: &str) -> Result<(Identity, SessionRecord)> {
        let record = self
            .find_session(cookie)
            .await?
            .ok_or_else(|| unauthenticated("not signed in"))?;
        self.require_active(&record.user).await?;
        let who = Identity {
            user: record.user.clone(),
            credential: Credential::Session,
        };
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

    /// Creates an account on the server's terms: open (after any verification and approval), with an invitation
    /// that also gives its role, or never when closed.
    pub async fn register(&self, request: Registration<'_>) -> Result<SignUp> {
        let user = account::validate_username(request.username)?;
        account::validate_password(request.password)?;
        let open = match self.registration_mode() {
            RegistrationMode::Closed => return Err(PynError::RegistrationClosed),
            RegistrationMode::Open => true,
            RegistrationMode::InviteOnly => false,
        };
        if let Some(client) = request.client {
            self.count_limited(
                &format!("register:client:{client}"),
                self.config.limits.register_per_client,
                REGISTER_WINDOW,
            )
            .await?;
        }
        let invite = if open {
            None
        } else {
            let invite = self.check_invitation(request.invite).await?;
            self.require_grantable(&invite.repo, &user).await?;
            Some(invite)
        };
        let email = request.email.map(account::normalize_email).transpose()?;
        let verify = open && self.config.require_email_verification;
        if verify && email.is_none() {
            return Err(PynError::InvalidRequest(
                "an email address is required to sign up on this server".into(),
            ));
        }
        let now = self.clock.now();
        self.store
            .delete_stale_pending(now, now - self.config.verification_ttl)
            .await?;
        self.limits.sweep(now).await?;
        if self.store.user_exists(&user).await? {
            return Err(PynError::UserExists(user));
        }
        let password_hash = self.passwords.hash(request.password).await?;

        if verify {
            let email = email.expect("checked above");
            return self.register_unverified(user, email, password_hash).await;
        }
        if let Some(invite) = &invite
            && !self.store.use_invite(&invite.id, &user, now).await?
        {
            return Err(PynError::InvalidInvite("it has already been used".into()));
        }
        let signup = if open && self.config.require_approval {
            SignupStage::PendingApproval
        } else {
            SignupStage::Complete
        };
        let new = NewAccount {
            user: user.clone(),
            email,
            password_hash,
            signup,
            created_at: now,
        };
        if !self.store.create_account(new).await? {
            return Err(PynError::UserExists(user));
        }
        if let Some(invite) = invite {
            self.store
                .set_role(&invite.repo, &user, invite.role)
                .await?;
            let detail = format!("{user} registered as {}", invite.role);
            self.record(&invite.repo, &user, AuditAction::MemberAdded, detail)
                .await?;
        }
        let status = match signup {
            SignupStage::PendingApproval => AccountStatus::PendingApproval,
            _ => AccountStatus::Active,
        };
        Ok(SignUp { user, status })
    }

    /// Open sign-up with email verification. An address that already belongs to a verified account gets a
    /// notice instead and the caller sees the same answer, so the response never says which addresses are taken.
    async fn register_unverified(
        &self,
        user: UserId,
        email: String,
        password_hash: String,
    ) -> Result<SignUp> {
        let signup = SignUp {
            user: user.clone(),
            status: AccountStatus::PendingVerification,
        };
        let mail_allowed = self
            .count_limited(
                &format!("mail:{email}"),
                self.config.limits.mail_per_email,
                MAIL_WINDOW,
            )
            .await
            .is_ok();
        if self.store.verified_email_owner(&email).await?.is_some() {
            if mail_allowed {
                self.email
                    .send(EmailMessage {
                        to: email,
                        subject: "Someone tried to sign up with your address".into(),
                        body: "A sign-up was started with this email address, but it already belongs to an \
                               account on this server. If that was you, sign in instead. Otherwise, ignore \
                               this message."
                            .into(),
                    })
                    .await?;
            }
            return Ok(signup);
        }
        let new = NewAccount {
            user: user.clone(),
            email: Some(email.clone()),
            password_hash,
            signup: SignupStage::PendingVerification,
            created_at: self.clock.now(),
        };
        if !self.store.create_account(new).await? {
            return Err(PynError::UserExists(user));
        }
        if mail_allowed {
            self.send_verification(&user, &email).await?;
        }
        Ok(signup)
    }

    async fn send_verification(&self, user: &UserId, email: &str) -> Result<()> {
        let (raw, token_hash) = verification::generate()?;
        self.store
            .put_verification(VerificationRecord {
                token_hash,
                user: user.clone(),
                email: email.to_string(),
                expires_at: self.clock.now() + self.config.verification_ttl,
            })
            .await?;
        let link = format!(
            "{}/verify-email?token={raw}",
            self.public_url().trim_end_matches('/')
        );
        self.email
            .send(EmailMessage {
                to: email.to_string(),
                subject: "Confirm your email address".into(),
                body: format!(
                    "Finish creating the account {user} by opening this link:\n\n{link}\n\n\
                     It works once and expires in {} hours. If you did not sign up, ignore this message.",
                    self.config.verification_ttl.num_hours()
                ),
            })
            .await
    }

    /// Confirms an address from the token in its email. The account then becomes active, or waits for
    /// approval. Every failure looks the same.
    pub async fn verify_email(&self, raw_token: &str) -> Result<SignUp> {
        let bad = |why: &str| PynError::InvalidVerification(why.to_string());
        let record = self
            .store
            .take_verification(&verification::hash(raw_token))
            .await?
            .ok_or_else(|| bad("the link is not valid or has already been used"))?;
        if record.expires_at <= self.clock.now() {
            return Err(bad("the link has expired; ask for a new one"));
        }
        let account = self
            .store
            .account(&record.user)
            .await?
            .filter(|a| a.signup == SignupStage::PendingVerification)
            .ok_or_else(|| bad("the link is not valid or has already been used"))?;
        let next = if self.config.require_approval {
            SignupStage::PendingApproval
        } else {
            SignupStage::Complete
        };
        if !self
            .store
            .complete_verification(&account.user, next, self.clock.now())
            .await?
        {
            return Err(bad("that address already belongs to another account"));
        }
        let status = match next {
            SignupStage::PendingApproval => AccountStatus::PendingApproval,
            _ => AccountStatus::Active,
        };
        Ok(SignUp {
            user: account.user,
            status,
        })
    }

    /// Sends a fresh verification email to accounts still waiting on `email`. Always succeeds from the
    /// caller's point of view, whether or not anything was sent.
    pub async fn resend_verification(&self, email: &str, client: Option<&str>) -> Result<()> {
        if let Some(client) = client {
            self.count_limited(
                &format!("register:client:{client}"),
                self.config.limits.register_per_client,
                REGISTER_WINDOW,
            )
            .await?;
        }
        let Ok(email) = account::normalize_email(email) else {
            return Ok(());
        };
        if self.registration_mode() != RegistrationMode::Open
            || !self.config.require_email_verification
        {
            return Ok(());
        }
        let pending = self.store.pending_by_email(&email).await?;
        if pending.is_empty() {
            return Ok(());
        }
        let allowed = self
            .count_limited(
                &format!("mail:{email}"),
                self.config.limits.mail_per_email,
                MAIL_WINDOW,
            )
            .await
            .is_ok();
        if allowed {
            for account in pending {
                self.send_verification(&account.user, &email).await?;
            }
        }
        Ok(())
    }

    async fn check_invitation(&self, code: Option<&str>) -> Result<InviteRecord> {
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
        self.require_grantable(repo, &user).await?;
        let hash = self.passwords.hash(password).await?;
        if !self.store.create_user(&user, self.clock.now()).await? {
            return Err(PynError::UserExists(user));
        }
        self.store.set_password_hash(&user, &hash).await?;
        self.store.set_role(repo, &user, role).await?;
        let detail = format!("{user} added as {role}");
        self.record(repo, &actor.user, AuditAction::MemberAdded, detail)
            .await?;
        Ok(user)
    }

    /// Changes the actor's own password. If they already have one, the current password must be given.
    pub async fn change_password(
        &self,
        actor: &Identity,
        current: Option<&str>,
        new: &str,
    ) -> Result<()> {
        account::validate_password(new)?;
        if let Some(hash) = self.store.password_hash(&actor.user).await? {
            let ok = match current {
                Some(current) => self.passwords.verify(current, &hash).await,
                None => false,
            };
            if !ok {
                return Err(PynError::Unauthenticated(
                    "the current password is wrong".into(),
                ));
            }
        }
        let hash = self.passwords.hash(new).await?;
        self.store.set_password_hash(&actor.user, &hash).await
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
        if self.is_org_repo(repo).await? {
            return Err(PynError::InvalidInvite(
                "an organization's repository cannot be shared by invitation; add the person to the organization first"
                    .into(),
            ));
        }
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

    pub async fn revoke_invite(
        &self,
        actor: &Principal,
        repo: &RepoId,
        id: &InviteId,
    ) -> Result<()> {
        actor.require(Permission::ManageUsers)?;
        let no_such = || PynError::InvalidInvite("there is no such invitation".into());
        match self.store.get_invite(id).await? {
            Some(invite) if &invite.repo == repo => {}
            _ => return Err(no_such()),
        }
        if self.store.revoke_invite(id, self.clock.now()).await? {
            Ok(())
        } else {
            Err(no_such())
        }
    }

    /// Links a public key to the actor's account. The title defaults to the key's own comment.
    pub async fn add_ssh_key(
        &self,
        actor: &Identity,
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

    /// The actor's own keys; another user's need `manage_users` in a repository that user belongs to.
    pub async fn list_ssh_keys(
        &self,
        actor: &Identity,
        user: &UserId,
    ) -> Result<Vec<SshKeyRecord>> {
        self.require_manages(actor, user).await?;
        self.store.list_ssh_keys(user).await
    }

    pub async fn delete_ssh_key(&self, actor: &Identity, user: &UserId, id: &str) -> Result<()> {
        self.require_manages(actor, user).await?;
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
        self.require_active(&record.user).await?;
        self.store
            .touch_ssh_key(fingerprint, self.clock.now())
            .await?;
        self.principal(repo, &record.user).await
    }

    /// Gives an existing user a password without needing the old one, for the person running the server.
    pub async fn set_password_for_operator(&self, user: &UserId, password: &str) -> Result<()> {
        account::validate_password(password)?;
        let hash = self.passwords.hash(password).await?;
        self.store.set_password_hash(user, &hash).await
    }

    /// Creates a server administrator and a token reaching every repository it belongs to, skipping setup.
    #[cfg(feature = "test-support")]
    pub async fn admin_for_tests(&self, user: &UserId) -> Result<String> {
        self.store.ensure_user(user, self.clock.now()).await?;
        self.store.set_admin(user, true).await?;
        let (_, full) = self
            .issue_token(user, "test", Permission::ALL.into(), Vec::new(), None)
            .await?;
        Ok(full)
    }

    /// Makes the user the admin of a repository they just created.
    pub async fn add_creator(&self, repo: &RepoId, user: &UserId) -> Result<()> {
        self.store.ensure_user(user, self.clock.now()).await?;
        self.store.set_role(repo, user, Role::Admin).await?;
        let detail = format!("{user} added as admin (creator)");
        self.record(repo, user, AuditAction::MemberAdded, detail)
            .await
    }

    /// The repositories where the user has a role: direct grants, team grants and every repository owned by an
    /// organization they own.
    pub async fn repos_of(&self, user: &UserId) -> Result<Vec<RepoId>> {
        let mut repos = self.store.repos_of(user).await?;
        repos.extend(self.store.team_repos_of(user).await?);
        if let Some(registry) = &self.registry {
            for (org, role) in self.store.orgs_of(user).await? {
                if role == OrgRole::Owner {
                    let owned = registry.list_repos(Some(&org)).await?;
                    repos.extend(owned.into_iter().map(|r| r.id));
                }
            }
        }
        repos.sort();
        repos.dedup();
        Ok(repos)
    }

    /// The user's role in the repository, if any.
    pub async fn role_in(&self, repo: &RepoId, user: &UserId) -> Result<Option<Role>> {
        self.effective_role(repo, user).await
    }

    /// Removes a deleted repository's memberships, role overrides and invitations. Its audit log stays.
    pub async fn forget_repo(&self, repo: &RepoId) -> Result<()> {
        self.store.delete_repo_access(repo).await
    }

    /// Whether the identity runs the server. The development identity does; a token needs `manage_users`.
    pub async fn is_server_admin(&self, who: &Identity) -> Result<bool> {
        match &who.credential {
            Credential::Unrestricted => return Ok(true),
            Credential::Service(_) => return Ok(false),
            Credential::Token(t) if !t.permissions.contains(&Permission::ManageUsers) => {
                return Ok(false);
            }
            _ => {}
        }
        Ok(self
            .store
            .account(&who.user)
            .await?
            .is_some_and(|a| a.is_admin))
    }

    async fn require_server_admin(&self, who: &Identity) -> Result<()> {
        if self.is_server_admin(who).await? {
            Ok(())
        } else {
            Err(PynError::ServerAdminRequired)
        }
    }

    /// Server administrators pass; a service credential passes only with `scope`.
    async fn require_admin_scope(&self, who: &Identity, scope: ServiceScope) -> Result<()> {
        match &who.credential {
            Credential::Service(c) if c.scopes.contains(&scope) => Ok(()),
            Credential::Service(_) => Err(PynError::ServiceScopeRequired(scope)),
            _ => self.require_server_admin(who).await,
        }
    }

    async fn record_server(
        &self,
        actor: &UserId,
        action: AuditAction,
        detail: String,
    ) -> Result<()> {
        self.record(AuditScope::Server, actor, action, detail).await
    }

    /// Accounts oldest first, optionally only those in `status`. Server administrators only.
    pub async fn list_accounts(
        &self,
        actor: &Identity,
        status: Option<AccountStatus>,
        limit: usize,
    ) -> Result<Vec<AccountRecord>> {
        self.require_admin_scope(actor, ServiceScope::ManageAccounts)
            .await?;
        self.store.list_accounts(status, limit).await
    }

    async fn existing_account(&self, user: &UserId) -> Result<AccountRecord> {
        self.store
            .account(user)
            .await?
            .filter(|a| a.kind == AccountKind::User)
            .ok_or_else(|| PynError::UserNotFound(user.to_string()))
    }

    /// Lets a verified account that is waiting for approval in. Server administrators only.
    pub async fn approve_account(&self, actor: &Identity, user: &UserId) -> Result<AccountRecord> {
        self.require_admin_scope(actor, ServiceScope::ManageAccounts)
            .await?;
        let account = self.existing_account(user).await?;
        if account.signup != SignupStage::PendingApproval {
            return Err(PynError::InvalidRequest(
                "that account is not waiting for approval".into(),
            ));
        }
        self.store.set_signup(user, SignupStage::Complete).await?;
        self.record_server(
            &actor.user,
            AuditAction::AccountApproved,
            format!("{user} approved"),
        )
        .await?;
        self.existing_account(user).await
    }

    /// Blocks the account from signing in and ends its sessions; its tokens and keys stop working too.
    /// Server administrators only, and never one's own account.
    pub async fn disable_account(
        &self,
        actor: &Identity,
        user: &UserId,
        reason: Option<&str>,
    ) -> Result<AccountRecord> {
        self.require_admin_scope(actor, ServiceScope::ManageAccounts)
            .await?;
        if &actor.user == user {
            return Err(PynError::InvalidRequest(
                "you cannot disable your own account".into(),
            ));
        }
        self.existing_account(user).await?;
        let reason = reason
            .map(str::trim)
            .filter(|r| !r.is_empty())
            .map(str::to_string);
        self.store
            .set_disabled(user, Some((self.clock.now(), reason.clone())))
            .await?;
        self.store.delete_sessions_of(user).await?;
        let detail = match &reason {
            Some(reason) => format!("{user} disabled: {reason}"),
            None => format!("{user} disabled"),
        };
        self.record_server(&actor.user, AuditAction::AccountDisabled, detail)
            .await?;
        self.existing_account(user).await
    }

    /// Lifts a disable. Server administrators only.
    pub async fn enable_account(&self, actor: &Identity, user: &UserId) -> Result<AccountRecord> {
        self.require_admin_scope(actor, ServiceScope::ManageAccounts)
            .await?;
        let account = self.existing_account(user).await?;
        if account.disabled_at.is_none() {
            return Err(PynError::InvalidRequest(
                "that account is not disabled".into(),
            ));
        }
        self.store.set_disabled(user, None).await?;
        self.record_server(
            &actor.user,
            AuditAction::AccountEnabled,
            format!("{user} enabled"),
        )
        .await?;
        self.existing_account(user).await
    }

    /// Makes an active account a server administrator. Server administrators only; granting again changes nothing.
    pub async fn grant_admin(&self, actor: &Identity, user: &UserId) -> Result<AccountRecord> {
        self.require_server_admin(actor).await?;
        let account = self.existing_account(user).await?;
        if account.is_admin {
            return Ok(account);
        }
        if account.status() != AccountStatus::Active {
            return Err(PynError::InvalidRequest(
                "only an active account can become an administrator".into(),
            ));
        }
        self.store.set_admin(user, true).await?;
        self.record_server(
            &actor.user,
            AuditAction::AdminGranted,
            format!("{user} made an administrator"),
        )
        .await?;
        self.existing_account(user).await
    }

    /// Removes the administrator flag, one's own included. The last active administrator cannot be removed.
    /// Server administrators only; revoking from a non-administrator changes nothing.
    pub async fn revoke_admin(&self, actor: &Identity, user: &UserId) -> Result<AccountRecord> {
        self.require_server_admin(actor).await?;
        self.existing_account(user).await?;
        match self.store.revoke_admin(user).await? {
            AdminRevoke::Revoked => {
                self.record_server(
                    &actor.user,
                    AuditAction::AdminRevoked,
                    format!("{user} no longer an administrator"),
                )
                .await?
            }
            AdminRevoke::NotAdmin => {}
            AdminRevoke::LastAdmin => return Err(PynError::LastServerAdmin),
        }
        self.existing_account(user).await
    }

    /// The server-wide audit log (account approvals and disables), newest first. Server administrators only.
    pub async fn server_audit(
        &self,
        actor: &Identity,
        query: &AuditQuery,
    ) -> Result<Vec<AuditEvent>> {
        self.require_server_admin(actor).await?;
        match &self.audit {
            Some(store) => store.list(&AuditScope::Server, query).await,
            None => Ok(Vec::new()),
        }
    }

    /// Creates a service credential with `scopes`. Server administrators only; the secret is not stored and
    /// cannot be shown again.
    pub async fn create_service_credential(
        &self,
        actor: &Identity,
        name: &str,
        scopes: BTreeSet<ServiceScope>,
    ) -> Result<(ServiceCredentialRecord, String)> {
        self.require_server_admin(actor).await?;
        let name = service_credential::validate_name(name)?;
        if scopes.is_empty() {
            return Err(PynError::InvalidRequest(
                "a service credential needs at least one scope".into(),
            ));
        }
        let (id, full, secret_hash) = service_credential::generate()?;
        let record = ServiceCredentialRecord {
            id,
            name: name.clone(),
            secret_hash,
            scopes,
            created_by: actor.user.clone(),
            created_at: self.clock.now(),
            revoked_at: None,
            last_used_at: None,
        };
        if !self.store.create_service_credential(record.clone()).await? {
            return Err(PynError::ServiceCredentialExists(name));
        }
        let detail = format!(
            "service credential {name} ({}): scopes [{}]",
            record.id,
            join(record.scopes.iter())
        );
        self.record_server(&actor.user, AuditAction::ServiceCredentialCreated, detail)
            .await?;
        Ok((record, full))
    }

    /// Every service credential, oldest first. Server administrators only.
    pub async fn list_service_credentials(
        &self,
        actor: &Identity,
    ) -> Result<Vec<ServiceCredentialRecord>> {
        self.require_server_admin(actor).await?;
        self.store.list_service_credentials().await
    }

    /// Revokes the named credential at once. Server administrators only.
    pub async fn revoke_service_credential(&self, actor: &Identity, name: &str) -> Result<()> {
        self.require_server_admin(actor).await?;
        let now = self.clock.now();
        if !self.store.revoke_service_credential(name, now).await? {
            return Err(PynError::ServiceCredentialNotFound(name.to_string()));
        }
        self.record_server(
            &actor.user,
            AuditAction::ServiceCredentialRevoked,
            format!("service credential {name} revoked"),
        )
        .await
    }
}
