//! Forks: a copy of a repository's content and history under another owner, linked to its source (decision #1282).

use std::collections::BTreeSet;

use super::*;
use crate::{ForkCopy, RepoId};

impl Repositories {
    /// Forks `source_owner/source_name` into `owner` as `name` (the source's name by default). The caller needs read
    /// access, and a private source must allow forks; the fork takes the source's visibility, a copy of its history
    /// (only the default branch's with `default_branch_only`) and policy, no locks, and what it copies counts in full
    /// against `owner`'s limits.
    pub async fn fork(
        &self,
        actor: &Identity,
        source_owner: &str,
        source_name: &str,
        owner: &UserId,
        name: Option<&str>,
        default_branch_only: bool,
    ) -> Result<RepoRecord> {
        actor.require_namespace_management()?;
        let (open, principal) = self
            .open_visible(source_owner, source_name, Some(actor))
            .await?;
        principal.require(Permission::Read)?;
        let source = open.record;
        let target = self
            .creation_target(actor, owner, source.visibility)
            .await?;
        if source.visibility == Visibility::Private && !source.settings.allow_private_forks {
            return Err(PynError::ForkForbidden(
                source.address(),
                "its owner does not allow forks of this private repository".into(),
            ));
        }
        let record = RepoRecord {
            id: repo::generate_id()?,
            owner: owner.clone(),
            name: repo::validate_name(name.unwrap_or(&source.name))?,
            visibility: source.visibility,
            settings: RepoSettings {
                allow_private_forks: false,
                ..source.settings
            },
            storage_cap: None,
            initial_policy: source.initial_policy.clone(),
            created_at: self.clock.now(),
            about: RepoAbout {
                description: source.about.description.clone(),
                ..RepoAbout::default()
            },
            fork_of: Some(source.id.clone()),
        };
        let limits = self.access.effective_limits(owner).await?;
        let (per_repository, per_owner) = self.fork_caps;
        let caps = ForkLimits {
            repositories: limits.repositories,
            storage_bytes: limits.storage_bytes,
            per_repository: Some(per_repository),
            per_owner: Some(per_owner),
        };
        let owner_id = self.access.account_id(owner).await?;
        let created = self.access.draft(DomainEvent::RepositoryCreated {
            repo_id: record.id.clone(),
            owner: owner.clone(),
            owner_id,
            name: record.name.clone(),
            visibility: record.visibility,
            actor: actor.user.clone(),
        })?;
        let copy = if default_branch_only {
            self.default_branch_copy(&source.id).await?
        } else {
            ForkCopy::All
        };
        let result = self
            .meta
            .fork_repo_with_events(record, &copy, caps, &[created])
            .await;
        let record = self
            .access
            .limit_result(owner, result, self.clock.now())
            .await?;
        let detail = format!(
            "forked {} to {} ({})",
            source.address(),
            record.address(),
            record.visibility
        );
        self.finish_create(actor, &record, target, detail).await?;
        Ok(record)
    }

    /// The default branch of `source` at its head now, with the LFS objects the pointer files in the history kept name.
    async fn default_branch_copy(&self, source: &RepoId) -> Result<ForkCopy> {
        let head = self
            .meta
            .list_branches(source)
            .await?
            .into_iter()
            .find(|b| b.default)
            .and_then(|b| b.head);
        let mut lfs = BTreeSet::new();
        if let Some(head) = &head
            && self.meta.lfs_bytes(source).await? > 0
        {
            for (content, size) in self.meta.fork_contents(source, head).await? {
                if size >= crate::service::LFS_POINTER_MAX_BYTES {
                    continue;
                }
                if let Some(bytes) = self.objects.get(&content).await?
                    && let Some(oid) = crate::service::lfs_pointer_oid(&bytes)
                {
                    lfs.insert(oid.to_string());
                }
            }
        }
        Ok(ForkCopy::DefaultBranch {
            head,
            lfs: lfs.into_iter().collect(),
        })
    }

    /// The forks of `source` the viewer may see, by the same rule as `open_visible`, with the viewer's role in each.
    pub async fn forks_visible(
        &self,
        source: &RepoRecord,
        who: Option<&Identity>,
    ) -> Result<Vec<(RepoRecord, Option<Role>)>> {
        let mut out = Vec::new();
        for record in self.meta.list_forks(&source.id).await? {
            if self.principal_for(&record, who).await?.is_none() {
                continue;
            }
            let role = self.access.role_for(&record.id, who).await?;
            out.push((record, role));
        }
        Ok(out)
    }

    /// How many repositories were forked from this one, whoever owns them.
    pub async fn fork_count(&self, record: &RepoRecord) -> Result<u64> {
        self.meta.count_forks(&record.id).await
    }

    /// The repository this one was forked from, when it still exists and is public or `viewer` has a role in it.
    pub async fn fork_source(
        &self,
        record: &RepoRecord,
        viewer: Option<&UserId>,
    ) -> Result<Option<RepoRecord>> {
        let Some(id) = &record.fork_of else {
            return Ok(None);
        };
        let Some(source) = self.meta.get_repo(id).await? else {
            return Ok(None);
        };
        if source.visibility == Visibility::Public {
            return Ok(Some(source));
        }
        let Some(user) = viewer else { return Ok(None) };
        Ok(self.access.role_in(&source.id, user).await?.map(|_| source))
    }

    /// A fork of a private repository may not be made public while its source is still private.
    pub(super) async fn require_public_source(&self, record: &RepoRecord) -> Result<()> {
        let Some(id) = &record.fork_of else {
            return Ok(());
        };
        match self.meta.get_repo(id).await? {
            Some(source) if source.visibility == Visibility::Private => {
                Err(PynError::InvalidRequest(format!(
                    "{} is a fork of a private repository and stays private while its source is private",
                    record.address()
                )))
            }
            _ => Ok(()),
        }
    }
}
