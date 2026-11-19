//! Second factors in PostgreSQL: factors, recovery codes, attempt state and fresh checks.

use chrono::{DateTime, Utc};
use pyn_core::two_factor::{
    AttemptState, ChallengePurpose, ChallengeRecord, FactorKind, FactorRecord, FactorRemoval,
    NoticeRecord, PasskeyRecord, TwoFactorPolicy, count_attempt,
};
use pyn_core::{NewSecret, PynError, Result, UserId};
use sqlx::Row;
use sqlx::postgres::PgRow;

use crate::{PgMetadataStore, db};

const FACTOR: &str =
    "id, user_id, kind, label, secret, encrypted, key_id, created_at, confirmed_at, last_step";

fn factor_from(row: &PgRow) -> Result<FactorRecord> {
    let kind: String = row.get("kind");
    Ok(FactorRecord {
        id: row.get("id"),
        user: UserId::new(row.get::<String, _>("user_id")),
        kind: FactorKind::parse(&kind)
            .ok_or_else(|| PynError::Storage(format!("unknown factor kind {kind}")))?,
        label: row.get("label"),
        secret: row.get("secret"),
        encrypted: row.get("encrypted"),
        key_id: row.get("key_id"),
        created_at: row.get("created_at"),
        confirmed_at: row.get("confirmed_at"),
        last_step: row.get("last_step"),
    })
}

const PASSKEY: &str = "id, user_id, name, credential_id, public_key, user_handle, sign_count, transports, backup_eligible, backup_state, created_at, last_used_at";

fn passkey_from(row: &PgRow) -> PasskeyRecord {
    PasskeyRecord {
        id: row.get("id"),
        user: UserId::new(row.get::<String, _>("user_id")),
        name: row.get("name"),
        credential_id: row.get("credential_id"),
        public_key: row.get("public_key"),
        user_handle: row.get("user_handle"),
        sign_count: row.get::<i64, _>("sign_count") as u32,
        transports: row.get("transports"),
        backup_eligible: row.get("backup_eligible"),
        backup_state: row.get("backup_state"),
        created_at: row.get("created_at"),
        last_used_at: row.get("last_used_at"),
    }
}

/// Whether the account still holds a confirmed factor or a passkey.
async fn second_factor_left(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user: &UserId,
) -> Result<bool> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM account_factors WHERE user_id = $1 AND confirmed_at IS NOT NULL)
             OR EXISTS (SELECT 1 FROM account_passkeys WHERE user_id = $1)",
    )
    .bind(user.as_str())
    .fetch_one(&mut **tx)
    .await
    .map_err(db)
}

fn policy_from(since: DateTime<Utc>, grace_days: i32) -> TwoFactorPolicy {
    TwoFactorPolicy {
        since,
        grace_days: grace_days.max(0) as u32,
    }
}

/// The server's requirement in a `server_settings` row.
pub(crate) fn server_two_factor(row: &PgRow) -> Option<TwoFactorPolicy> {
    let since: Option<DateTime<Utc>> = row.get("two_factor_since");
    let days: Option<i32> = row.get("two_factor_grace_days");
    since.zip(days).map(|(s, d)| policy_from(s, d))
}

impl PgMetadataStore {
    pub(crate) async fn set_server_two_factor_row(
        &self,
        policy: Option<TwoFactorPolicy>,
    ) -> Result<bool> {
        let done = sqlx::query(
            "UPDATE server_settings SET two_factor_since = $1, two_factor_grace_days = $2",
        )
        .bind(policy.map(|p| p.since))
        .bind(policy.map(|p| p.grace_days as i32))
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(done.rows_affected() > 0)
    }

