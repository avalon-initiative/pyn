//! Teams: flat groups of an organization's members that hold one role per repository the organization owns.

use super::*;
use crate::access::team;
use crate::repo::RepoRecord;

/// A team with its members and the repositories it holds a role in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamDetail {
    pub team: TeamRecord,
    pub members: Vec<UserId>,
    pub repos: Vec<(RepoRecord, Role)>,
}

impl AccessService {
    async fn team_detail(&self, team: TeamRecord) -> Result<TeamDetail> {
        let members = self.store.team_members(&team.org, &team.slug).await?;
        let mut repos = Vec::new();
        if let Some(registry) = &self.registry {
            for (id, role) in self.store.team_repos(&team.org, &team.slug).await? {
                if let Some(record) = registry.get_repo(&id).await? {
                    repos.push((record, role));
                }
            }
        }
        Ok(TeamDetail {
            team,
            members,
            repos,
        })
    }

    pub(super) async fn require_team(&self, org: &UserId, slug: &str) -> Result<TeamRecord> {
        self.store
            .team(org, slug)
            .await?
            .ok_or_else(|| PynError::TeamNotFound {
                org: org.to_string(),
                team: slug.to_string(),
            })
    }

    /// The organization's teams, ordered by slug. Any member of the organization may list them.
    pub async fn teams(&self, actor: &Identity, org: &UserId) -> Result<Vec<TeamDetail>> {
        self.org(org).await?;
        if self.store.org_role(org, &actor.user).await?.is_none() {
            return Err(PynError::NotOrgMember(org.to_string()));
        }
        let mut out = Vec::new();
        for t in self.store.teams(org).await? {
            out.push(self.team_detail(t).await?);
        }
        Ok(out)
    }

    pub async fn team(&self, actor: &Identity, org: &UserId, slug: &str) -> Result<TeamDetail> {
        self.org(org).await?;
        if self.store.org_role(org, &actor.user).await?.is_none() {
            return Err(PynError::NotOrgMember(org.to_string()));
        }
        let t = self.require_team(org, slug).await?;
        self.team_detail(t).await
    }

    async fn require_team_admin(&self, actor: &Identity, org: &UserId) -> Result<()> {
        actor.require_namespace_management()?;
        self.org(org).await?;
        self.require_org_owner(org, &actor.user).await
    }

    /// Creates a team; the name defaults to the slug. Organization owners only.
    pub async fn create_team(
        &self,
        actor: &Identity,
        org: &UserId,
        slug: &str,
        name: Option<&str>,
        description: Option<&str>,
    ) -> Result<TeamDetail> {
        self.require_team_admin(actor, org).await?;
        let slug = team::validate_slug(slug)?;
        let record = TeamRecord {
            org: org.clone(),
            name: team::validate_name(name.unwrap_or(&slug))?,
            description: team::validate_description(description.unwrap_or_default())?,
            slug,
            created_at: self.clock.now(),
        };
        if !self.store.create_team(record.clone()).await? {
            return Err(PynError::TeamExists {
                org: org.to_string(),
                team: record.slug,
            });
        }
        let detail = format!("team {} created", record.slug);
        self.record(
            AuditScope::Org(org.clone()),
            &actor.user,
            AuditAction::TeamCreated,
            detail,
        )
        .await?;
        self.team_detail(record).await
    }

    /// Changes a team's name and description; fields left out stay. Organization owners only.
    pub async fn update_team(
        &self,
        actor: &Identity,
        org: &UserId,
        slug: &str,
        name: Option<&str>,
        description: Option<&str>,
    ) -> Result<TeamDetail> {
        self.require_team_admin(actor, org).await?;
        let mut record = self.require_team(org, slug).await?;
        if let Some(name) = name {
            record.name = team::validate_name(name)?;
        }
        if let Some(description) = description {
            record.description = team::validate_description(description)?;
        }
        if !self.store.update_team(&record).await? {
            return Err(PynError::TeamNotFound {
                org: org.to_string(),
                team: slug.to_string(),
            });
        }
        let detail = format!("team {slug} updated");
        self.record(
            AuditScope::Org(org.clone()),
            &actor.user,
            AuditAction::TeamUpdated,
            detail,
        )
        .await?;
        self.team_detail(record).await
    }

    /// Deletes a team, its memberships and its repository roles. Organization owners only.
    pub async fn delete_team(&self, actor: &Identity, org: &UserId, slug: &str) -> Result<()> {
        self.require_team_admin(actor, org).await?;
        self.require_team(org, slug).await?;
        let repos = self.store.team_repos(org, slug).await?;
        let rules = self
            .rules_note(
                org,
                &crate::repo_policy::CreationSubject::Team(slug.to_string()),
            )
            .await?;
        if !self.store.delete_team(org, slug).await? {
            return Err(PynError::TeamNotFound {
                org: org.to_string(),
                team: slug.to_string(),
            });
        }
        let mut detail = format!("team {slug} deleted{rules}");
        if let Some(registry) = &self.registry
            && !repos.is_empty()
        {
            let mut addresses = Vec::new();
            for (id, role) in &repos {
                let note = format!("team {slug} removed with the team (was {role})");
                self.record(id, &actor.user, AuditAction::TeamAccessRemoved, note)
                    .await?;
                if let Some(record) = registry.get_repo(id).await? {
                    addresses.push(record.address());
                }
            }
            detail.push_str(&format!("; access dropped in {}", join(addresses.iter())));
        }
        self.record(
            AuditScope::Org(org.clone()),
            &actor.user,
            AuditAction::TeamDeleted,
            detail,
        )
        .await
    }

