//! The repository registry as a service: who may create, change and delete repositories, and a `RepoService`
//! built on demand for each.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use chrono::Duration;

use crate::access::{Credential, Identity, Permission, Role};
use crate::access_service::AccessService;
use crate::audit::{AuditAction, AuditStore, NewAuditEvent};
use crate::clock::Clock;
use crate::error::{PynError, Result};
use crate::object::ObjectStore;
use crate::repo::{
    self, DEFAULT_MAX_LOCKS_PER_USER, RepoRecord, RepoSettings, RepoUpdate, Visibility,
};
use crate::rules::Rules;
use crate::service::{LockLimit, RepoService, ServiceConfig};
use crate::store::MetadataStore;
use crate::types::UserId;

fn limit_detail(settings: &RepoSettings) -> String {
    settings
        .max_locks
        .map_or(String::new(), |n| format!(", max {n} locks per user"))
}

/// A repository found by address, with the service that enforces its policy.
#[derive(Clone)]
pub struct OpenRepo {
    pub record: RepoRecord,
    pub service: Arc<RepoService>,
}

pub struct Repositories {
    meta: Arc<dyn MetadataStore>,
    objects: Arc<dyn ObjectStore>,
    audit: Arc<dyn AuditStore>,
    access: Arc<AccessService>,
    clock: Arc<dyn Clock>,
    rules: Rules,
    default_max_locks: u32,
    services: Mutex<HashMap<crate::RepoId, (RepoSettings, Arc<RepoService>)>>,
}

impl Repositories {
    /// `rules` apply to a repository until it has a `.pyn/pyn.toml`.
    pub fn new(
        meta: Arc<dyn MetadataStore>,
        objects: Arc<dyn ObjectStore>,
        audit: Arc<dyn AuditStore>,
        access: Arc<AccessService>,
        clock: Arc<dyn Clock>,
        rules: Rules,
    ) -> Self {
        Self {
            meta,
            objects,
            audit,
            access,
            clock,
            rules,
            default_max_locks: DEFAULT_MAX_LOCKS_PER_USER,
            services: Mutex::new(HashMap::new()),
        }
    }

    /// The per-user lock limit for repositories that set none; validated 1 to `MAX_LOCKS_PER_USER_CEILING`.
    pub fn with_default_max_locks(mut self, limit: u32) -> Result<Self> {
        self.default_max_locks = repo::validate_max_locks(limit)?;
        Ok(self)
    }

    async fn record(
        &self,
        record: &RepoRecord,
        actor: &UserId,
        action: AuditAction,
        detail: String,
    ) -> Result<()> {
        let event = NewAuditEvent {
            at: self.clock.now(),
            actor: actor.clone(),
            action,
            path: None,
            detail,
        };
        self.audit.record(&record.id, event).await
    }

    fn cached(&self, record: &RepoRecord) -> Option<Arc<RepoService>> {
        let services = self.services.lock().unwrap();
        let (settings, svc) = services.get(&record.id)?;
        (*settings == record.settings).then(|| svc.clone())
    }

    /// The cached service, or a new one with the policy read from the repository's head `.pyn/pyn.toml`.
    async fn service_for(&self, record: &RepoRecord) -> Result<Arc<RepoService>> {
        if let Some(svc) = self.cached(record) {
            return Ok(svc);
        }
        let svc = Arc::new(RepoService::new(
            record.id.clone(),
            self.rules.clone(),
            self.meta.clone(),
            self.objects.clone(),
            self.audit.clone(),
            self.clock.clone(),
            ServiceConfig {
                lease: Duration::hours(i64::from(record.settings.lease_hours)),
                max_locks: record.settings.max_locks.unwrap_or(self.default_max_locks),
            },
        ));
        svc.load_policy().await?;
        let mut services = self.services.lock().unwrap();
        if let Some((settings, existing)) = services.get(&record.id)
            && *settings == record.settings
        {
            return Ok(existing.clone());
        }
        services.insert(record.id.clone(), (record.settings, svc.clone()));
        Ok(svc)
    }

    /// Finds `owner/name`; `RepoNotFound` if there is none.
    pub async fn open(&self, owner: &str, name: &str) -> Result<OpenRepo> {
        let record = self
            .meta
            .find_repo(&UserId::new(owner), name)
            .await?
            .ok_or_else(|| PynError::RepoNotFound(format!("{owner}/{name}")))?;
        let service = self.service_for(&record).await?;
        Ok(OpenRepo { record, service })
    }

    /// The lock limit in force for the repository and whether its policy file sets it.
    pub async fn lock_limit(&self, record: &RepoRecord) -> Result<LockLimit> {
        Ok(self.service_for(record).await?.lock_limit())
    }

