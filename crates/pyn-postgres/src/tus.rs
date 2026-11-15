//! Resumable LFS upload sessions.

use chrono::{DateTime, Utc};
use pyn_core::{ObjectId, PynError, RepoId, Result, TusCancel, TusClaim, TusSession, UserId};
use sqlx::postgres::PgRow;
use sqlx::{AssertSqlSafe, Postgres, Row, Transaction};

use crate::{PgMetadataStore, db, object_id};

const COLUMNS: &str =
    "repo, oid, owner, size, received, part_ids, part_sizes, created, expires_at, busy_until";

fn session(row: &PgRow) -> Result<TusSession> {
    let ids: Vec<String> = row.get("part_ids");
    let sizes: Vec<i64> = row.get("part_sizes");
    let created: Vec<String> = row.get("created");
    Ok(TusSession {
        repo: RepoId::new(row.get::<String, _>("repo")),
        oid: row.get("oid"),
        owner: UserId::new(row.get::<String, _>("owner")),
        size: row.get::<i64, _>("size") as u64,
        offset: row.get::<i64, _>("received") as u64,
        parts: ids
            .iter()
            .zip(sizes)
            .map(|(id, n)| Ok((object_id(id, "upload part")?, n as u64)))
            .collect::<Result<_>>()?,
        created: created
            .iter()
            .map(|id| object_id(id, "upload object"))
            .collect::<Result<_>>()?,
        expires: row.get("expires_at"),
        busy_until: row.get("busy_until"),
    })
}

fn hex(ids: &[ObjectId]) -> Vec<String> {
    ids.iter().map(ObjectId::to_hex).collect()
}

impl PgMetadataStore {
    pub(crate) async fn tus_session_row(
        &self,
        repo: &RepoId,
        oid: &str,
    ) -> Result<Option<TusSession>> {
        let row = sqlx::query(AssertSqlSafe(format!(
            "SELECT {COLUMNS} FROM tus_sessions WHERE repo = $1 AND oid = $2"
        )))
        .bind(repo.as_str())
        .bind(oid)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?;
        row.as_ref().map(session).transpose()
    }

    pub(crate) async fn tus_open_row(
        &self,
        new: &TusSession,
        max_open: usize,
    ) -> Result<TusSession> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
            .bind(format!("tus:{}", new.owner.as_str()))
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        if let Some(have) = fetch(&mut tx, &new.repo, &new.oid).await? {
            return Ok(have);
        }
        let open: i64 = sqlx::query_scalar("SELECT count(*) FROM tus_sessions WHERE owner = $1")
            .bind(new.owner.as_str())
            .fetch_one(&mut *tx)
            .await
            .map_err(db)?;
        if open as usize >= max_open {
            return Err(PynError::UploadsBusy {
                scope: "resumable uploads",
            });
        }
        sqlx::query(
            "INSERT INTO tus_sessions (repo, oid, owner, size, expires_at) VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(new.repo.as_str())
        .bind(&new.oid)
        .bind(new.owner.as_str())
        .bind(new.size as i64)
        .bind(new.expires)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(new.clone())
    }

