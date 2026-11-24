//! Watches: a user's level on a repository and the events queued for it. Whether the caller may see the repository is the caller's check.

use super::*;

/// Watch changes one account may make per hour.
const WATCH_CHANGES_PER_HOUR: u32 = 120;

impl AccessService {
    /// Sets (`Some`) or clears (`None`) the actor's level on `repo`; false when nothing changed. Every call counts against the hourly limit.
    pub async fn set_watch(
        &self,
        actor: &Identity,
        repo: &RepoId,
        level: Option<WatchLevel>,
    ) -> Result<bool> {
        actor.require_full_scope()?;
        self.account_id(&actor.user).await?;
        self.count_limited(
            &format!("watch:{}", bounded_key(&actor.user.as_str().to_lowercase())),
            WATCH_CHANGES_PER_HOUR,
            Duration::hours(1),
        )
        .await?;
        match level {
            Some(level) => {
                let before = self.store.watch_of(repo, &actor.user).await?;
                self.store
                    .set_watch(repo, &actor.user, level, self.clock.now())
                    .await?;
                Ok(before != Some(level))
            }
            None => self.store.clear_watch(repo, &actor.user).await,
        }
    }

    /// Starts the creator of a repository watching all activity; best effort, and they can change it.
    pub(crate) async fn watch_by_default(&self, repo: &RepoId, user: &UserId) {
        if self.account_id(user).await.is_ok() {
            let _ = self
                .store
                .set_watch(repo, user, WatchLevel::All, self.clock.now())
                .await;
        }
    }

    pub async fn watch_level(&self, repo: &RepoId, user: &UserId) -> Result<Option<WatchLevel>> {
        self.store.watch_of(repo, user).await
    }

    pub async fn watcher_count(&self, repo: &RepoId) -> Result<u64> {
        self.store.watcher_count(repo).await
    }

    /// The repositories `user` watches with their level, newest change first.
    pub async fn watched_by(
        &self,
        user: &UserId,
    ) -> Result<Vec<(RepoId, WatchLevel, DateTime<Utc>)>> {
        self.store.watches_of(user).await
    }

    /// Events queued for `user` after `after`, oldest first.
    pub async fn notifications(
        &self,
        user: &UserId,
        after: i64,
        limit: u32,
    ) -> Result<Vec<Notification>> {
        self.store.notifications_of(user, after, limit).await
    }
}