    pub(crate) async fn org_two_factor_row(&self, org: &UserId) -> Result<Option<TwoFactorPolicy>> {
        let row = sqlx::query("SELECT since, grace_days FROM org_two_factor WHERE org = $1")
            .bind(org.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(db)?;
        Ok(row.map(|r| policy_from(r.get("since"), r.get("grace_days"))))
    }

    pub(crate) async fn set_org_two_factor_row(
        &self,
        org: &UserId,
        policy: Option<TwoFactorPolicy>,
    ) -> Result<Option<TwoFactorPolicy>> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let old =
            sqlx::query("SELECT since, grace_days FROM org_two_factor WHERE org = $1 FOR UPDATE")
                .bind(org.as_str())
                .fetch_optional(&mut *tx)
                .await
                .map_err(db)?
                .map(|r| policy_from(r.get("since"), r.get("grace_days")));
        match policy {
            Some(p) => sqlx::query(
                "INSERT INTO org_two_factor (org, since, grace_days) VALUES ($1, $2, $3)
                 ON CONFLICT (org) DO UPDATE SET since = EXCLUDED.since, grace_days = EXCLUDED.grace_days",
            )
            .bind(org.as_str())
            .bind(p.since)
            .bind(p.grace_days as i32)
            .execute(&mut *tx)
            .await
            .map_err(db)?,
            None => sqlx::query("DELETE FROM org_two_factor WHERE org = $1")
                .bind(org.as_str())
                .execute(&mut *tx)
                .await
                .map_err(db)?,
        };
        tx.commit().await.map_err(db)?;
        Ok(old)
    }

    pub(crate) async fn member_two_factor_rows(
        &self,
        user: &UserId,
    ) -> Result<Vec<(UserId, TwoFactorPolicy)>> {
        let rows = sqlx::query(
            "SELECT t.org, t.since, t.grace_days FROM org_two_factor t
             JOIN org_members m ON m.org = t.org WHERE m.user_id = $1 ORDER BY t.org",
        )
        .bind(user.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        Ok(rows
            .iter()
            .map(|r| {
                (
                    UserId::new(r.get::<String, _>("org")),
                    policy_from(r.get("since"), r.get("grace_days")),
                )
            })
            .collect())
    }

    pub(crate) async fn add_notice_row(&self, notice: NoticeRecord) -> Result<()> {
        sqlx::query(
            "INSERT INTO account_notices (id, user_id, kind, detail, created_at) VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(&notice.id)
        .bind(notice.user.as_str())
        .bind(&notice.kind)
        .bind(&notice.detail)
        .bind(notice.created_at)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(())
    }

    pub(crate) async fn notices_rows(&self, user: &UserId) -> Result<Vec<NoticeRecord>> {
        let rows = sqlx::query(
            "SELECT id, kind, detail, created_at FROM account_notices WHERE user_id = $1 ORDER BY created_at, id",
        )
        .bind(user.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        Ok(rows
            .iter()
            .map(|r| NoticeRecord {
                id: r.get("id"),
                user: user.clone(),
                kind: r.get("kind"),
                detail: r.get("detail"),
                created_at: r.get("created_at"),
            })
            .collect())
    }

    pub(crate) async fn dismiss_notice_row(&self, user: &UserId, id: &str) -> Result<bool> {
        let done = sqlx::query("DELETE FROM account_notices WHERE user_id = $1 AND id = $2")
            .bind(user.as_str())
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(done.rows_affected() > 0)
    }

    pub(crate) async fn factors_rows(&self, user: &UserId) -> Result<Vec<FactorRecord>> {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {FACTOR} FROM account_factors WHERE user_id = $1 ORDER BY created_at, id"
        )))
        .bind(user.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        rows.iter().map(factor_from).collect()
    }

    pub(crate) async fn create_factor_row(&self, factor: &FactorRecord) -> Result<()> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        sqlx::query("DELETE FROM account_factors WHERE user_id = $1 AND confirmed_at IS NULL")
            .bind(factor.user.as_str())
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        sqlx::query(
            "INSERT INTO account_factors (id, user_id, kind, label, secret, encrypted, key_id, created_at, confirmed_at, last_step)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(&factor.id)
        .bind(factor.user.as_str())
        .bind(factor.kind.as_str())
        .bind(&factor.label)
        .bind(&factor.secret)
        .bind(factor.encrypted)
        .bind(&factor.key_id)
        .bind(factor.created_at)
        .bind(factor.confirmed_at)
        .bind(factor.last_step)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        tx.commit().await.map_err(db)
    }

    pub(crate) async fn confirm_factor_row(
        &self,
        user: &UserId,
        id: &str,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        let done = sqlx::query(
            "UPDATE account_factors SET confirmed_at = $3
             WHERE user_id = $1 AND id = $2 AND confirmed_at IS NULL",
        )
        .bind(user.as_str())
        .bind(id)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(done.rows_affected() > 0)
    }

    pub(crate) async fn remove_factor_row(&self, user: &UserId, id: &str) -> Result<FactorRemoval> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let done = sqlx::query("DELETE FROM account_factors WHERE user_id = $1 AND id = $2")
            .bind(user.as_str())
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        if done.rows_affected() == 0 {
            return Ok(FactorRemoval::NotFound);
        }
        let left = second_factor_left(&mut tx, user).await?;
        if !left {
            forget(&mut tx, user).await?;
        }
        tx.commit().await.map_err(db)?;
        Ok(FactorRemoval::Removed {
            second_factor_left: left,
        })
    }

