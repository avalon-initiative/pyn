//! Account deletion (decision #976): the scheduled-deletion record, name reservations and the store outcomes.

use chrono::{DateTime, Utc};

use crate::access::AccountRecord;
use crate::types::UserId;

/// How long a purged account's name stays reserved, in days.
pub const NAME_RESERVATION_DAYS: i64 = 90;

/// A deletion scheduled for an account: it stays disabled until `purge_at`, then is purged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountDeletion {
    pub user: UserId,
    /// The account itself or the administrator that scheduled it.
    pub requested_by: UserId,
    /// Self-service: the owner may sign in to restore the account until `purge_at`.
    pub by_owner: bool,
    pub reason: Option<String>,
    pub scheduled_at: DateTime<Utc>,
    pub purge_at: DateTime<Utc>,
}

/// A name held back after its account was purged. `until` is `None` once the name is known to appear in history
/// or the audit log, which keeps it reserved for good.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameReservation {
    /// Lowercase.
    pub name: String,
    pub reserved_at: DateTime<Utc>,
    pub until: Option<DateTime<Utc>>,
}

impl NameReservation {
    /// Whether the time-limited reservation still runs at `now`.
    pub fn holds_at(&self, now: DateTime<Utc>) -> bool {
        self.until.is_none_or(|until| until > now)
    }
}

/// The outcome of scheduling an account's deletion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeletionSchedule {
    Scheduled,
    NoAccount,
    /// The account is the only active administrator.
    LastAdmin,
}

/// The outcome of purging an account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountPurge {
    /// Gone; carries the record as it was.
    Purged(AccountRecord),
    NoAccount,
    /// Only a disabled account is purged.
    NotDisabled,
}

/// What stands in the way of purging an account.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PurgeBlockers {
    /// Repositories the account owns.
    pub repos: Vec<String>,
    /// Organizations the account is the only owner of.
    pub orgs: Vec<String>,
}

impl PurgeBlockers {
    pub fn is_empty(&self) -> bool {
        self.repos.is_empty() && self.orgs.is_empty()
    }
}

/// What one pass of the deletion task did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeletionSweep {
    pub purged: Vec<UserId>,
    /// Accounts due for purging that a rule held back, with the reason.
    pub blocked: Vec<(UserId, String)>,
    /// Stale schedules dropped because the account is no longer disabled.
    pub dropped: Vec<UserId>,
    /// Name reservations released because the name appears nowhere.
    pub names_released: usize,
}

impl DeletionSweep {
    pub fn is_empty(&self) -> bool {
        self.purged.is_empty()
            && self.blocked.is_empty()
            && self.dropped.is_empty()
            && self.names_released == 0
    }
}
