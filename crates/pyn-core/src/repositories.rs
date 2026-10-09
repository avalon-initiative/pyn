//! The repository registry as a service: who may create, change and delete repositories, and a `RepoService`
//! built on demand for each.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use chrono::Duration;

use crate::access::{Credential, Identity, OrgRole, Permission, Principal, Role};
use crate::access_service::AccessService;
use crate::audit::{AuditAction, AuditScope, AuditStore, NewAuditEvent};
use crate::clock::Clock;
use crate::error::{PynError, Result};
use crate::object::ObjectStore;
use crate::repo::{
    self, DEFAULT_MAX_LOCKS_PER_USER, RepoRecord, RepoSettings, RepoUpdate, Visibility,
};
use crate::rules::Rules;
use crate::service::{LockLimit, RepoService, ServiceConfig};
use crate::store::MetadataStore;
use crate::types::{Lock, UserId};

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
        self.audit
            .record(&AuditScope::Repo(record.id.clone()), event)
            .await
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

    /// What the caller may do in the repository: their role, plus read on a public one for anyone, signed in or not.
    /// `None` hides a private repository from someone with no part in it.
    pub async fn principal_for(
        &self,
        record: &RepoRecord,
        who: Option<&Identity>,
    ) -> Result<Option<Principal>> {
        let held = match who {
            Some(who) => match self.access.principal_in(&record.id, who).await {
                Ok(p) => Some(p),
                Err(PynError::Unauthenticated(_)) => None,
                Err(e) => return Err(e),
            },
            None => None,
        };
        let public = record.visibility == Visibility::Public;
        Ok(match held {
            Some(mut p) if !p.permissions.is_empty() => {
                if public {
                    p.permissions.insert(Permission::Read);
                }
                Some(p)
            }
            _ if public => Some(Principal {
                user: who.map_or_else(|| UserId::new(""), |w| w.user.clone()),
                permissions: [Permission::Read].into(),
            }),
            _ => None,
        })
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

    /// Creates `owner/name` in the actor's namespace or an organization whose policy lets them; the creator of a
    /// user's repository, or a non-owner's in an organization, becomes its admin. A token must be unscoped and
    /// carry `manage_roles`.
    pub async fn create(
        &self,
        actor: &Identity,
        owner: &UserId,
        name: &str,
        visibility: Option<Visibility>,
        settings: Option<RepoSettings>,
    ) -> Result<RepoRecord> {
        actor.require_namespace_management()?;
        let visibility = visibility.unwrap_or(Visibility::Private);
        let in_org = owner != &actor.user && self.access.is_org(owner).await?;
        let mut org_role = None;
        if in_org {
            org_role = Some(
                self.access
                    .require_repo_creator(owner, &actor.user, visibility)
                    .await?,
            );
            self.access.require_org_open(owner).await?;
        } else if owner != &actor.user {
            return Err(PynError::NotNamespaceOwner(owner.to_string()));
        }
        let record = RepoRecord {
            id: repo::generate_id()?,
            owner: owner.clone(),
            name: repo::validate_name(name)?,
            visibility,
            settings: settings.unwrap_or_default().validate()?,
            created_at: self.clock.now(),
        };
        let record = self.meta.create_repo(record).await?;
        if in_org && let Err(e) = self.access.require_org_open(owner).await {
            let _ = self.meta.delete_repo(&record.id).await;
            return Err(e);
        }
        if org_role != Some(OrgRole::Owner)
            && let Err(e) = self.add_creator(&record, owner, in_org, &actor.user).await
        {
            let _ = self.meta.delete_repo(&record.id).await;
            let _ = self.access.forget_repo(&record.id).await;
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

    /// Makes the creator admin; in an organization, also re-checks they are still a member.
    async fn add_creator(
        &self,
        record: &RepoRecord,
        owner: &UserId,
        in_org: bool,
        creator: &UserId,
    ) -> Result<()> {
        self.access.add_creator(&record.id, creator).await?;
        if in_org && self.access.org_role(owner, creator).await?.is_none() {
            return Err(PynError::NotOrgMember(owner.to_string()));
        }
        Ok(())
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
            Credential::Service(_) => HashSet::new(),
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

    /// The caller's live locks in every repository they can read, ordered by address then path.
    pub async fn locks_of(&self, who: &Identity) -> Result<Vec<(RepoRecord, Lock)>> {
        let mut out = Vec::new();
        let mut readable: HashMap<crate::RepoId, Option<RepoRecord>> = HashMap::new();
        for (id, lock) in self.meta.list_locks_of(&who.user, self.clock.now()).await? {
            if !readable.contains_key(&id) {
                let record = match self.meta.get_repo(&id).await? {
                    Some(r) => match self.principal_for(&r, Some(who)).await? {
                        Some(p) if p.has(Permission::Read) => Some(r),
                        _ => None,
                    },
                    None => None,
                };
                readable.insert(id.clone(), record);
            }
            if let Some(Some(record)) = readable.get(&id) {
                out.push((record.clone(), lock));
            }
        }
        out.sort_by_cached_key(|(r, l)| (r.address(), l.path.clone()));
        Ok(out)
    }

    /// Only the owner (or an owner of the owning organization), holding admin rights in the repository, may
    /// rename, delete or reconfigure it.
    async fn require_owner_admin(&self, actor: &Identity, record: &RepoRecord) -> Result<()> {
        if record.owner != actor.user {
            if !self.access.is_org(&record.owner).await? {
                return Err(PynError::NotNamespaceOwner(record.owner.to_string()));
            }
            self.access
                .require_org_owner(&record.owner, &actor.user)
                .await?;
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
        let visibility = if record.visibility == updated.visibility {
            updated.visibility.to_string()
        } else {
            format!("visibility {} -> {}", record.visibility, updated.visibility)
        };
        let detail = format!(
            "{} -> {} ({visibility}, lease {}h{})",
            record.address(),
            updated.address(),
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