    /// Puts an organization member in the team; adding someone already in it changes nothing.
    pub async fn add_team_member(
        &self,
        actor: &Identity,
        org: &UserId,
        slug: &str,
        user: &UserId,
    ) -> Result<()> {
        self.require_team_admin(actor, org).await?;
        self.require_team(org, slug).await?;
        if self.store.org_role(org, user).await?.is_none() {
            return Err(PynError::UserNotOrgMember {
                org: org.to_string(),
                user: user.to_string(),
            });
        }
        if !self.store.add_team_member(org, slug, user).await? {
            return Ok(());
        }
        let detail = format!("{user} added to team {slug}");
        self.record(
            AuditScope::Org(org.clone()),
            &actor.user,
            AuditAction::TeamMemberAdded,
            detail,
        )
        .await
    }

    /// Takes the person out of the team, which ends any access they had through it.
    pub async fn remove_team_member(
        &self,
        actor: &Identity,
        org: &UserId,
        slug: &str,
        user: &UserId,
    ) -> Result<()> {
        self.require_team_admin(actor, org).await?;
        self.require_team(org, slug).await?;
        if !self.store.remove_team_member(org, slug, user).await? {
            return Err(PynError::TeamMemberNotFound {
                team: slug.to_string(),
                user: user.to_string(),
            });
        }
        let detail = format!("{user} removed from team {slug}");
        self.record(
            AuditScope::Org(org.clone()),
            &actor.user,
            AuditAction::TeamMemberRemoved,
            detail,
        )
        .await
    }

    /// The organization that owns `repo`; `NotOrgRepo` when a user owns it.
    async fn repo_org(&self, repo: &RepoId) -> Result<(UserId, String)> {
        let record = match &self.registry {
            Some(registry) => registry.get_repo(repo).await?,
            None => None,
        }
        .ok_or_else(|| PynError::RepoNotFound(repo.to_string()))?;
        if !self.is_org(&record.owner).await? {
            return Err(PynError::NotOrgRepo(record.address()));
        }
        Ok((record.owner.clone(), record.address()))
    }

    /// The teams holding a role in the repository with their roles, ordered by slug.
    pub async fn repo_teams(
        &self,
        actor: &Principal,
        repo: &RepoId,
    ) -> Result<Vec<(TeamRecord, Role)>> {
        actor.require(Permission::ManageUsers)?;
        let Some(registry) = &self.registry else {
            return Ok(Vec::new());
        };
        let Some(record) = registry.get_repo(repo).await? else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for (slug, role) in self.store.team_grants(repo).await? {
            if let Some(t) = self.store.team(&record.owner, &slug).await? {
                out.push((t, role));
            }
        }
        Ok(out)
    }

    /// Gives a team of the owning organization `role` in the repository, like a direct grant but for its members.
    pub async fn set_team_access(
        &self,
        actor: &Principal,
        repo: &RepoId,
        slug: &str,
        role: Role,
    ) -> Result<()> {
        actor.require(Permission::ManageUsers)?;
        let (org, address) = self.repo_org(repo).await?;
        self.require_team(&org, slug).await?;
        self.require_can_grant(actor, repo, role).await?;
        let before = self.store.set_team_role(repo, &org, slug, role).await?;
        if before == Some(role) {
            return Ok(());
        }
        let (org_note, repo_note) = match before {
            None => (
                format!("team {slug} granted {role} on {address}"),
                format!("team {slug} added as {role}"),
            ),
            Some(old) => (
                format!("team {slug} on {address}: {old} -> {role}"),
                format!("team {slug}: {old} -> {role}"),
            ),
        };
        self.record(
            AuditScope::Org(org),
            &actor.user,
            AuditAction::TeamAccessSet,
            org_note,
        )
        .await?;
        self.record(repo, &actor.user, AuditAction::TeamAccessSet, repo_note)
            .await
    }

    /// Takes a team's role in the repository away. The actor must be able to grant the role being removed.
    pub async fn remove_team_access(
        &self,
        actor: &Principal,
        repo: &RepoId,
        slug: &str,
    ) -> Result<()> {
        actor.require(Permission::ManageUsers)?;
        let (org, address) = self.repo_org(repo).await?;
        self.require_team(&org, slug).await?;
        if let Some(old) = self
            .store
            .team_grants(repo)
            .await?
            .into_iter()
            .find_map(|(t, role)| (t == slug).then_some(role))
        {
            self.require_can_grant(actor, repo, old).await?;
        }
        let Some(old) = self.store.remove_team_role(repo, &org, slug).await? else {
            return Ok(());
        };
        let org_note = format!("team {slug} lost {old} on {address}");
        let repo_note = format!("team {slug} removed (was {old})");
        self.record(
            AuditScope::Org(org),
            &actor.user,
            AuditAction::TeamAccessRemoved,
            org_note,
        )
        .await?;
        self.record(repo, &actor.user, AuditAction::TeamAccessRemoved, repo_note)
            .await
    }
}
