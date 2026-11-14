//! Account self-service in PostgreSQL: added email addresses, renames and the name-span ledger.

use chrono::{DateTime, Utc};
use pyn_core::{
    AccountEmail, AccountId, EmailAdd, EmailRemove, EventDraft, PrimaryChange, RenameOutcome,
    Result, UserId,
};
use sqlx::Row;

use crate::{PgMetadataStore, db};

impl PgMetadataStore {
    pub(crate) async fn emails_of(&self, user: &UserId) -> Result<Vec<AccountEmail>> {
        let rows = sqlx::query(
            "SELECT address, verified_at, created_at, primary_slot FROM (
                 SELECT email AS address, email_verified_at AS verified_at, created_at, true AS primary_slot
                   FROM users WHERE id = $1 AND email IS NOT NULL
                 UNION ALL
                 SELECT address, verified_at, created_at, false FROM account_emails WHERE user_id = $1
             ) a ORDER BY primary_slot DESC, created_at, address",
        )
        .bind(user.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        Ok(rows
            .iter()
            .map(|r| AccountEmail {
                address: r.get("address"),
                verified_at: r.get("verified_at"),
                primary: r.get("primary_slot"),
                added_at: r.get("created_at"),
            })
            .collect())
    }

    pub(crate) async fn add_email_row(
        &self,
        user: &UserId,
        address: &str,
        now: DateTime<Utc>,
    ) -> Result<EmailAdd> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(format!("pyn:email:{address}"))
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        let holders: Vec<String> = sqlx::query_scalar(
            "SELECT id FROM users WHERE email = $1
             UNION ALL SELECT user_id FROM account_emails WHERE address = $1",
        )
        .bind(address)
        .fetch_all(&mut *tx)
        .await
        .map_err(db)?;
        if holders.iter().any(|h| h == user.as_str()) {
            return Ok(EmailAdd::Own);
        }
        if !holders.is_empty() {
            return Ok(EmailAdd::Taken);
        }
        sqlx::query(
            "INSERT INTO account_emails (user_id, address, created_at) VALUES ($1, $2, $3)",
        )
        .bind(user.as_str())
        .bind(address)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(EmailAdd::Added)
    }

    pub(crate) async fn confirm_email_row(
        &self,
        user: &UserId,
        address: &str,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let taken: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM users WHERE email = $1 AND email_verified_at IS NOT NULL AND id <> $2)",
        )
        .bind(address)
        .bind(user.as_str())
        .fetch_one(&mut *tx)
        .await
        .map_err(db)?;
        if taken {
            return Ok(false);
        }
        let done = sqlx::query(
            "UPDATE account_emails SET verified_at = $3
              WHERE user_id = $1 AND address = $2 AND verified_at IS NULL",
        )
        .bind(user.as_str())
        .bind(address)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        if done.rows_affected() == 0 {
            return Ok(false);
        }
        let promoted = sqlx::query(
            "UPDATE users SET email = $2, email_verified_at = $3 WHERE id = $1 AND email IS NULL",
        )
        .bind(user.as_str())
        .bind(address)
        .bind(now)
        .execute(&mut *tx)
        .await;
        match promoted {
            Ok(p) if p.rows_affected() > 0 => {
                sqlx::query("DELETE FROM account_emails WHERE user_id = $1 AND address = $2")
                    .bind(user.as_str())
                    .bind(address)
                    .execute(&mut *tx)
                    .await
                    .map_err(db)?;
            }
            Ok(_) => {}
            Err(e)
                if e.as_database_error()
                    .is_some_and(|d| d.is_unique_violation()) =>
            {
                return Ok(false);
            }
            Err(e) => return Err(db(e)),
        }
        tx.commit().await.map_err(db)?;
        Ok(true)
    }

    pub(crate) async fn set_primary_row(
        &self,
        user: &UserId,
        address: &str,
    ) -> Result<PrimaryChange> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let slot = sqlx::query(
            "SELECT email, email_verified_at, created_at FROM users WHERE id = $1 FOR UPDATE",
        )
        .bind(user.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?;
        let Some(slot) = slot else {
            return Ok(PrimaryChange::NotFound);
        };
        let current: Option<String> = slot.get("email");
        let current_verified: Option<DateTime<Utc>> = slot.get("email_verified_at");
        if current.as_deref() == Some(address) {
            return Ok(match current_verified {
                Some(_) => PrimaryChange::Changed,
                None => PrimaryChange::Unverified,
            });
        }
        let row = sqlx::query(
            "DELETE FROM account_emails WHERE user_id = $1 AND address = $2 RETURNING verified_at",
        )
        .bind(user.as_str())
        .bind(address)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?;
        let Some(row) = row else {
            return Ok(PrimaryChange::NotFound);
        };
        let Some(verified): Option<DateTime<Utc>> = row.get("verified_at") else {
            return Ok(PrimaryChange::Unverified);
        };
        if let Some(old) = current {
            sqlx::query(
                "INSERT INTO account_emails (user_id, address, verified_at, created_at) VALUES ($1, $2, $3, $4)",
            )
            .bind(user.as_str())
            .bind(old)
            .bind(current_verified)
            .bind(slot.get::<DateTime<Utc>, _>("created_at"))
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        }
        sqlx::query("UPDATE users SET email = $2, email_verified_at = $3 WHERE id = $1")
            .bind(user.as_str())
            .bind(address)
            .bind(verified)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(PrimaryChange::Changed)
    }

    pub(crate) async fn remove_email_row(
        &self,
        user: &UserId,
        address: &str,
        keep_verified: bool,
    ) -> Result<EmailRemove> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let slot =
            sqlx::query("SELECT email, email_verified_at FROM users WHERE id = $1 FOR UPDATE")
                .bind(user.as_str())
                .fetch_optional(&mut *tx)
                .await
                .map_err(db)?;
        let Some(slot) = slot else {
            return Ok(EmailRemove::NotFound);
        };
        let current: Option<String> = slot.get("email");
        let current_verified: Option<DateTime<Utc>> = slot.get("email_verified_at");
        let primary = current.as_deref() == Some(address);
        let others = sqlx::query(
            "SELECT address, verified_at FROM account_emails WHERE user_id = $1 ORDER BY created_at, address",
        )
        .bind(user.as_str())
        .fetch_all(&mut *tx)
        .await
        .map_err(db)?;
        let row_verified = others
            .iter()
            .find(|r| r.get::<String, _>("address") == address)
            .map(|r| r.get::<Option<DateTime<Utc>>, _>("verified_at").is_some());
        if !primary && row_verified.is_none() {
            return Ok(EmailRemove::NotFound);
        }
        let verified_now = others
            .iter()
            .filter(|r| r.get::<Option<DateTime<Utc>>, _>("verified_at").is_some())
            .count()
            + usize::from(current_verified.is_some());
        let removing_verified = if primary {
            current_verified.is_some()
        } else {
            row_verified == Some(true)
        };
        if keep_verified && removing_verified && verified_now == 1 {
            return Ok(EmailRemove::LastVerified);
        }
        if !primary {
            sqlx::query("DELETE FROM account_emails WHERE user_id = $1 AND address = $2")
                .bind(user.as_str())
                .bind(address)
                .execute(&mut *tx)
                .await
                .map_err(db)?;
            tx.commit().await.map_err(db)?;
            return Ok(EmailRemove::Removed);
        }
        let next = others
            .iter()
            .find(|r| r.get::<Option<DateTime<Utc>>, _>("verified_at").is_some());
        match next {
            Some(r) => {
                let promoted: String = r.get("address");
                let verified: Option<DateTime<Utc>> = r.get("verified_at");
                sqlx::query("DELETE FROM account_emails WHERE user_id = $1 AND address = $2")
                    .bind(user.as_str())
                    .bind(&promoted)
                    .execute(&mut *tx)
                    .await
                    .map_err(db)?;
                sqlx::query("UPDATE users SET email = $2, email_verified_at = $3 WHERE id = $1")
                    .bind(user.as_str())
                    .bind(promoted)
                    .bind(verified)
                    .execute(&mut *tx)
                    .await
                    .map_err(db)?;
            }
            None => {
                sqlx::query(
                    "UPDATE users SET email = NULL, email_verified_at = NULL WHERE id = $1",
                )
                .bind(user.as_str())
                .execute(&mut *tx)
                .await
                .map_err(db)?;
            }
        }
        tx.commit().await.map_err(db)?;
        Ok(EmailRemove::Removed)
    }

    pub(crate) async fn rename_account_row(
        &self,
        user: &UserId,
        new: &UserId,
        at: DateTime<Utc>,
        events: &[EventDraft],
    ) -> Result<RenameOutcome> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let found = sqlx::query(
            "SELECT account_id, created_at FROM users WHERE id = $1 AND kind = 'user' FOR UPDATE",
        )
        .bind(user.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?;
        let Some(found) = found else {
            return Ok(RenameOutcome::NoAccount);
        };
        let account: String = found.get("account_id");
        let created: DateTime<Utc> = found.get("created_at");
        let last: Option<DateTime<Utc>> =
            sqlx::query_scalar("SELECT max(at) FROM account_renames WHERE account_id = $1")
                .bind(&account)
                .fetch_one(&mut *tx)
                .await
                .map_err(db)?;
        // Commit times are whole seconds, so a span opens at the start of its second.
        let held_from = last.map_or(created, |l| l.max(created));
        let held_from = DateTime::from_timestamp(held_from.timestamp(), 0).unwrap_or(held_from);
        let renamed = sqlx::query("UPDATE users SET id = $2 WHERE id = $1")
            .bind(user.as_str())
            .bind(new.as_str())
            .execute(&mut *tx)
            .await;
        match renamed {
            Ok(_) => {}
            Err(e)
                if e.as_database_error()
                    .is_some_and(|d| d.is_unique_violation()) =>
            {
                return Ok(RenameOutcome::NameTaken);
            }
            Err(e) => return Err(db(e)),
        }
        for sql in [
            "UPDATE org_repo_creation_rules SET subject = $2 WHERE kind = 'user' AND subject = $1",
            "UPDATE invites SET used_by = $2 WHERE used_by = $1",
            "UPDATE org_invitations SET invited_by = $2 WHERE invited_by = $1",
            "UPDATE webhook_subscriptions SET created_by = $2 WHERE created_by = $1",
        ] {
            sqlx::query(sql)
                .bind(user.as_str())
                .bind(new.as_str())
                .execute(&mut *tx)
                .await
                .map_err(db)?;
        }
        sqlx::query(
            "INSERT INTO account_renames (account_id, old_name, new_name, held_from, at) VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(&account)
        .bind(user.as_str())
        .bind(new.as_str())
        .bind(held_from)
        .bind(at)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        self.queue_events(&mut tx, events).await?;
        tx.commit().await.map_err(db)?;
        Ok(RenameOutcome::Renamed {
            account: AccountId::new(account),
        })
    }

    pub(crate) async fn current_names_of(
        &self,
        authors: &[(UserId, DateTime<Utc>)],
    ) -> Result<Vec<Option<UserId>>> {
        if authors.is_empty() {
            return Ok(Vec::new());
        }
        let names: Vec<String> = authors.iter().map(|(n, _)| n.to_string()).collect();
        let times: Vec<DateTime<Utc>> = authors.iter().map(|(_, t)| *t).collect();
        let rows = sqlx::query(
            "SELECT a.ord, (SELECT z.new_name FROM account_renames z
                             WHERE z.account_id = s.account_id ORDER BY z.at DESC LIMIT 1) AS current
               FROM unnest($1::text[], $2::timestamptz[]) WITH ORDINALITY AS a(name, at, ord)
               JOIN LATERAL (SELECT r.account_id FROM account_renames r
                              WHERE r.old_name = a.name AND r.held_from <= a.at AND a.at < r.at
                              ORDER BY r.held_from DESC LIMIT 1) s ON true",
        )
        .bind(names)
        .bind(times)
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        let mut out = vec![None; authors.len()];
        for r in rows {
            let ord: i64 = r.get("ord");
            out[(ord - 1) as usize] = r.get::<Option<String>, _>("current").map(UserId::new);
        }
        Ok(out)
    }

    pub(crate) async fn rename_owner_rows(
        &self,
        old: &UserId,
        new: &UserId,
        now: DateTime<Utc>,
    ) -> Result<Vec<String>> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let moved: Vec<(String, String)> = sqlx::query_as(
            "UPDATE repositories SET owner = $2 WHERE owner = $1 RETURNING id, name",
        )
        .bind(old.as_str())
        .bind(new.as_str())
        .fetch_all(&mut *tx)
        .await
        .map_err(db)?;
        for (id, name) in &moved {
            sqlx::query(
                "INSERT INTO repo_redirects (owner, name_lower, repo_id, created_at) VALUES ($1, lower($2), $3, $4)
                 ON CONFLICT (owner, name_lower) DO UPDATE SET repo_id = EXCLUDED.repo_id, created_at = EXCLUDED.created_at",
            )
            .bind(old.as_str())
            .bind(name)
            .bind(id)
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        }
        for sql in [
            "UPDATE locks SET owner = $2 WHERE owner = $1",
            "UPDATE branches SET created_by = $2 WHERE created_by = $1",
        ] {
            sqlx::query(sql)
                .bind(old.as_str())
                .bind(new.as_str())
                .execute(&mut *tx)
                .await
                .map_err(db)?;
        }
        tx.commit().await.map_err(db)?;
        Ok(moved.into_iter().map(|(id, _)| id).collect())
    }
}
