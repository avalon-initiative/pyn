//! Administrator access sessions (decision #1024): an administrator acts as one account for at most an hour.
//! Start, end and each route reached are recorded in the server audit log and in the account's security log.

use super::*;
use crate::access_session::{
    ACCESS_SESSION_MAX_ROUTES, ACCESS_SESSION_NOTES_CHARS, session_length,
};

impl AccessService {
    /// Starts a session as `user` for a listed reason (`other` needs notes). Server administrators only, never on
    /// themselves.
    pub async fn start_access_session(
        &self,
        actor: &Identity,
        user: &UserId,
        reason: &str,
        notes: Option<&str>,
    ) -> Result<(AccessSessionRecord, String)> {
        self.require_server_admin(actor).await?;
        let reason: AccessSessionReason = reason.parse()?;
        let notes = notes.map(str::trim).filter(|n| !n.is_empty());
        if reason == AccessSessionReason::Other && notes.is_none() {
            return Err(PynError::InvalidRequest(
                "the reason \"other\" needs notes".into(),
            ));
        }
        if notes.is_some_and(|n| n.chars().count() > ACCESS_SESSION_NOTES_CHARS) {
            return Err(PynError::InvalidRequest(format!(
                "notes are at most {ACCESS_SESSION_NOTES_CHARS} characters"
            )));
        }
        let admin = self.existing_account(&actor.user).await?;
        if &actor.user == user {
            return Err(PynError::InvalidRequest(
                "an access session acts as another account".into(),
            ));
        }
        let target = self.existing_account(user).await?;
        self.require_active(user).await?;
        let now = self.clock.now();
        let (id, full, secret_hash) = session_credential::generate()?;
        let record = AccessSessionRecord {
            id,
            secret_hash,
            admin_id: admin.id,
            admin: admin.user.clone(),
            account_id: target.id.clone(),
            user: target.user.clone(),
            reason,
            notes: notes.map(str::to_string),
            started_at: now,
            expires_at: now + session_length(),
            ended_at: None,
            end: None,
        };
        self.store.create_access_session(record.clone()).await?;
        let mut detail = format!(
            "{} started access session {} as {}: {reason}",
            record.admin, record.id, record.user
        );
        if let Some(n) = &record.notes {
            detail.push_str(&format!(" ({})", crate::audit::brief(n)));
        }
        let detail = AuditDetail::from(detail).time("expires_at", record.expires_at);
        self.record_session_event(&record, AuditAction::AccessSessionStarted, detail)
            .await?;
        Ok((record, full))
    }

    /// Ends a session now. Any server administrator may; ending one that already stopped changes nothing.
    pub async fn end_access_session(
        &self,
        actor: &Identity,
        id: &TokenId,
    ) -> Result<AccessSessionView> {
        self.require_server_admin(actor).await?;
        self.finish_expired_access_sessions().await?;
        let record = self.session_record(id).await?;
        let now = self.clock.now();
        if self
            .store
            .end_access_session(id, now, AccessSessionEnd::Ended)
            .await?
        {
            let by = if actor.user == record.admin {
                String::new()
            } else {
                format!(" by {}", actor.user)
            };
            self.record_session_end(&record, AccessSessionEnd::Ended, &by)
                .await?;
        }
        let record = self.session_record(id).await?;
        let mut views = self.views(vec![record]).await?;
        Ok(views.remove(0))
    }

    /// Sessions, newest first, optionally only those on `user`. Server administrators only.
    pub async fn list_access_sessions(
        &self,
        actor: &Identity,
        user: Option<&UserId>,
        limit: usize,
    ) -> Result<Vec<AccessSessionView>> {
        self.require_server_admin(actor).await?;
        self.finish_expired_access_sessions().await?;
        let account = match user {
            Some(u) => Some(self.existing_account(u).await?.id),
            None => None,
        };
        self.views(
            self.store
                .list_access_sessions(account.as_ref(), limit)
                .await?,
        )
        .await
    }

    /// The signed-in account's own sessions, newest first. There is nothing to turn off or dismiss: this is the
    /// notification, and it lists every session on the account.
    pub async fn my_access_sessions(
        &self,
        actor: &Identity,
        limit: usize,
    ) -> Result<Vec<AccessSessionView>> {
        actor.require_full_scope()?;
        self.finish_expired_access_sessions().await?;
        let account = self.existing_account(&actor.user).await?;
        self.views(
            self.store
                .list_access_sessions(Some(&account.id), limit)
                .await?,
        )
        .await
    }

    async fn views(&self, records: Vec<AccessSessionRecord>) -> Result<Vec<AccessSessionView>> {
        let mut views = Vec::with_capacity(records.len());
        for record in records {
            let reached = self.store.access_session_reach(&record.id).await?;
            let state = match record.end {
                Some(end) => end.as_str(),
                None if record.is_active(self.clock.now()) => "active",
                None => "expired",
            };
            views.push(AccessSessionView {
                record,
                reached,
                state,
            });
        }
        Ok(views)
    }

    async fn session_record(&self, id: &TokenId) -> Result<AccessSessionRecord> {
        self.store
            .get_access_session(id)
            .await?
            .ok_or_else(|| PynError::AccessSessionNotFound(id.to_string()))
    }

