//! Watching as a viewer may see it: the count and the caller's own level come with a repository the viewer can read.

use super::*;
use crate::watch::{Notification, WatchLevel, WatchState};

impl Repositories {
    /// The watcher count of a repository the caller already holds, and `viewer`'s own level.
    pub async fn watch_state(
        &self,
        record: &RepoRecord,
        viewer: Option<&UserId>,
    ) -> Result<WatchState> {
        let level = match viewer {
            Some(user) => self.access.watch_level(&record.id, user).await?,
            None => None,
        };
        Ok(WatchState {
            level,
            watchers: self.access.watcher_count(&record.id).await?,
        })
    }

    /// Sets or clears (`None`) the level of a signed-in full-scope actor who may read `owner/name`.
    pub async fn set_watch(
        &self,
        actor: &Identity,
        owner: &str,
        name: &str,
        level: Option<WatchLevel>,
    ) -> Result<(RepoRecord, WatchState)> {
        actor.require_full_scope()?;
        let (open, _) = self.open_visible(owner, name, Some(actor)).await?;
        self.access.set_watch(actor, &open.record.id, level).await?;
        let state = self.watch_state(&open.record, Some(&actor.user)).await?;
        Ok((open.record, state))
    }

    /// The repositories the actor watches that they may still read, newest change first, each with their role and level.
    /// A repository they lost access to is skipped, so it stops appearing without the level being forgotten.
    pub async fn watched_by(
        &self,
        actor: &Identity,
    ) -> Result<
        Vec<(
            RepoRecord,
            Option<Role>,
            WatchLevel,
            chrono::DateTime<chrono::Utc>,
        )>,
    > {
        let mut out = Vec::new();
        for (id, level, at) in self.access.watched_by(&actor.user).await? {
            let Some(record) = self.meta.get_repo(&id).await? else {
                continue;
            };
            if self.principal_for(&record, Some(actor)).await?.is_none() {
                continue;
            }
            let role = self.access.role_for(&record.id, Some(actor)).await?;
            out.push((record, role, level, at));
        }
        Ok(out)
    }

    /// The events queued for the actor after `after`, oldest first, only for repositories they may still read: revoked
    /// access stops notifications.
    pub async fn notifications(
        &self,
        actor: &Identity,
        after: i64,
        limit: u32,
    ) -> Result<Vec<Notification>> {
        let mut out = Vec::new();
        let mut cursor = after;
        while out.len() < limit as usize {
            let page = self
                .access
                .notifications(&actor.user, cursor, limit)
                .await?;
            let Some(last) = page.last() else { break };
            cursor = last.id;
            for n in page {
                let readable = match self.meta.get_repo(&n.repo).await? {
                    Some(record) => self.principal_for(&record, Some(actor)).await?.is_some(),
                    None => false,
                };
                if readable && out.len() < limit as usize {
                    out.push(n);
                }
            }
        }
        Ok(out)
    }
}
