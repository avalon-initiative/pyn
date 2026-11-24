//! Watch levels and queued notifications, shared by the access and webhook stores.
//! Its own lock, so queueing an event under another store's lock cannot deadlock.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};

use crate::types::{RepoId, UserId};
use crate::watch::{Notification, WatchLevel};
use crate::webhook::OutboxEvent;

#[derive(Default)]
pub(super) struct WatchRegistry {
    levels: BTreeMap<(RepoId, UserId), (WatchLevel, DateTime<Utc>)>,
    queue: Vec<Notification>,
    next: i64,
}

impl WatchRegistry {
    pub(super) fn set(
        &mut self,
        repo: &RepoId,
        user: &UserId,
        level: WatchLevel,
        now: DateTime<Utc>,
    ) {
        self.levels
            .insert((repo.clone(), user.clone()), (level, now));
    }

    pub(super) fn clear(&mut self, repo: &RepoId, user: &UserId) -> bool {
        self.levels.remove(&(repo.clone(), user.clone())).is_some()
    }

    pub(super) fn level(&self, repo: &RepoId, user: &UserId) -> Option<WatchLevel> {
        self.levels
            .get(&(repo.clone(), user.clone()))
            .map(|(l, _)| *l)
    }

    pub(super) fn count(&self, repo: &RepoId) -> u64 {
        self.levels
            .iter()
            .filter(|((r, _), (l, _))| r == repo && *l != WatchLevel::Ignore)
            .count() as u64
    }

    pub(super) fn of(&self, user: &UserId) -> Vec<(RepoId, WatchLevel, DateTime<Utc>)> {
        let mut out: Vec<_> = self
            .levels
            .iter()
            .filter(|((_, u), _)| u == user)
            .map(|((r, _), (l, at))| (r.clone(), *l, *at))
            .collect();
        out.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
        out
    }

    pub(super) fn notifications(&self, user: &UserId, after: i64, limit: u32) -> Vec<Notification> {
        self.queue
            .iter()
            .filter(|n| &n.user == user && n.id > after)
            .take(limit as usize)
            .cloned()
            .collect()
    }

    /// Queues `event` for each watcher of `repo` whose level wants its type.
    pub(super) fn fan_out(&mut self, repo: &RepoId, event: &OutboxEvent) {
        let wanting: Vec<UserId> = self
            .levels
            .iter()
            .filter(|((r, _), (l, _))| r == repo && l.wants(&event.event_type))
            .map(|((_, u), _)| u.clone())
            .collect();
        for user in wanting {
            self.next += 1;
            self.queue.push(Notification {
                id: self.next,
                user,
                repo: repo.clone(),
                event: event.clone(),
            });
        }
    }

    pub(super) fn drop_repo(&mut self, repo: &RepoId) {
        self.levels.retain(|(r, _), _| r != repo);
        self.queue.retain(|n| &n.repo != repo);
    }

    pub(super) fn drop_user(&mut self, user: &UserId) {
        self.levels.retain(|(_, u), _| u != user);
        self.queue.retain(|n| &n.user != user);
    }

    pub(super) fn rename_user(&mut self, user: &UserId, new: &UserId) {
        self.levels = std::mem::take(&mut self.levels)
            .into_iter()
            .map(|((r, mut u), v)| {
                if &u == user {
                    u = new.clone();
                }
                ((r, u), v)
            })
            .collect();
        for n in &mut self.queue {
            if &n.user == user {
                n.user = new.clone();
            }
        }
    }

    /// Drops the notifications of events older than `cutoff`, as the webhook purge drops their events.
    pub(super) fn purge_before(&mut self, cutoff: DateTime<Utc>) {
        self.queue.retain(|n| n.event.created_at >= cutoff);
    }
}