    /// Records the end of every session whose hour ran out, once each, stamped with the time it expired.
    pub async fn finish_expired_access_sessions(&self) -> Result<usize> {
        let expired = self.store.expire_access_sessions(self.clock.now()).await?;
        for record in &expired {
            self.record_session_end(record, AccessSessionEnd::Expired, "")
                .await?;
        }
        Ok(expired.len())
    }

    /// Checks a session credential and names the account it acts as. Refused once it ended or ran out, and when
    /// its administrator is no longer an active administrator or the account is no longer active.
    pub(super) async fn identify_access_session(&self, raw: &str) -> Result<Identity> {
        let (id, secret) = session_credential::parse(raw)
            .ok_or_else(|| unauthenticated("malformed access session"))?;
        let presented = token::hash_secret(secret);
        let record = self.store.get_access_session(&id).await?;
        let stored = record
            .as_ref()
            .map_or(presented.as_str(), |r| &r.secret_hash);
        let matches = token::hashes_match(stored, &presented);
        let record = match record {
            Some(r) if matches => r,
            _ => return Err(unauthenticated("unknown access session")),
        };
        let now = self.clock.now();
        if record.ended_at.is_some() {
            return Err(unauthenticated("access session ended"));
        }
        if record.expires_at <= now {
            self.finish_expired_access_sessions().await?;
            return Err(unauthenticated("access session expired"));
        }
        let admin = self.store.account_by_id(&record.admin_id).await?;
        if !admin.is_some_and(|a| a.is_admin && a.status() == AccountStatus::Active) {
            return Err(unauthenticated(
                "the administrator no longer holds this access",
            ));
        }
        let account = self
            .store
            .account_by_id(&record.account_id)
            .await?
            .ok_or_else(|| unauthenticated("unknown access session"))?;
        self.require_active(&account.user).await?;
        Ok(Identity {
            user: account.user,
            credential: Credential::AccessSession(record),
        })
    }

    /// Counts a request of the session; only the first call to each route (or refusal) is written to the logs.
    pub async fn note_access_session_reach(
        &self,
        who: &Identity,
        method: &str,
        route: &str,
        target: &str,
        refused: bool,
    ) -> Result<()> {
        let Credential::AccessSession(record) = &who.credential else {
            return Ok(());
        };
        let at = self.clock.now();
        let reach = AccessSessionReach {
            method: method.to_string(),
            route: route.to_string(),
            target: target.to_string(),
            refused,
            count: 1,
            first_at: at,
            last_at: at,
        };
        let first = self
            .store
            .note_access_session_reach(&record.id, reach, ACCESS_SESSION_MAX_ROUTES)
            .await?;
        let detail = match first {
            ReachNote::Again => return Ok(()),
            ReachNote::Overflowed => format!(
                "session {}: more than {ACCESS_SESSION_MAX_ROUTES} routes, the rest are counted, not listed",
                record.id
            ),
            ReachNote::First => {
                let outcome = if refused { "refused" } else { "reached" };
                let on = if target.is_empty() {
                    String::new()
                } else {
                    format!(" on {target}")
                };
                format!("session {} {outcome}: {method} {route}{on}", record.id)
            }
        };
        self.record_session_event(record, AuditAction::AccessSessionReached, detail)
            .await
    }

    async fn record_session_end(
        &self,
        record: &AccessSessionRecord,
        end: AccessSessionEnd,
        by: &str,
    ) -> Result<()> {
        let reached = self.store.access_session_reach(&record.id).await?;
        let requests: u64 = reached.iter().map(|r| r.count).sum();
        let detail = format!(
            "access session {} as {} {}{by}: {requests} requests over {} routes",
            record.id,
            record.user,
            match end {
                AccessSessionEnd::Ended => "ended",
                AccessSessionEnd::Expired => "expired",
            },
            reached.len()
        );
        let at = match end {
            AccessSessionEnd::Expired => record.expires_at,
            AccessSessionEnd::Ended => self.clock.now(),
        };
        self.record_session_event_at(record, AuditAction::AccessSessionEnded, detail, at)
            .await
    }

    async fn record_session_event(
        &self,
        record: &AccessSessionRecord,
        action: AuditAction,
        detail: impl Into<AuditDetail>,
    ) -> Result<()> {
        self.record_session_event_at(record, action, detail, self.clock.now())
            .await
    }

    /// Writes the event to the server log and to the security log of the account the session acts as.
    async fn record_session_event_at(
        &self,
        record: &AccessSessionRecord,
        action: AuditAction,
        detail: impl Into<AuditDetail>,
        at: DateTime<Utc>,
    ) -> Result<()> {
        let Some(store) = &self.audit else {
            return Ok(());
        };
        let detail = detail.into();
        for scope in [
            AuditScope::Server,
            AuditScope::Account(record.account_id.clone()),
        ] {
            let event = NewAuditEvent {
                at,
                actor: record.actor(),
                acting_admin: None,
                action,
                path: None,
                detail: detail.text.clone(),
                times: detail.times.clone(),
            };
            store.record(&scope, event).await?;
        }
        Ok(())
    }
}
