//! Stars as a viewer may see them: the count and the caller's own mark come with a repository the viewer can read.

use super::*;

/// A repository's star count and whether the viewer starred it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StarState {
    pub count: u64,
    pub starred: bool,
}

impl Repositories {
    /// The count of a repository the caller already holds, and `viewer`'s own mark.
    pub async fn star_state(
        &self,
        record: &RepoRecord,
        viewer: Option<&UserId>,
    ) -> Result<StarState> {
        let starred = match viewer {
            Some(user) => self.access.is_starred(&record.id, user).await?,
            None => false,
        };
        Ok(StarState {
            count: self.access.star_count(&record.id).await?,
            starred,
        })
    }

    /// Stars or unstars `owner/name` for a signed-in full-scope actor who may read it; starring twice is not an error.
    pub async fn set_star(
        &self,
        actor: &Identity,
        owner: &str,
        name: &str,
        on: bool,
    ) -> Result<(RepoRecord, StarState)> {
        actor.require_full_scope()?;
        let (open, _) = self.open_visible(owner, name, Some(actor)).await?;
        self.access.set_star(actor, &open.record.id, on).await?;
        let state = self.star_state(&open.record, Some(&actor.user)).await?;
        Ok((open.record, state))
    }

    /// The repositories `user` starred that `viewer` may see, newest first, each with the viewer's role and when it was starred.
    pub async fn starred_visible_to(
        &self,
        user: &UserId,
        viewer: Option<&Identity>,
    ) -> Result<Vec<(RepoRecord, Option<Role>, chrono::DateTime<chrono::Utc>)>> {
        let mut out = Vec::new();
        for (id, at) in self.access.starred_by(user).await? {
            let Some(record) = self.meta.get_repo(&id).await? else {
                continue;
            };
            if self.principal_for(&record, viewer).await?.is_none() {
                continue;
            }
            let role = self.access.role_for(&record.id, viewer).await?;
            out.push((record, role, at));
        }
        Ok(out)
    }
}