    /// `owner/name` for a repository id; the id itself if the repository no longer exists.
    pub async fn address_of(&self, id: &crate::RepoId) -> Result<String> {
        Ok(match self.meta.get_repo(id).await? {
            Some(record) => record.address(),
            None => id.to_string(),
        })
    }

    /// Creates `owner/name` and makes the actor its admin. The actor can only use their own namespace, and a token
    /// must be unscoped and carry `manage_roles`.
    pub async fn create(
        &self,
        actor: &Identity,
        owner: &UserId,
        name: &str,
        visibility: Option<Visibility>,
        settings: Option<RepoSettings>,
    ) -> Result<RepoRecord> {
        if let Credential::Token(t) = &actor.credential
            && (!t.repos.is_empty() || !t.permissions.contains(&Permission::ManageRoles))
        {
            return Err(PynError::Forbidden(Permission::ManageRoles));
        }
        if owner != &actor.user {
            return Err(PynError::NotNamespaceOwner(owner.to_string()));
        }
        let record = RepoRecord {
            id: repo::generate_id()?,
            owner: owner.clone(),
            name: repo::validate_name(name)?,
            visibility: visibility.unwrap_or(Visibility::Private),
            settings: settings.unwrap_or_default().validate()?,
            created_at: self.clock.now(),
        };
        let record = self.meta.create_repo(record).await?;
        if let Err(e) = self.access.add_creator(&record.id, &actor.user).await {
            let _ = self.meta.delete_repo(&record.id).await;
            return Err(e);
        }
        let detail = format!(
            "created {} ({}, lease {}h{})",
            record.address(),
            record.visibility,
            record.settings.lease_hours,
            limit_detail(&record.settings)
        );
        self.record(&record, &actor.user, AuditAction::RepoCreated, detail)
            .await?;
        Ok(record)
    }

    /// Repositories the identity belongs to (every repository for the development identity), optionally one
    /// owner's, with the identity's role in each.
    pub async fn list(
        &self,
        who: &Identity,
        owner: Option<&UserId>,
    ) -> Result<Vec<(RepoRecord, Option<Role>)>> {
        let all = self.meta.list_repos(owner).await?;
        let member: HashSet<_> = match &who.credential {
            Credential::Unrestricted => all.iter().map(|r| r.id.clone()).collect(),
            Credential::Session => self.access.repos_of(&who.user).await?.into_iter().collect(),
            Credential::Token(t) => self
                .access
                .repos_of(&who.user)
                .await?
                .into_iter()
                .filter(|r| t.repos.is_empty() || t.repos.contains(r))
                .collect(),
        };
        let mut out = Vec::new();
        for record in all.into_iter().filter(|r| member.contains(&r.id)) {
            let role = self.access.role_in(&record.id, &who.user).await?;
            out.push((record, role));
        }
        Ok(out)
    }

    /// Only the owner, holding admin rights in the repository, may rename, delete or reconfigure it.
    async fn require_owner_admin(&self, actor: &Identity, record: &RepoRecord) -> Result<()> {
        if record.owner != actor.user {
            return Err(PynError::NotNamespaceOwner(record.owner.to_string()));
        }
        let held = self.access.principal_in(&record.id, actor).await?;
        held.require(Permission::ManageRoles)
    }

    pub async fn update(
        &self,
        actor: &Identity,
        record: &RepoRecord,
        mut update: RepoUpdate,
    ) -> Result<RepoRecord> {
        self.require_owner_admin(actor, record).await?;
        if let Some(name) = &update.name {
            update.name = Some(repo::validate_name(name)?);
        }
        if let Some(settings) = update.settings {
            let settings = settings.validate()?;
            if settings.max_locks != record.settings.max_locks
                && self.lock_limit(record).await?.from_policy
            {
                return Err(PynError::InvalidRequest(
                    "the lock limit is set by .pyn/pyn.toml; change it there".into(),
                ));
            }
            update.settings = Some(settings);
        }
        let updated = self.meta.update_repo(&record.id, update).await?;
        let detail = format!(
            "{} -> {} ({}, lease {}h{})",
            record.address(),
            updated.address(),
            updated.visibility,
            updated.settings.lease_hours,
            limit_detail(&updated.settings)
        );
        self.record(&updated, &actor.user, AuditAction::RepoUpdated, detail)
            .await?;
        Ok(updated)
    }

    /// Removes the repository, its locks, revisions and access. Its audit log and stored content stay.
    pub async fn delete(&self, actor: &Identity, record: &RepoRecord) -> Result<()> {
        self.require_owner_admin(actor, record).await?;
        let detail = format!("deleted {}", record.address());
        self.record(record, &actor.user, AuditAction::RepoDeleted, detail)
            .await?;
        self.meta.delete_repo(&record.id).await?;
        self.services.lock().unwrap().remove(&record.id);
        self.access.forget_repo(&record.id).await
    }
}