    pub(crate) async fn passkeys_rows(&self, user: &UserId) -> Result<Vec<PasskeyRecord>> {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {PASSKEY} FROM account_passkeys WHERE user_id = $1 ORDER BY created_at, id"
        )))
        .bind(user.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        Ok(rows.iter().map(passkey_from).collect())
    }

    pub(crate) async fn passkey_by_credential_row(
        &self,
        credential_id: &[u8],
    ) -> Result<Option<PasskeyRecord>> {
        let row = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {PASSKEY} FROM account_passkeys WHERE credential_id = $1"
        )))
        .bind(credential_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?;
        Ok(row.as_ref().map(passkey_from))
    }

    pub(crate) async fn create_passkey_row(&self, p: &PasskeyRecord) -> Result<bool> {
        let done = sqlx::query(
            "INSERT INTO account_passkeys (id, user_id, name, credential_id, public_key, user_handle, sign_count, transports, backup_eligible, backup_state, created_at, last_used_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
             ON CONFLICT (credential_id) DO NOTHING",
        )
        .bind(&p.id)
        .bind(p.user.as_str())
        .bind(&p.name)
        .bind(&p.credential_id)
        .bind(&p.public_key)
        .bind(&p.user_handle)
        .bind(i64::from(p.sign_count))
        .bind(&p.transports)
        .bind(p.backup_eligible)
        .bind(p.backup_state)
        .bind(p.created_at)
        .bind(p.last_used_at)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(done.rows_affected() > 0)
    }

    pub(crate) async fn rename_passkey_row(
        &self,
        user: &UserId,
        id: &str,
        name: &str,
    ) -> Result<bool> {
        let done =
            sqlx::query("UPDATE account_passkeys SET name = $3 WHERE user_id = $1 AND id = $2")
                .bind(user.as_str())
                .bind(id)
                .bind(name)
                .execute(&self.pool)
                .await
                .map_err(db)?;
        Ok(done.rows_affected() > 0)
    }

    pub(crate) async fn remove_passkey_row(
        &self,
        user: &UserId,
        id: &str,
    ) -> Result<FactorRemoval> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let done = sqlx::query("DELETE FROM account_passkeys WHERE user_id = $1 AND id = $2")
            .bind(user.as_str())
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        if done.rows_affected() == 0 {
            return Ok(FactorRemoval::NotFound);
        }
        let left = second_factor_left(&mut tx, user).await?;
        if !left {
            forget(&mut tx, user).await?;
        }
        tx.commit().await.map_err(db)?;
        Ok(FactorRemoval::Removed {
            second_factor_left: left,
        })
    }

    pub(crate) async fn record_passkey_use_row(
        &self,
        user: &UserId,
        id: &str,
        previous: u32,
        sign_count: u32,
        backup_state: bool,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        let done = sqlx::query(
            "UPDATE account_passkeys SET sign_count = $4, backup_state = $5, last_used_at = $6
             WHERE user_id = $1 AND id = $2 AND sign_count = $3",
        )
        .bind(user.as_str())
        .bind(id)
        .bind(i64::from(previous))
        .bind(i64::from(sign_count))
        .bind(backup_state)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(done.rows_affected() > 0)
    }

    pub(crate) async fn put_challenge_row(
        &self,
        c: &ChallengeRecord,
        now: DateTime<Utc>,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        sqlx::query("DELETE FROM webauthn_challenges WHERE expires_at <= $1")
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        if let Some(user) = &c.user {
            sqlx::query("DELETE FROM webauthn_challenges WHERE user_id = $1 AND purpose = $2")
                .bind(user.as_str())
                .bind(c.purpose.as_str())
                .execute(&mut *tx)
                .await
                .map_err(db)?;
        }
        sqlx::query(
            "INSERT INTO webauthn_challenges (id, user_id, purpose, challenge, expires_at) VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(&c.id)
        .bind(c.user.as_ref().map(UserId::as_str))
        .bind(c.purpose.as_str())
        .bind(&c.challenge)
        .bind(c.expires_at)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        tx.commit().await.map_err(db)
    }

    pub(crate) async fn take_challenge_row(
        &self,
        id: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<ChallengeRecord>> {
        let row = sqlx::query(
            "DELETE FROM webauthn_challenges WHERE id = $1 RETURNING id, user_id, purpose, challenge, expires_at",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?;
        let Some(row) = row else { return Ok(None) };
        let purpose: String = row.get("purpose");
        let expires_at: DateTime<Utc> = row.get("expires_at");
        if expires_at <= now {
            return Ok(None);
        }
        Ok(Some(ChallengeRecord {
            id: row.get("id"),
            user: row.get::<Option<String>, _>("user_id").map(UserId::new),
            purpose: ChallengePurpose::parse(&purpose)
                .ok_or_else(|| PynError::Storage(format!("unknown challenge purpose {purpose}")))?,
            challenge: row.get("challenge"),
            expires_at,
        }))
    }

    pub(crate) async fn accept_step_row(&self, user: &UserId, id: &str, step: i64) -> Result<bool> {
        let done = sqlx::query(
            "UPDATE account_factors SET last_step = $3
             WHERE user_id = $1 AND id = $2 AND (last_step IS NULL OR last_step < $3)",
        )
        .bind(user.as_str())
        .bind(id)
        .bind(step)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(done.rows_affected() > 0)
    }

    pub(crate) async fn replace_codes_rows(
        &self,
        user: &UserId,
        hashes: &[String],
        now: DateTime<Utc>,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        sqlx::query("DELETE FROM recovery_codes WHERE user_id = $1")
            .bind(user.as_str())
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        for hash in hashes {
            sqlx::query(
                "INSERT INTO recovery_codes (user_id, code_hash, created_at) VALUES ($1, $2, $3)",
            )
            .bind(user.as_str())
            .bind(hash)
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        }
        tx.commit().await.map_err(db)
    }

    pub(crate) async fn use_code_row(
        &self,
        user: &UserId,
        hash: &str,
        now: DateTime<Utc>,
    ) -> Result<bool> {
        let done = sqlx::query(
            "UPDATE recovery_codes SET used_at = $3
             WHERE user_id = $1 AND code_hash = $2 AND used_at IS NULL",
        )
        .bind(user.as_str())
        .bind(hash)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(done.rows_affected() > 0)
    }

    pub(crate) async fn unused_codes_count(&self, user: &UserId) -> Result<usize> {
        let n: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM recovery_codes WHERE user_id = $1 AND used_at IS NULL",
        )
        .bind(user.as_str())
        .fetch_one(&self.pool)
        .await
        .map_err(db)?;
        Ok(n as usize)
    }

    pub(crate) async fn count_attempt_row(
        &self,
        user: &UserId,
        now: DateTime<Utc>,
    ) -> Result<std::result::Result<(), DateTime<Utc>>> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        sqlx::query(
            "INSERT INTO factor_attempts (user_id, attempts) VALUES ($1, 0) ON CONFLICT (user_id) DO NOTHING",
        )
        .bind(user.as_str())
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        let row = sqlx::query(
            "SELECT attempts, locked_until, last_at FROM factor_attempts WHERE user_id = $1 FOR UPDATE",
        )
        .bind(user.as_str())
        .fetch_one(&mut *tx)
        .await
        .map_err(db)?;
        let state = AttemptState {
            attempts: row.get::<i32, _>("attempts") as u32,
            locked_until: row.get("locked_until"),
            last_at: row.get("last_at"),
        };
        let (next, outcome) = count_attempt(state, now);
        sqlx::query(
            "UPDATE factor_attempts SET attempts = $2, locked_until = $3, last_at = $4 WHERE user_id = $1",
        )
        .bind(user.as_str())
        .bind(next.attempts as i32)
        .bind(next.locked_until)
        .bind(next.last_at)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(outcome)
    }

    pub(crate) async fn clear_attempt_row(&self, user: &UserId) -> Result<()> {
        sqlx::query("DELETE FROM factor_attempts WHERE user_id = $1")
            .bind(user.as_str())
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(())
    }

    pub(crate) async fn mark_fresh_row(
        &self,
        credential: &str,
        user: &UserId,
        until: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<()> {
        sqlx::query("DELETE FROM fresh_checks WHERE fresh_until <= $1")
            .bind(now)
            .execute(&self.pool)
            .await
            .map_err(db)?;
        sqlx::query(
            "INSERT INTO fresh_checks (credential, user_id, fresh_until) VALUES ($1, $2, $3)
             ON CONFLICT (credential) DO UPDATE SET fresh_until = EXCLUDED.fresh_until",
        )
        .bind(credential)
        .bind(user.as_str())
        .bind(until)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(())
    }

    pub(crate) async fn fresh_until_row(&self, credential: &str) -> Result<Option<DateTime<Utc>>> {
        sqlx::query_scalar("SELECT fresh_until FROM fresh_checks WHERE credential = $1")
            .bind(credential)
            .fetch_optional(&self.pool)
            .await
            .map_err(db)
    }

    pub(crate) async fn clear_two_factor_rows(&self, user: &UserId) -> Result<usize> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let confirmed: i64 = sqlx::query_scalar(
            "SELECT (SELECT count(*) FROM account_factors WHERE user_id = $1 AND confirmed_at IS NOT NULL)
                  + (SELECT count(*) FROM account_passkeys WHERE user_id = $1)",
        )
        .bind(user.as_str())
        .fetch_one(&mut *tx)
        .await
        .map_err(db)?;
        forget(&mut tx, user).await?;
        tx.commit().await.map_err(db)?;
        Ok(confirmed as usize)
    }

    pub(crate) async fn reseal_seed_rows(
        &self,
        reseal: &(dyn for<'a> Fn(&'a FactorRecord) -> Result<NewSecret> + Send + Sync),
    ) -> Result<u64> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        sqlx::query("LOCK TABLE account_factors IN EXCLUSIVE MODE")
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {FACTOR} FROM account_factors ORDER BY id"
        )))
        .fetch_all(&mut *tx)
        .await
        .map_err(db)?;
        for row in &rows {
            let current = factor_from(row)?;
            let new = reseal(&current)?;
            sqlx::query(
                "UPDATE account_factors SET secret = $2, encrypted = $3, key_id = $4 WHERE id = $1",
            )
            .bind(&current.id)
            .bind(&new.secret)
            .bind(new.encrypted)
            .bind(&new.key_id)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        }
        tx.commit().await.map_err(db)?;
        Ok(rows.len() as u64)
    }
}

async fn forget(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, user: &UserId) -> Result<()> {
    for sql in [
        "DELETE FROM account_factors WHERE user_id = $1",
        "DELETE FROM account_passkeys WHERE user_id = $1",
        "DELETE FROM webauthn_challenges WHERE user_id = $1",
        "DELETE FROM recovery_codes WHERE user_id = $1",
        "DELETE FROM factor_attempts WHERE user_id = $1",
        "DELETE FROM fresh_checks WHERE user_id = $1",
    ] {
        sqlx::query(sql)
            .bind(user.as_str())
            .execute(&mut **tx)
            .await
            .map_err(db)?;
    }
    Ok(())
}
