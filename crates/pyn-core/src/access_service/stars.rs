//! Stars: a user's mark on a repository. Whether the caller may see the repository is the caller's check.

use super::*;

/// Stars and unstars one account may make per hour.
const STAR_CHANGES_PER_HOUR: u32 = 120;

impl AccessService {
    /// Marks or unmarks `repo` for the actor; false when nothing changed. Every call counts against the hourly limit.
    pub async fn set_star(&self, actor: &Identity, repo: &RepoId, on: bool) -> Result<bool> {
        actor.require_full_scope()?;
        self.account_id(&actor.user).await?;
        self.count_limited(
            &format!("star:{}", bounded_key(&actor.user.as_str().to_lowercase())),
            STAR_CHANGES_PER_HOUR,
            Duration::hours(1),
        )
        .await?;
        if on {
            self.store
                .add_star(repo, &actor.user, self.clock.now())
                .await
        } else {
            self.store.remove_star(repo, &actor.user).await
        }
    }

    pub async fn is_starred(&self, repo: &RepoId, user: &UserId) -> Result<bool> {
        self.store.has_star(repo, user).await
    }

    pub async fn star_count(&self, repo: &RepoId) -> Result<u64> {
        self.store.star_count(repo).await
    }

    /// The repositories `user` starred, newest first.
    pub async fn starred_by(&self, user: &UserId) -> Result<Vec<(RepoId, DateTime<Utc>)>> {
        self.store.stars_of(user).await
    }
}
