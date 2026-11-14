//! Account self-service: email addresses and username changes (#982, decisions #983 and #986).

use chrono::{DateTime, Utc};

use crate::types::{AccountId, UserId};

/// One address an account holds; the primary is the one the server uses for the account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountEmail {
    /// Lowercase.
    pub address: String,
    pub verified_at: Option<DateTime<Utc>>,
    pub primary: bool,
    pub added_at: DateTime<Utc>,
}

/// The outcome of adding an address to an account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmailAdd {
    Added,
    /// Another account holds or has pending the address; the caller must not say which.
    Taken,
    /// The account already holds it.
    Own,
}

/// The outcome of removing an address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmailRemove {
    Removed,
    NotFound,
    /// It is the only verified address and the server requires one.
    LastVerified,
}

/// The outcome of choosing a primary address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrimaryChange {
    Changed,
    NotFound,
    Unverified,
}

/// The outcome of renaming an account in the access store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenameOutcome {
    Renamed {
        account: AccountId,
    },
    /// An account or organization already holds the new name.
    NameTaken,
    NoAccount,
}

/// One name an account held, and the span it held it for; how history resolves an author to the current name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameSpan {
    pub account: AccountId,
    pub name: UserId,
    pub held_from: DateTime<Utc>,
    pub held_until: DateTime<Utc>,
    /// The name the account took when this span ended.
    pub next: UserId,
}