    pub(crate) async fn tus_claim_row(
        &self,
        repo: &RepoId,
        oid: &str,
        owner: &UserId,
        range: std::ops::Range<u64>,
        now: DateTime<Utc>,
        busy_until: DateTime<Utc>,
    ) -> Result<TusClaim> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let Some(have) = fetch(&mut tx, repo, oid).await? else {
            return Ok(TusClaim::Missing);
        };
        if &have.owner != owner || have.expires <= now {
            return Ok(TusClaim::Missing);
        }
        if have.busy_until.is_some_and(|b| b > now) {
            return Ok(TusClaim::Busy);
        }
        let (offset, len) = (range.start, range.end - range.start);
        if offset != have.offset {
            return Ok(TusClaim::Offset {
                expected: have.offset,
            });
        }
        if len > have.size - have.offset {
            return Ok(TusClaim::TooLong);
        }
        sqlx::query("UPDATE tus_sessions SET busy_until = $3 WHERE repo = $1 AND oid = $2")
            .bind(repo.as_str())
            .bind(oid)
            .bind(busy_until)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(TusClaim::Claimed(TusSession {
            busy_until: Some(busy_until),
            ..have
        }))
    }

    pub(crate) async fn tus_commit_row(
        &self,
        repo: &RepoId,
        oid: &str,
        busy_until: DateTime<Utc>,
        part: Option<(ObjectId, u64)>,
        created: &[ObjectId],
        expires: DateTime<Utc>,
    ) -> Result<Option<TusSession>> {
        let (part_ids, part_sizes): (Vec<String>, Vec<i64>) = part
            .map(|(id, n)| (id.to_hex(), n as i64))
            .into_iter()
            .unzip();
        let added: i64 = part_sizes.iter().sum();
        let mut tx = self.pool.begin().await.map_err(db)?;
        let row = sqlx::query(AssertSqlSafe(format!(
            "UPDATE tus_sessions SET received = received + $4, part_ids = part_ids || $5,
                 part_sizes = part_sizes || $6, created = created || $7, expires_at = $8, busy_until = NULL
             WHERE repo = $1 AND oid = $2 AND busy_until = $3 RETURNING {COLUMNS}"
        )))
        .bind(repo.as_str())
        .bind(oid)
        .bind(busy_until)
        .bind(added)
        .bind(part_ids)
        .bind(part_sizes)
        .bind(hex(created))
        .bind(expires)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?;
        let Some(s) = row.as_ref().map(session).transpose()? else {
            return Ok(None);
        };
        if s.offset == s.size {
            sqlx::query("DELETE FROM tus_sessions WHERE repo = $1 AND oid = $2")
                .bind(repo.as_str())
                .bind(oid)
                .execute(&mut *tx)
                .await
                .map_err(db)?;
        }
        tx.commit().await.map_err(db)?;
        Ok(Some(s))
    }

    pub(crate) async fn tus_cancel_row(
        &self,
        repo: &RepoId,
        oid: &str,
        owner: &UserId,
        now: DateTime<Utc>,
    ) -> Result<TusCancel> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let Some(have) = fetch(&mut tx, repo, oid).await? else {
            return Ok(TusCancel::Missing);
        };
        if &have.owner != owner {
            return Ok(TusCancel::Missing);
        }
        if have.busy_until.is_some_and(|b| b > now) {
            return Ok(TusCancel::Busy);
        }
        sqlx::query("DELETE FROM tus_sessions WHERE repo = $1 AND oid = $2")
            .bind(repo.as_str())
            .bind(oid)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(TusCancel::Removed(have))
    }

    pub(crate) async fn tus_take_expired_rows(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Vec<TusSession>> {
        let rows = sqlx::query(AssertSqlSafe(format!(
            "DELETE FROM tus_sessions WHERE expires_at <= $1 AND (busy_until IS NULL OR busy_until <= $1)
             RETURNING {COLUMNS}"
        )))
        .bind(now)
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        rows.iter().map(session).collect()
    }

    pub(crate) async fn tus_held(&self, repo: &RepoId) -> Result<u64> {
        let n: i64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM(received), 0)::bigint FROM tus_sessions WHERE repo = $1",
        )
        .bind(repo.as_str())
        .fetch_one(&self.pool)
        .await
        .map_err(db)?;
        Ok(n as u64)
    }

    pub(crate) async fn tus_held_by_owner(&self, owner: &UserId) -> Result<u64> {
        let n: i64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM(t.received), 0)::bigint FROM tus_sessions t
             JOIN repositories p ON p.id = t.repo WHERE p.owner = $1",
        )
        .bind(owner.as_str())
        .fetch_one(&self.pool)
        .await
        .map_err(db)?;
        Ok(n as u64)
    }
}

async fn fetch(
    tx: &mut Transaction<'_, Postgres>,
    repo: &RepoId,
    oid: &str,
) -> Result<Option<TusSession>> {
    let row = sqlx::query(AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM tus_sessions WHERE repo = $1 AND oid = $2 FOR UPDATE"
    )))
    .bind(repo.as_str())
    .bind(oid)
    .fetch_optional(&mut **tx)
    .await
    .map_err(db)?;
    row.as_ref().map(session).transpose()
}
