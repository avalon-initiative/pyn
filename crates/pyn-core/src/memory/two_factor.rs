//! The in-memory side of second factors: factors, recovery codes, attempt state and fresh checks.

use super::*;
use crate::two_factor::{
    AttemptState, ChallengeRecord, FactorRecord, FactorRemoval, NoticeRecord, PasskeyRecord,
    TwoFactorPolicy, count_attempt,
};

#[derive(Default)]
pub(super) struct TwoFactorState {
    factors: Vec<FactorRecord>,
    passkeys: Vec<PasskeyRecord>,
    challenges: Vec<ChallengeRecord>,
    /// (account, hash, spent at)
    codes: Vec<(UserId, String, Option<DateTime<Utc>>)>,
    attempts: HashMap<UserId, AttemptState>,
    fresh: HashMap<String, (UserId, DateTime<Utc>)>,
    org_policies: HashMap<UserId, TwoFactorPolicy>,
    notices: Vec<NoticeRecord>,
}

impl TwoFactorState {
    pub(super) fn rename(&mut self, old: &UserId, new: &UserId) {
        let swap = |u: &mut UserId| {
            if u == old {
                *u = new.clone();
            }
        };
        self.factors.iter_mut().for_each(|f| swap(&mut f.user));
        self.passkeys.iter_mut().for_each(|p| swap(&mut p.user));
        self.challenges
            .iter_mut()
            .filter_map(|c| c.user.as_mut())
            .for_each(swap);
        self.codes.iter_mut().for_each(|c| swap(&mut c.0));
        self.fresh.values_mut().for_each(|f| swap(&mut f.0));
        self.notices.iter_mut().for_each(|n| swap(&mut n.user));
        if let Some(state) = self.attempts.remove(old) {
            self.attempts.insert(new.clone(), state);
        }
    }

    pub(super) fn forget(&mut self, user: &UserId) -> usize {
        let confirmed = self
            .factors
            .iter()
            .filter(|f| &f.user == user && f.confirmed_at.is_some())
            .count()
            + self.passkeys.iter().filter(|p| &p.user == user).count();
        self.factors.retain(|f| &f.user != user);
        self.passkeys.retain(|p| &p.user != user);
        self.challenges.retain(|c| c.user.as_ref() != Some(user));
        self.codes.retain(|c| &c.0 != user);
        self.attempts.remove(user);
        self.fresh.retain(|_, f| &f.0 != user);
        confirmed
    }

    pub(super) fn forget_account(&mut self, user: &UserId) {
        self.forget(user);
        self.notices.retain(|n| &n.user != user);
    }

    pub(super) fn forget_org(&mut self, org: &UserId) {
        self.org_policies.remove(org);
    }

    pub(super) fn org_policy(&self, org: &UserId) -> Option<TwoFactorPolicy> {
        self.org_policies.get(org).copied()
    }

    pub(super) fn set_org_policy(
        &mut self,
        org: &UserId,
        policy: Option<TwoFactorPolicy>,
    ) -> Option<TwoFactorPolicy> {
        match policy {
            Some(p) => self.org_policies.insert(org.clone(), p),
            None => self.org_policies.remove(org),
        }
    }

    pub(super) fn add_notice(&mut self, notice: NoticeRecord) {
        self.notices.push(notice);
    }

    pub(super) fn notices_of(&self, user: &UserId) -> Vec<NoticeRecord> {
        let mut out: Vec<_> = self
            .notices
            .iter()
            .filter(|n| &n.user == user)
            .cloned()
            .collect();
        out.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        out
    }

    pub(super) fn dismiss_notice(&mut self, user: &UserId, id: &str) -> bool {
        let before = self.notices.len();
        self.notices.retain(|n| !(&n.user == user && n.id == id));
        self.notices.len() != before
    }

    pub(super) fn factors_of(&self, user: &UserId) -> Vec<FactorRecord> {
        let mut out: Vec<_> = self
            .factors
            .iter()
            .filter(|f| &f.user == user)
            .cloned()
            .collect();
        out.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        out
    }

    pub(super) fn create_factor(&mut self, factor: FactorRecord) {
        self.factors
            .retain(|f| !(f.user == factor.user && f.confirmed_at.is_none()));
        self.factors.push(factor);
    }

    pub(super) fn confirm_factor(&mut self, user: &UserId, id: &str, now: DateTime<Utc>) -> bool {
        match self
            .factors
            .iter_mut()
            .find(|f| &f.user == user && f.id == id && f.confirmed_at.is_none())
        {
            Some(f) => {
                f.confirmed_at = Some(now);
                true
            }
            None => false,
        }
    }

    pub(super) fn remove_factor(&mut self, user: &UserId, id: &str) -> FactorRemoval {
        let before = self.factors.len();
        self.factors.retain(|f| !(&f.user == user && f.id == id));
        if self.factors.len() == before {
            return FactorRemoval::NotFound;
        }
        let left = self.second_factor_left(user);
        if !left {
            self.forget(user);
        }
        FactorRemoval::Removed {
            second_factor_left: left,
        }
    }

    fn second_factor_left(&self, user: &UserId) -> bool {
        self.factors
            .iter()
            .any(|f| &f.user == user && f.confirmed_at.is_some())
            || self.passkeys.iter().any(|p| &p.user == user)
    }

