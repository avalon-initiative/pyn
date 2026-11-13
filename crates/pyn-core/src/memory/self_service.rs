//! The in-memory side of account self-service: added email addresses, renames and the name-span ledger.

use super::*;
use crate::account_self::{
    AccountEmail, EmailAdd, EmailRemove, NameSpan, PrimaryChange, RenameOutcome,
};

/// An address held beside the account's primary one.
#[derive(Debug, Clone)]
pub(super) struct EmailRow {
    user: UserId,
    address: String,
    verified_at: Option<DateTime<Utc>>,
    added_at: DateTime<Utc>,
}

impl AccessState {
    pub(super) fn emails_of(&self, user: &UserId) -> Vec<AccountEmail> {
        let mut out = Vec::new();
        if let Some(a) = self.accounts.get(user)
            && let Some(address) = &a.email
        {
            out.push(AccountEmail {
                address: address.clone(),
                verified_at: a.email_verified_at,
                primary: true,
                added_at: a.created_at,
            });
        }
        let mut rows: Vec<_> = self.emails.iter().filter(|r| &r.user == user).collect();
        rows.sort_by_key(|r| r.added_at);
        out.extend(rows.into_iter().map(|r| AccountEmail {
            address: r.address.clone(),
            verified_at: r.verified_at,
            primary: false,
            added_at: r.added_at,
        }));
        out
    }

    pub(super) fn verified_address_owner(&self, address: &str) -> Option<UserId> {
        let slot = self
            .accounts
            .values()
            .find(|a| a.email_verified_at.is_some() && a.email.as_deref() == Some(address))
            .map(|a| a.user.clone());
        slot.or_else(|| {
            self.emails
                .iter()
                .find(|r| r.verified_at.is_some() && r.address == address)
                .map(|r| r.user.clone())
        })
    }

    pub(super) fn add_email(
        &mut self,
        user: &UserId,
        address: &str,
        now: DateTime<Utc>,
    ) -> EmailAdd {
        let mine = self.emails_of(user).iter().any(|e| e.address == address);
        if mine {
            return EmailAdd::Own;
        }
        let held = self
            .accounts
            .values()
            .any(|a| &a.user != user && a.email.as_deref() == Some(address))
            || self
                .emails
                .iter()
                .any(|r| &r.user != user && r.address == address);
        if held {
            return EmailAdd::Taken;
        }
        self.emails.push(EmailRow {
            user: user.clone(),
            address: address.to_string(),
            verified_at: None,
            added_at: now,
        });
        EmailAdd::Added
    }

    pub(super) fn confirm_added_email(
        &mut self,
        user: &UserId,
        address: &str,
        now: DateTime<Utc>,
    ) -> bool {
        let Some(at) = self
            .emails
            .iter()
            .position(|r| &r.user == user && r.address == address && r.verified_at.is_none())
        else {
            return false;
        };
        let elsewhere = self.accounts.values().any(|a| {
            &a.user != user && a.email_verified_at.is_some() && a.email.as_deref() == Some(address)
        });
        if elsewhere {
            return false;
        }
        let no_primary = self.accounts.get(user).is_some_and(|a| a.email.is_none());
        if no_primary {
            self.emails.remove(at);
            let account = self.accounts.get_mut(user).expect("checked above");
            account.email = Some(address.to_string());
            account.email_verified_at = Some(now);
        } else {
            self.emails[at].verified_at = Some(now);
        }
        true
    }

    pub(super) fn set_primary_email(&mut self, user: &UserId, address: &str) -> PrimaryChange {
        let Some(account) = self.accounts.get(user) else {
            return PrimaryChange::NotFound;
        };
        if account.email.as_deref() == Some(address) {
            return match account.email_verified_at {
                Some(_) => PrimaryChange::Changed,
                None => PrimaryChange::Unverified,
            };
        }
        let Some(at) = self
            .emails
            .iter()
            .position(|r| &r.user == user && r.address == address)
        else {
            return PrimaryChange::NotFound;
        };
        let Some(verified) = self.emails[at].verified_at else {
            return PrimaryChange::Unverified;
        };
        let row = self.emails.remove(at);
        let account = self.accounts.get_mut(user).expect("checked above");
        if let Some(old) = account.email.take() {
            self.emails.push(EmailRow {
                user: user.clone(),
                address: old,
                verified_at: account.email_verified_at,
                added_at: account.created_at,
            });
        }
        account.email = Some(row.address);
        account.email_verified_at = Some(verified);
        PrimaryChange::Changed
    }

    pub(super) fn remove_email(
        &mut self,
        user: &UserId,
        address: &str,
        keep_verified: bool,
    ) -> EmailRemove {
        let Some(account) = self.accounts.get(user) else {
            return EmailRemove::NotFound;
        };
        let primary = account.email.as_deref() == Some(address);
        let row = self
            .emails
            .iter()
            .position(|r| &r.user == user && r.address == address);
        if !primary && row.is_none() {
            return EmailRemove::NotFound;
        }
        let verified_now = self
            .emails_of(user)
            .iter()
            .filter(|e| e.verified_at.is_some())
            .count();
        let removing_verified = if primary {
            account.email_verified_at.is_some()
        } else {
            self.emails[row.expect("checked above")]
                .verified_at
                .is_some()
        };
        if keep_verified && removing_verified && verified_now == 1 {
            return EmailRemove::LastVerified;
        }
        if !primary {
            self.emails.remove(row.expect("checked above"));
            return EmailRemove::Removed;
        }
        let next = self
            .emails
            .iter()
            .enumerate()
            .filter(|(_, r)| &r.user == user && r.verified_at.is_some())
            .min_by_key(|(_, r)| r.added_at)
            .map(|(i, _)| i);
        let promoted = next.map(|i| self.emails.remove(i));
        let account = self.accounts.get_mut(user).expect("checked above");
        match promoted {
            Some(row) => {
                account.email = Some(row.address);
                account.email_verified_at = row.verified_at;
            }
            None => {
                account.email = None;
                account.email_verified_at = None;
            }
        }
        EmailRemove::Removed
    }

    pub(super) fn rename_account(
        &mut self,
        user: &UserId,
        new: &UserId,
        at: DateTime<Utc>,
    ) -> RenameOutcome {
        let Some(account) = self.accounts.get(user) else {
            return RenameOutcome::NoAccount;
        };
        if account.kind != AccountKind::User {
            return RenameOutcome::NoAccount;
        }
        if self.accounts.contains_key(new) {
            return RenameOutcome::NameTaken;
        }
        let id = account.id.clone();
        let held_from = self
            .spans
            .iter()
            .filter(|s| s.account == id)
            .map(|s| s.held_until)
            .max()
            .unwrap_or(account.created_at)
            .max(account.created_at);
        let held_from = DateTime::from_timestamp(held_from.timestamp(), 0).unwrap_or(held_from);
        self.spans.push(NameSpan {
            account: id.clone(),
            name: user.clone(),
            held_from,
            held_until: at,
            next: new.clone(),
        });
        let swap = |u: &mut UserId| {
            if u == user {
                *u = new.clone();
            }
        };
        let mut account = self.accounts.remove(user).expect("checked above");
        account.user = new.clone();
        self.accounts.insert(new.clone(), account);
        if let Some(p) = self.passwords.remove(user) {
            self.passwords.insert(new.clone(), p);
        }
        if let Some(l) = self.limits.remove(user) {
            self.limits.insert(new.clone(), l);
        }
        self.roles = std::mem::take(&mut self.roles)
            .into_iter()
            .map(|((repo, mut u), r)| {
                swap(&mut u);
                ((repo, u), r)
            })
            .collect();
        self.org_roles = std::mem::take(&mut self.org_roles)
            .into_iter()
            .map(|((org, mut u), r)| {
                swap(&mut u);
                ((org, u), r)
            })
            .collect();
        self.org_invitations = std::mem::take(&mut self.org_invitations)
            .into_iter()
            .map(|((org, mut u), mut inv)| {
                swap(&mut u);
                swap(&mut inv.user);
                swap(&mut inv.invited_by);
                ((org, u), inv)
            })
            .collect();
        self.team_members = std::mem::take(&mut self.team_members)
            .into_iter()
            .map(|(org, team, mut u)| {
                swap(&mut u);
                (org, team, u)
            })
            .collect();
        self.creation_rules = std::mem::take(&mut self.creation_rules)
            .into_iter()
            .map(|((org, subject, effect), scope)| {
                let subject = match subject {
                    CreationSubject::User(mut u) => {
                        swap(&mut u);
                        CreationSubject::User(u)
                    }
                    other => other,
                };
                ((org, subject, effect), scope)
            })
            .collect();
        for i in self.invites.values_mut() {
            swap(&mut i.created_by);
            if let Some(u) = i.used_by.as_mut() {
                swap(u);
            }
        }
        for k in &mut self.ssh_keys {
            swap(&mut k.user);
        }
        for t in self.tokens.values_mut() {
            swap(&mut t.user);
        }
        for s in self.sessions.values_mut() {
            swap(&mut s.user);
        }
        for i in &mut self.identities {
            swap(&mut i.user);
        }
        for f in self.oidc_flows.values_mut() {
            if let Some(u) = f.link_user.as_mut() {
                swap(u);
            }
        }
        for v in self.verifications.values_mut() {
            swap(&mut v.user);
        }
        for e in &mut self.emails {
            swap(&mut e.user);
        }
        RenameOutcome::Renamed { account: id }
    }

    pub(super) fn current_names(&self, authors: &[(UserId, DateTime<Utc>)]) -> Vec<Option<UserId>> {
        authors
            .iter()
            .map(|(name, at)| {
                let span = self
                    .spans
                    .iter()
                    .filter(|s| &s.name == name && s.held_from <= *at && *at < s.held_until)
                    .max_by_key(|s| s.held_from)?;
                let last = self
                    .spans
                    .iter()
                    .filter(|s| s.account == span.account)
                    .max_by_key(|s| s.held_until)?;
                Some(last.next.clone())
            })
            .collect()
    }
}