    pub(super) fn passkeys_of(&self, user: &UserId) -> Vec<PasskeyRecord> {
        let mut out: Vec<_> = self
            .passkeys
            .iter()
            .filter(|p| &p.user == user)
            .cloned()
            .collect();
        out.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        out
    }

    pub(super) fn passkey_by_credential(&self, credential_id: &[u8]) -> Option<PasskeyRecord> {
        self.passkeys
            .iter()
            .find(|p| p.credential_id == credential_id)
            .cloned()
    }

    pub(super) fn create_passkey(&mut self, passkey: PasskeyRecord) -> bool {
        if self
            .passkeys
            .iter()
            .any(|p| p.credential_id == passkey.credential_id)
        {
            return false;
        }
        self.passkeys.push(passkey);
        true
    }

    pub(super) fn rename_passkey(&mut self, user: &UserId, id: &str, name: &str) -> bool {
        match self
            .passkeys
            .iter_mut()
            .find(|p| &p.user == user && p.id == id)
        {
            Some(p) => {
                p.name = name.to_string();
                true
            }
            None => false,
        }
    }

    pub(super) fn remove_passkey(&mut self, user: &UserId, id: &str) -> FactorRemoval {
        let before = self.passkeys.len();
        self.passkeys.retain(|p| !(&p.user == user && p.id == id));
        if self.passkeys.len() == before {
            return FactorRemoval::NotFound;
        }
        let left = self.second_factor_left(user);
        if !left {
            self.forget(user);
        }
        FactorRemoval::Removed {
            second_factor_left: left,
        }
    }

    pub(super) fn record_passkey_use(
        &mut self,
        user: &UserId,
        id: &str,
        previous: u32,
        sign_count: u32,
        backup_state: bool,
        now: DateTime<Utc>,
    ) -> bool {
        match self
            .passkeys
            .iter_mut()
            .find(|p| &p.user == user && p.id == id && p.sign_count == previous)
        {
            Some(p) => {
                p.sign_count = sign_count;
                p.backup_state = backup_state;
                p.last_used_at = Some(now);
                true
            }
            None => false,
        }
    }

    pub(super) fn put_challenge(&mut self, challenge: ChallengeRecord, now: DateTime<Utc>) {
        self.challenges.retain(|c| {
            c.expires_at > now
                && !(challenge.user.is_some()
                    && c.user == challenge.user
                    && c.purpose == challenge.purpose)
        });
        self.challenges.push(challenge);
    }

    pub(super) fn take_challenge(
        &mut self,
        id: &str,
        now: DateTime<Utc>,
    ) -> Option<ChallengeRecord> {
        let at = self.challenges.iter().position(|c| c.id == id)?;
        let taken = self.challenges.remove(at);
        (taken.expires_at > now).then_some(taken)
    }

    pub(super) fn accept_factor_step(&mut self, user: &UserId, id: &str, step: i64) -> bool {
        match self
            .factors
            .iter_mut()
            .find(|f| &f.user == user && f.id == id)
        {
            Some(f) if f.last_step.is_none_or(|last| step > last) => {
                f.last_step = Some(step);
                true
            }
            _ => false,
        }
    }

    pub(super) fn replace_recovery_codes(&mut self, user: &UserId, hashes: &[String]) {
        self.codes.retain(|c| &c.0 != user);
        self.codes
            .extend(hashes.iter().map(|h| (user.clone(), h.clone(), None)));
    }

    pub(super) fn use_recovery_code(
        &mut self,
        user: &UserId,
        hash: &str,
        now: DateTime<Utc>,
    ) -> bool {
        match self
            .codes
            .iter_mut()
            .find(|c| &c.0 == user && c.1 == hash && c.2.is_none())
        {
            Some(c) => {
                c.2 = Some(now);
                true
            }
            None => false,
        }
    }

    pub(super) fn unused_recovery_codes(&self, user: &UserId) -> usize {
        self.codes
            .iter()
            .filter(|c| &c.0 == user && c.2.is_none())
            .count()
    }

    pub(super) fn count_attempt(
        &mut self,
        user: &UserId,
        now: DateTime<Utc>,
    ) -> Result<(), DateTime<Utc>> {
        let state = self.attempts.get(user).copied().unwrap_or_default();
        let (next, outcome) = count_attempt(state, now);
        self.attempts.insert(user.clone(), next);
        outcome
    }

    pub(super) fn clear_attempts(&mut self, user: &UserId) {
        self.attempts.remove(user);
    }

    pub(super) fn mark_fresh(
        &mut self,
        credential: &str,
        user: &UserId,
        until: DateTime<Utc>,
        now: DateTime<Utc>,
    ) {
        self.fresh.retain(|_, (_, until)| *until > now);
        self.fresh
            .insert(credential.to_string(), (user.clone(), until));
    }

    pub(super) fn fresh_until(&self, credential: &str) -> Option<DateTime<Utc>> {
        self.fresh.get(credential).map(|(_, until)| *until)
    }

    pub(super) fn reseal(
        &mut self,
        reseal: &(dyn for<'a> Fn(&'a FactorRecord) -> Result<NewSecret> + Send + Sync),
    ) -> Result<u64> {
        let sealed = self
            .factors
            .iter()
            .map(reseal)
            .collect::<Result<Vec<_>>>()?;
        let count = sealed.len() as u64;
        for (row, new) in self.factors.iter_mut().zip(sealed) {
            (row.secret, row.encrypted, row.key_id) = (new.secret, new.encrypted, new.key_id);
        }
        Ok(count)
    }
}
